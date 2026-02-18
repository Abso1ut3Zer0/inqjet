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

use std::cell::RefCell;
#[cfg(feature = "log-compat")]
use std::cell::UnsafeCell;
#[cfg(feature = "log-compat")]
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use crossbeam_utils::Backoff;
use crossbeam_utils::sync::Unparker;
use nexus_logbuf::queue::mpsc;

#[cfg(feature = "log-compat")]
use crate::format;
use crate::record::{self, HEADER_SIZE, RecordHeader};

pub(crate) struct LoggerState {
    /// Source producer — threads clone from this.
    pub source_producer: mpsc::Producer,

    /// Unparker for waking the archiver thread.
    pub unparker: Unparker,

    /// Global level filter (used by the `log` crate bridge).
    #[cfg(feature = "log-compat")]
    pub max_level: crate::LevelFilter,

    /// Backpressure strategy when the ring buffer is full.
    pub backpressure: crate::BackpressureMode,

    /// Shutdown flag. Arc-shared with guard and archiver.
    pub running: Arc<AtomicBool>,
}

pub(crate) static LOGGER: OnceLock<LoggerState> = OnceLock::new();

/// Fast level gate for standalone macros. One atomic load before
/// `format_args!()` evaluates its arguments.
pub(crate) static MAX_LEVEL: AtomicU8 = AtomicU8::new(0); // 0 = Off

pub(crate) fn log_enabled(level: u8) -> bool {
    level <= MAX_LEVEL.load(Ordering::Relaxed)
}

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

    /// Thread-local formatting buffer for the `log` crate bridge path.
    /// UnsafeCell because re-entrancy is guarded by TLS_PRODUCER's RefCell above.
    ///
    /// # Safety
    ///
    /// Only accessed while TLS_PRODUCER is mutably borrowed. The RefCell
    /// borrow on TLS_PRODUCER prevents re-entrant access to this buffer.
    #[cfg(feature = "log-compat")]
    static TLS_BUF: UnsafeCell<String> = const { UnsafeCell::new(String::new()) };
}

#[cfg(feature = "log-compat")]
fn level_to_u8(level: log::Level) -> u8 {
    match level {
        log::Level::Error => 1,
        log::Level::Warn => 2,
        log::Level::Info => 3,
        log::Level::Debug => 4,
        log::Level::Trace => 5,
    }
}

/// Core log path for the `log` crate bridge (`Logger::log()`).
///
/// Level gating is the caller's responsibility — `Logger::log()` checks
/// against `state.max_level`.
#[cfg(feature = "log-compat")]
pub(crate) fn log_impl(level: u8, target: &str, line: u32, args: std::fmt::Arguments<'_>) {
    let state = match LOGGER.get() {
        Some(s) => s,
        None => return,
    };

    if !state.running.load(Ordering::Relaxed) {
        return;
    }

    let timestamp_ns = format::snap_timestamp();

    TLS_PRODUCER.with(|producer_cell| {
        let mut producer_opt = producer_cell
            .try_borrow_mut()
            .expect("re-entrant logging detected: Display/Debug impls must not call log macros");

        let producer = producer_opt.get_or_insert_with(|| state.source_producer.clone());

        TLS_BUF.with(|buf_cell| {
            // SAFETY: TLS is single-threaded. Re-entrancy is guarded by
            // TLS_PRODUCER's RefCell borrow above — a re-entrant log call
            // would panic at try_borrow_mut() before reaching here.
            let buf = unsafe { &mut *buf_cell.get() };
            buf.clear();

            write!(buf, "{}", args).ok();

            let total_size = record::standard_record_size(target.len(), buf.len());

            let header = RecordHeader {
                timestamp_ns,
                formatter: format::standard_format_fn,
                level,
                _pad: [0u8; 7],
            };

            let backoff = Backoff::new();
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
                        break;
                    }
                    Err(nexus_logbuf::TryClaimError::Full) => match state.backpressure {
                        crate::BackpressureMode::Drop => return,
                        crate::BackpressureMode::Backoff => {
                            state.unparker.unpark();
                            backoff.snooze();
                        }
                    },
                    Err(nexus_logbuf::TryClaimError::ZeroLength) => {
                        unreachable!("record size is always > 0");
                    }
                }
            }
        });
    });
}

/// Hot-path submit: writes raw encoded payload to the logbuf.
///
/// Unlike `log_impl` which formats text via `format_args!()`, this
/// receives pre-encoded bytes and a function pointer for consumer-side
/// formatting. The encode closure writes argument bytes directly into
/// the claimed region — no `TLS_BUF` needed.
pub(crate) fn hot_log_submit(
    level: u8,
    payload_size: usize,
    fmt_fn: crate::record::FormatFn,
    encode: impl FnOnce(&mut [u8]),
) {
    let state = match LOGGER.get() {
        Some(s) => s,
        None => return,
    };
    if !state.running.load(Ordering::Relaxed) {
        return;
    }

    let timestamp_ns = crate::format::snap_timestamp();
    let total_size = HEADER_SIZE + payload_size;

    let header = RecordHeader {
        timestamp_ns,
        formatter: fmt_fn,
        level,
        _pad: [0u8; 7],
    };

    TLS_PRODUCER.with(|producer_cell| {
        let mut producer_opt = producer_cell
            .try_borrow_mut()
            .expect("re-entrant logging detected: Display/Debug impls must not call log macros");
        let producer = producer_opt.get_or_insert_with(|| state.source_producer.clone());

        let backoff = Backoff::new();
        loop {
            match producer.try_claim(total_size) {
                Ok(mut claim) => {
                    record::write_header(&mut claim, &header);
                    encode(&mut claim[HEADER_SIZE..]);
                    claim.commit();
                    break;
                }
                Err(nexus_logbuf::TryClaimError::Full) => match state.backpressure {
                    crate::BackpressureMode::Drop => return,
                    crate::BackpressureMode::Backoff => {
                        state.unparker.unpark();
                        backoff.snooze();
                    }
                },
                Err(nexus_logbuf::TryClaimError::ZeroLength) => {
                    unreachable!("record size is always > 0");
                }
            }
        }
    });
}

/// Logger that implements `log::Log` by writing records to the logbuf.
#[cfg(feature = "log-compat")]
pub(crate) struct Logger;

#[cfg(feature = "log-compat")]
impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        LOGGER
            .get()
            .map(|s| level_to_u8(metadata.level()) <= s.max_level.as_u8())
            .unwrap_or(false)
    }

    fn log(&self, record: &log::Record) {
        let state = match LOGGER.get() {
            Some(s) => s,
            None => return,
        };

        if level_to_u8(record.level()) > state.max_level.as_u8() {
            return;
        }

        log_impl(
            level_to_u8(record.level()),
            record.target(),
            record.line().unwrap_or(0),
            *record.args(),
        );
    }

    fn flush(&self) {}
}
