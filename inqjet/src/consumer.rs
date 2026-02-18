//! Consumer-side archiver loop.
//!
//! Single background thread polls the logbuf for records, reads the
//! RecordHeader, and calls the format function to write output.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_utils::sync::Parker;
use nexus_logbuf::queue::mpsc;

use crate::record;

/// A logbuf consumer paired with its output writer.
pub(crate) struct Stream {
    pub consumer: mpsc::Consumer,
    pub writer: Box<dyn std::io::Write + Send>,
}

/// Runs the archiver loop, polling the stream for records.
///
/// Reads records from the logbuf, calls the format function from each
/// record header to write formatted output to the stream's writer.
/// Parks with timeout when idle. Drains all remaining records on shutdown.
pub(crate) fn archiver_loop(
    stream: &mut Stream,
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

        while let Some(claim) = stream.consumer.try_claim() {
            let header = record::read_header(&claim);
            let payload = &claim[record::HEADER_SIZE..];
            buf.clear();
            (header.formatter)(header.timestamp_ns, header.level, payload, &mut buf);
            let _ = stream.writer.write_all(&buf);
            had_work = true;
            // ReadClaim dropped here -> region zeroed, head advanced
        }

        if had_work {
            let _ = stream.writer.flush();
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
        let payload = &claim[record::HEADER_SIZE..];
        buf.clear();
        (header.formatter)(header.timestamp_ns, header.level, payload, &mut buf);
        let _ = stream.writer.write_all(&buf);
    }
    let _ = stream.writer.flush();
}
