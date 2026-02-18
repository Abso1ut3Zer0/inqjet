//! Consumer-side archiver loop.
//!
//! Single background thread polls the log stream and any archive streams
//! for records, reads headers, and calls format functions to write output.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_utils::sync::Parker;
use nexus_logbuf::queue::mpsc;

use crate::archive::ArchiveStream;
use crate::record::{self, FormatContext};

/// A logbuf consumer paired with its output writer.
pub(crate) struct Stream {
    pub consumer: mpsc::Consumer,
    pub writer: Box<dyn std::io::Write + Send>,
}

/// Runs the archiver loop, polling the log stream and archive streams.
///
/// Reads records from each stream's ring buffer and calls the appropriate
/// format function. Parks with timeout when idle. Drains all remaining
/// records on shutdown.
pub(crate) fn archiver_loop(
    stream: &mut Stream,
    archives: &mut [ArchiveStream],
    running: &AtomicBool,
    timeout: Option<Duration>,
    parker: Parker,
) {
    // Reusable format buffer — grows to max line size once, never deallocated.
    // Batches the many small write!() calls from format functions into a
    // single write_all() to the real writer per record.
    let mut buf = Vec::with_capacity(256);

    while running.load(Ordering::Relaxed) {
        let mut had_work = false;

        // Poll log stream
        while let Some(claim) = stream.consumer.try_claim() {
            let header = record::read_header(&claim);
            buf.clear();
            (header.formatter)(FormatContext {
                timestamp_ns: header.timestamp_ns,
                level: header.level,
                payload: &claim[record::HEADER_SIZE..],
                out: &mut buf,
            });
            let _ = stream.writer.write_all(&buf);
            had_work = true;
            // ReadClaim dropped here -> region zeroed, head advanced
        }

        // Poll archive streams
        for archive in archives.iter_mut() {
            while let Some(claim) = archive.consumer.try_claim() {
                buf.clear();
                (archive.format_fn)(&claim, &mut buf);
                let _ = archive.writer.write_all(&buf);
                had_work = true;
            }
        }

        if had_work {
            let _ = stream.writer.flush();
            for archive in archives.iter_mut() {
                let _ = archive.writer.flush();
            }
        } else {
            match timeout {
                Some(dur) => parker.park_timeout(dur),
                None => std::hint::spin_loop(),
            }
        }
    }

    // Drain remaining records on shutdown.
    //
    // Race: records mid-claim (between try_claim success and commit) when
    // `running` went false will not be visible here. The window is
    // nanoseconds. Accepted trade-off — adding in-flight tracking would
    // add atomic ops to every producer claim on the hot path.
    while let Some(claim) = stream.consumer.try_claim() {
        let header = record::read_header(&claim);
        buf.clear();
        (header.formatter)(FormatContext {
            timestamp_ns: header.timestamp_ns,
            level: header.level,
            payload: &claim[record::HEADER_SIZE..],
            out: &mut buf,
        });
        let _ = stream.writer.write_all(&buf);
    }
    let _ = stream.writer.flush();

    for archive in archives.iter_mut() {
        while let Some(claim) = archive.consumer.try_claim() {
            buf.clear();
            (archive.format_fn)(&claim, &mut buf);
            let _ = archive.writer.write_all(&buf);
        }
        let _ = archive.writer.flush();
    }
}
