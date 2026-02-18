//! Record format for log entries in the nexus-logbuf ring buffer.
//!
//! Every record written to the ring buffer starts with a fixed 24-byte
//! RecordHeader followed by variable-length payload bytes. The header
//! carries a function pointer that the consumer calls to format the
//! payload into the output writer.

use std::io;

pub(crate) const HEADER_SIZE: usize = std::mem::size_of::<RecordHeader>();
const _: () = assert!(HEADER_SIZE == 24);

/// Called by the consumer to format a record's payload into output.
pub(crate) type FormatFn = fn(u64, u8, &[u8], &mut dyn io::Write);

/// Fixed 24-byte header prepended to every record in the ring buffer.
///
/// All fields are naturally 8-byte aligned, matching nexus-logbuf's
/// 8-byte record alignment. No internal padding.
///
/// # Layout
/// ```text
/// ┌────────────────┬────────────────┬───────┬─────────┐
/// │  timestamp_ns  │  formatter_ptr │ level │ padding │
/// │   (u64, 8B)    │   (usize, 8B)  │ (u8)  │  (7B)   │
/// └────────────────┴────────────────┴───────┴─────────┘
/// ```
#[repr(C)]
pub(crate) struct RecordHeader {
    pub timestamp_ns: u64,
    pub formatter: FormatFn,
    pub level: u8,
    pub _pad: [u8; 7],
}

/// Writes a RecordHeader into the first HEADER_SIZE bytes of `buf`.
///
/// # Safety
///
/// Uses `copy_nonoverlapping` — both sides are valid for HEADER_SIZE bytes.
/// Source is a repr(C) struct in the same binary. No alignment requirement
/// on the destination (byte-level copy).
pub(crate) fn write_header(buf: &mut [u8], header: &RecordHeader) {
    assert!(buf.len() >= HEADER_SIZE);
    // SAFETY: RecordHeader is repr(C), copied as raw bytes into a buffer
    // we own (the logbuf WriteClaim). Both source and destination are valid
    // for HEADER_SIZE bytes. Destination has no alignment requirement
    // (byte-level copy via copy_nonoverlapping).
    unsafe {
        std::ptr::copy_nonoverlapping(
            header as *const RecordHeader as *const u8,
            buf.as_mut_ptr(),
            HEADER_SIZE,
        );
    }
}

/// Reads a RecordHeader from the first HEADER_SIZE bytes of `buf`.
///
/// # Safety
///
/// Uses `read_unaligned` defensively. RecordHeader's 8-byte alignment
/// matches nexus-logbuf's record alignment, but `read_unaligned` is
/// zero-cost on x86-64 and removes any need to reason about alignment.
pub(crate) fn read_header(buf: &[u8]) -> RecordHeader {
    assert!(buf.len() >= HEADER_SIZE);
    // SAFETY: Buffer was written by write_header in the same binary, so
    // layout matches. read_unaligned is used defensively (zero-cost on
    // x86-64, same as aligned read).
    unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const RecordHeader) }
}

// Standard payload layout (Phases 1-3):
//
// bytes 0..4:             line number (u32 LE, 0 = not available)
// bytes 4..6:             target length (u16 LE)
// bytes 6..6+target_len:  target string (UTF-8, from module_path!())
// bytes 6+target_len..:   message string (UTF-8, from format_args!())

/// Calculates total record size for a standard payload.
#[cfg(any(feature = "log-compat", test))]
pub(crate) fn standard_record_size(target_len: usize, message_len: usize) -> usize {
    HEADER_SIZE + 4 + 2 + target_len + message_len
}

/// Writes the standard payload into `buf`. Returns bytes written.
#[cfg(any(feature = "log-compat", test))]
pub(crate) fn write_standard_payload(
    buf: &mut [u8],
    line: u32,
    target: &str,
    message: &str,
) -> usize {
    let tb = target.as_bytes();
    let mb = message.as_bytes();
    assert!(
        tb.len() <= u16::MAX as usize,
        "target string exceeds u16::MAX bytes ({})",
        tb.len()
    );
    let tl = tb.len() as u16;

    buf[0..4].copy_from_slice(&line.to_le_bytes());
    buf[4..6].copy_from_slice(&tl.to_le_bytes());
    buf[6..6 + tb.len()].copy_from_slice(tb);
    let msg_start = 6 + tb.len();
    buf[msg_start..msg_start + mb.len()].copy_from_slice(mb);

    msg_start + mb.len()
}

/// Reads the standard payload. Returns (line, target, message).
#[cfg(any(feature = "log-compat", test))]
pub(crate) fn read_standard_payload(payload: &[u8]) -> (u32, &str, &str) {
    let line = u32::from_le_bytes(
        payload[0..4]
            .try_into()
            .expect("payload too short for line field"),
    );
    let tl = u16::from_le_bytes(
        payload[4..6]
            .try_into()
            .expect("payload too short for target_len field"),
    ) as usize;
    let target = std::str::from_utf8(&payload[6..6 + tl]).expect("invalid UTF-8 in target field");
    let message = std::str::from_utf8(&payload[6 + tl..]).expect("invalid UTF-8 in message field");
    (line, target, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_format_fn(_: u64, _: u8, _: &[u8], _: &mut dyn io::Write) {}

    #[test]
    fn header_size_is_24() {
        assert_eq!(HEADER_SIZE, 24);
    }

    #[test]
    fn header_round_trip() {
        let header = RecordHeader {
            timestamp_ns: 1_700_000_000_000_000_000,
            formatter: dummy_format_fn,
            level: 3,
            _pad: [0; 7],
        };
        let mut buf = [0u8; HEADER_SIZE];
        write_header(&mut buf, &header);
        let got = read_header(&buf);

        assert_eq!(got.timestamp_ns, header.timestamp_ns);
        assert_eq!(got.level, header.level);
        assert_eq!(got.formatter as usize, header.formatter as usize);
    }

    #[test]
    fn payload_round_trip() {
        let mut buf = [0u8; 128];
        let n = write_standard_payload(&mut buf, 42, "my_app::db", "connection opened");
        let (line, target, message) = read_standard_payload(&buf[..n]);

        assert_eq!(line, 42);
        assert_eq!(target, "my_app::db");
        assert_eq!(message, "connection opened");
    }

    #[test]
    fn payload_zero_line() {
        let mut buf = [0u8; 128];
        let n = write_standard_payload(&mut buf, 0, "app", "msg");
        let (line, target, message) = read_standard_payload(&buf[..n]);

        assert_eq!(line, 0);
        assert_eq!(target, "app");
        assert_eq!(message, "msg");
    }

    #[test]
    fn standard_record_size_correct() {
        let target = "my_app::handler";
        let message = "request processed";
        let size = standard_record_size(target.len(), message.len());
        assert_eq!(size, HEADER_SIZE + 4 + 2 + target.len() + message.len());
    }
}
