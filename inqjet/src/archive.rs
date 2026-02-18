//! Archive streams for structured event logging.
//!
//! Each archive stream has its own ring buffer, separate from the main
//! log stream. Records carry a user-defined tag (memcpy'd as raw bytes)
//! and an opaque payload. Formatting happens on the archiver thread.

use std::io;
use std::marker::PhantomData;

use nexus_logbuf::queue::mpsc;

/// Marker trait for archive record tags.
///
/// Tags identify the type of archive record. The tag struct is memcpy'd
/// into the ring buffer on the producer side and reconstructed on the
/// consumer side, where `write_label` formats a human-readable label.
///
/// # Contract
///
/// - Must be `Copy` (memcpy'd as raw bytes through ring buffer)
/// - Must be `'static`
/// - `write_label` is called on the background archiver thread (not hot path)
///
/// # Example
///
/// ```rust,ignore
/// #[derive(Clone, Copy)]
/// enum Direction { Inbound, Outbound }
///
/// impl Direction {
///     const fn as_str(self) -> &'static str {
///         match self {
///             Direction::Inbound => "INBOUND",
///             Direction::Outbound => "OUTBOUND",
///         }
///     }
/// }
///
/// #[derive(Clone, Copy)]
/// struct VenueEvent {
///     venue: &'static str,
///     direction: Direction,
/// }
///
/// impl ArchiveTag for VenueEvent {
///     fn write_label(&self, out: &mut dyn std::io::Write) {
///         write!(out, "{} {}", self.venue, self.direction.as_str()).unwrap();
///     }
/// }
/// ```
pub trait ArchiveTag: Copy + 'static {
    /// Write a human-readable label for this tag.
    ///
    /// Called on the consumer/archiver thread — not on the hot path.
    /// Allocations and formatting are acceptable here.
    fn write_label(&self, out: &mut dyn io::Write);
}

/// Handle for writing archive records to a dedicated ring buffer.
///
/// Each handle is backed by its own ring buffer, separate from the main
/// log stream. Clone the handle for use across multiple threads — each
/// clone gets its own producer endpoint.
///
/// # Example
///
/// ```rust,ignore
/// let mut handle = builder.add_archive::<VenueEvent>(file, 65536);
///
/// // On the hot path:
/// handle.write(
///     &VenueEvent { venue: "BINANCE", direction: Direction::Inbound },
///     b"8=FIX.4.4|35=D|...",
/// );
/// ```
pub struct ArchiveHandle<T: ArchiveTag> {
    producer: mpsc::Producer,
    _tag: PhantomData<T>,
}

impl<T: ArchiveTag> Clone for ArchiveHandle<T> {
    fn clone(&self) -> Self {
        Self {
            producer: self.producer.clone(),
            _tag: PhantomData,
        }
    }
}

impl<T: ArchiveTag> ArchiveHandle<T> {
    pub(crate) fn new(producer: mpsc::Producer) -> Self {
        Self {
            producer,
            _tag: PhantomData,
        }
    }

    /// Write an archive record.
    ///
    /// The tag is memcpy'd as raw bytes into the ring buffer; payload
    /// is copied verbatim. If the ring buffer is full, the record is
    /// silently dropped.
    pub fn write(&mut self, tag: T, payload: &[u8]) {
        let tag_size = std::mem::size_of::<T>();
        let record_size = 8 + tag_size + 4 + payload.len();

        let Ok(mut claim) = self.producer.try_claim(record_size) else {
            return; // Buffer full — drop silently
        };

        let buf = &mut *claim;
        let mut off = 0;

        // Timestamp
        let ts = crate::format::snap_timestamp();
        buf[off..off + 8].copy_from_slice(&ts.to_le_bytes());
        off += 8;

        // Tag (raw bytes)
        // SAFETY: T: Copy + 'static. Source is a valid T, destination is
        // a ring buffer claim we own. Both valid for tag_size bytes.
        // Same-binary constraint ensures layout consistency.
        if tag_size > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    &tag as *const T as *const u8,
                    buf[off..].as_mut_ptr(),
                    tag_size,
                );
            }
        }
        off += tag_size;

        // Payload length + payload
        buf[off..off + 4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        off += 4;
        buf[off..off + payload.len()].copy_from_slice(payload);

        claim.commit();
    }
}

/// Type-erased archive stream for the consumer/archiver thread.
pub(crate) struct ArchiveStream {
    pub consumer: mpsc::Consumer,
    pub writer: Box<dyn io::Write + Send>,
    pub format_fn: fn(&[u8], &mut dyn io::Write),
}

/// Monomorphized format function for a specific [`ArchiveTag`] type.
///
/// Registered per-stream at build time. Reads timestamp + tag + payload
/// from the record bytes, formats the tag label, and writes a line to
/// the output.
///
/// Output format:
/// ```text
/// 2024-01-15T14:30:45.123456789Z [BINANCE INBOUND] payload bytes here
/// ```
pub(crate) fn archive_format_fn<T: ArchiveTag>(record: &[u8], out: &mut dyn io::Write) {
    let tag_size = std::mem::size_of::<T>();
    let mut off = 0;

    // Timestamp
    let ts = u64::from_le_bytes(
        record[off..off + 8]
            .try_into()
            .expect("archive record too short for timestamp"),
    );
    off += 8;

    // Tag
    // SAFETY: Bytes were written from a valid T via copy_nonoverlapping
    // in the same binary. Layout matches. read_unaligned handles alignment.
    // T: Copy, so no drop concerns.
    let tag: T = if tag_size > 0 {
        unsafe { std::ptr::read_unaligned(record[off..].as_ptr() as *const T) }
    } else {
        // ZST — produce the only possible value.
        // SAFETY: A zero-sized Copy type has exactly one valid value.
        // No bytes are read or written.
        unsafe { std::mem::zeroed() }
    };
    off += tag_size;

    // Payload
    let plen = u32::from_le_bytes(
        record[off..off + 4]
            .try_into()
            .expect("archive record too short for payload length"),
    ) as usize;
    off += 4;
    let payload = &record[off..off + plen];

    // Format output
    crate::format::format_timestamp(ts, out);
    let _ = write!(out, " [");
    tag.write_label(out);
    let _ = write!(out, "] ");
    let _ = out.write_all(payload);
    let _ = out.write_all(b"\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct SimpleTag(&'static str);

    impl ArchiveTag for SimpleTag {
        fn write_label(&self, out: &mut dyn io::Write) {
            let _ = out.write_all(self.0.as_bytes());
        }
    }

    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    enum Direction {
        Inbound,
        Outbound,
    }

    impl Direction {
        const fn as_str(self) -> &'static str {
            match self {
                Direction::Inbound => "INBOUND",
                Direction::Outbound => "OUTBOUND",
            }
        }
    }

    #[derive(Clone, Copy)]
    struct VenueEvent {
        venue: &'static str,
        direction: Direction,
    }

    impl ArchiveTag for VenueEvent {
        fn write_label(&self, out: &mut dyn io::Write) {
            let _ = write!(out, "{} {}", self.venue, self.direction.as_str());
        }
    }

    #[test]
    fn archive_format_simple_tag() {
        let tag = SimpleTag("HEARTBEAT");
        let payload = b"ping";

        // Build a record manually
        let tag_size = std::mem::size_of::<SimpleTag>();
        let record_size = 8 + tag_size + 4 + payload.len();
        let mut record = vec![0u8; record_size];

        let ts: u64 = 1_705_329_045_123_456_789;
        let mut off = 0;
        record[off..off + 8].copy_from_slice(&ts.to_le_bytes());
        off += 8;
        unsafe {
            std::ptr::copy_nonoverlapping(
                &tag as *const SimpleTag as *const u8,
                record[off..].as_mut_ptr(),
                tag_size,
            );
        }
        off += tag_size;
        record[off..off + 4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        off += 4;
        record[off..off + payload.len()].copy_from_slice(payload);

        let mut out = Vec::new();
        archive_format_fn::<SimpleTag>(&record, &mut out);
        let s = String::from_utf8(out).unwrap();

        assert!(s.contains("2024-01-15T14:30:45.123456789Z"));
        assert!(s.contains("[HEARTBEAT]"));
        assert!(s.contains("ping"));
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn archive_format_composite_tag() {
        let tag = VenueEvent {
            venue: "BINANCE",
            direction: Direction::Inbound,
        };
        let payload = b"8=FIX.4.4|35=D";

        let tag_size = std::mem::size_of::<VenueEvent>();
        let record_size = 8 + tag_size + 4 + payload.len();
        let mut record = vec![0u8; record_size];

        let ts: u64 = 1_705_329_045_123_456_789;
        let mut off = 0;
        record[off..off + 8].copy_from_slice(&ts.to_le_bytes());
        off += 8;
        unsafe {
            std::ptr::copy_nonoverlapping(
                &tag as *const VenueEvent as *const u8,
                record[off..].as_mut_ptr(),
                tag_size,
            );
        }
        off += tag_size;
        record[off..off + 4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        off += 4;
        record[off..off + payload.len()].copy_from_slice(payload);

        let mut out = Vec::new();
        archive_format_fn::<VenueEvent>(&record, &mut out);
        let s = String::from_utf8(out).unwrap();

        assert!(s.contains("[BINANCE INBOUND]"));
        assert!(s.contains("8=FIX.4.4|35=D"));
    }
}
