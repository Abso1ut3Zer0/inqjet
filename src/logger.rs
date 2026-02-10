//! Producer-side logging and global state.
//!
//! # Thread-Local Producer Pattern
//!
//! nexus-logbuf's `Producer::try_claim(&mut self)` requires mutable access.
//! Producer is `Clone + Send` but NOT `Sync`. For `log::Log::log(&self)`
//! (which requires `Sync`), we use thread-local producer clones.
//!
//! Each thread lazily clones a Producer from the global source on first
//! log call. Clone is cheap (Arc clone + fresh cached_head). After that,
//! pure thread-local — zero contention on the producer path.
//!
//! # Re-Entrancy Invariant
//!
//! `TLS_PRODUCER` uses `RefCell` which serves double duty: safe mutable
//! access AND re-entrancy detection. If a `Display`/`Debug` impl calls
//! a log macro during formatting, `try_borrow_mut()` panics with a clear
//! message rather than causing UB.
//!
//! `TLS_BUF` uses `UnsafeCell` because re-entrancy is already guarded by
//! `TLS_PRODUCER`'s `RefCell` — the borrow on `TLS_PRODUCER` is held for
//! the entire log path, so any re-entrant call hits the RefCell guard
//! before reaching the buffer.

use std::cell::{RefCell, UnsafeCell};
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crossbeam_utils::sync::Unparker;
use log::LevelFilter;
use nexus_logbuf::queue::mpsc;

use crate::format;
use crate::record::{self, RecordHeader, HEADER_SIZE};

pub(crate) struct LoggerState {
    /// Source producer — threads clone from this.
    pub source_producer: mpsc::Producer,

    /// Unparker for waking the archiver thread.
    pub unparker: Unparker,

    /// Global level filter.
    pub max_level: LevelFilter,

    /// Shutdown flag. Arc-shared with guard and archiver.
    pub running: Arc<AtomicBool>,
}

pub(crate) static LOGGER: OnceLock<LoggerState> = OnceLock::new();

thread_local! {
    /// Thread-local producer clone. RefCell provides re-entrancy detection:
    /// if a Display/Debug impl calls log!() during formatting, try_borrow_mut()
    /// panics before any UB can occur.
    ///
    /// # Invariant
    ///
    /// While this RefCell is mutably borrowed, no other code in the log path
    /// may attempt to borrow it. This borrow guards the entire producer path
    /// including TLS_BUF access below.
    static TLS_PRODUCER: RefCell<Option<mpsc::Producer>> = const { RefCell::new(None) };

    /// Thread-local formatting buffer. UnsafeCell because re-entrancy is
    /// guarded by TLS_PRODUCER's RefCell above.
    ///
    /// # Safety
    ///
    /// Only accessed while TLS_PRODUCER is mutably borrowed. The RefCell
    /// borrow on TLS_PRODUCER prevents re-entrant access to this buffer.
    static TLS_BUF: UnsafeCell<String> = const { UnsafeCell::new(String::new()) };
}

fn level_to_u8(level: log::Level) -> u8 {
    match level {
        log::Level::Error => 1,
        log::Level::Warn => 2,
        log::Level::Info => 3,
        log::Level::Debug => 4,
        log::Level::Trace => 5,
    }
}

/// Logger that implements `log::Log` by writing records to the logbuf.
pub(crate) struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        LOGGER
            .get()
            .map(|s| metadata.level() <= s.max_level)
            .unwrap_or(false)
    }

    fn log(&self, record: &log::Record) {
        let state = match LOGGER.get() {
            Some(s) => s,
            None => return,
        };

        if record.level() > state.max_level {
            return;
        }

        if !state.running.load(Ordering::Relaxed) {
            return;
        }

        let timestamp_ns = format::snap_timestamp();
        let level = level_to_u8(record.level());
        let target = record.target();
        let line = record.line().unwrap_or(0);

        TLS_PRODUCER.with(|producer_cell| {
            let mut producer_opt = producer_cell.try_borrow_mut().expect(
                "re-entrant logging detected: Display/Debug impls must not call log macros",
            );

            let producer = producer_opt.get_or_insert_with(|| state.source_producer.clone());

            TLS_BUF.with(|buf_cell| {
                // SAFETY: TLS is single-threaded. Re-entrancy is guarded by
                // TLS_PRODUCER's RefCell borrow above — a re-entrant log call
                // would panic at try_borrow_mut() before reaching here.
                let buf = unsafe { &mut *buf_cell.get() };
                buf.clear();

                write!(buf, "{}", record.args()).ok();

                let total_size = record::standard_record_size(target.len(), buf.len());

                let header = RecordHeader {
                    timestamp_ns,
                    formatter: format::standard_format_fn,
                    level,
                    _pad: [0u8; 7],
                };

                loop {
                    match producer.try_claim(total_size) {
                        Ok(mut claim) => {
                            record::write_header(&mut claim, &header);
                            record::write_standard_payload(
                                &mut claim[HEADER_SIZE..],
                                line,
                                target,
                                buf.as_str(),
                            );
                            claim.commit();
                            state.unparker.unpark();
                            break;
                        }
                        Err(nexus_logbuf::TryClaimError::Full) => {
                            state.unparker.unpark();
                            std::hint::spin_loop();
                        }
                        Err(nexus_logbuf::TryClaimError::ZeroLength) => {
                            unreachable!("record size is always > 0");
                        }
                    }
                }
            });
        });
    }

    fn flush(&self) {}
}
