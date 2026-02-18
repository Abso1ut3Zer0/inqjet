//! InqJet — High-performance logging for latency-critical Rust applications.
//!
//! All log data flows through a nexus-logbuf ring buffer as raw bytes with
//! function pointer dispatch. Formatting happens on a background archiver
//! thread, not the producer.
//!
//! # Quick Start
//!
//! ```rust, ignore
//! use inqjet::{InqJetBuilder, LevelFilter};
//!
//! let _guard = InqJetBuilder::default()
//!     .with_writer(std::io::stdout())
//!     .with_log_level(LevelFilter::Info)
//!     .build()?;
//!
//! // Standalone macros — bypass the log facade, direct to logbuf:
//! inqjet::info!("Server started on port {}", 8080);
//! inqjet::error!("Connection failed: {}", "timeout");
//!
//! // log crate macros still work via the bridge (with `log-compat` feature):
//! log::info!("This also works");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crossbeam_utils::sync::{Parker, Unparker};
use std::io::{self, IsTerminal};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::consumer::Stream;
use crate::logger::{LOGGER, LoggerState};

pub(crate) mod consumer;
pub(crate) mod format;
pub(crate) mod logger;
pub(crate) mod pod;
pub(crate) mod record;

pub use pod::Pod;
pub use pod::reserve_fallback_capacity;

// -- Log levels ---------------------------------------------------------------

/// Log level filter for configuring maximum verbosity.
///
/// Controls which log messages are processed. Messages at or below the
/// configured level are logged; more verbose levels are filtered out.
///
/// # Examples
///
/// ```rust,ignore
/// use inqjet::{InqJetBuilder, LevelFilter};
///
/// let _guard = InqJetBuilder::default()
///     .with_writer(std::io::stdout())
///     .with_log_level(LevelFilter::Info)
///     .build()?;
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum LevelFilter {
    /// No messages will be logged.
    Off = 0,
    /// Only error messages.
    Error = 1,
    /// Error and warning messages.
    Warn = 2,
    /// Error, warning, and info messages.
    Info = 3,
    /// Error, warning, info, and debug messages.
    Debug = 4,
    /// All messages including trace.
    Trace = 5,
}

impl LevelFilter {
    pub(crate) fn as_u8(self) -> u8 {
        self as u8
    }
}

#[cfg(feature = "log-compat")]
impl From<log::LevelFilter> for LevelFilter {
    fn from(lf: log::LevelFilter) -> Self {
        match lf {
            log::LevelFilter::Off => LevelFilter::Off,
            log::LevelFilter::Error => LevelFilter::Error,
            log::LevelFilter::Warn => LevelFilter::Warn,
            log::LevelFilter::Info => LevelFilter::Info,
            log::LevelFilter::Debug => LevelFilter::Debug,
            log::LevelFilter::Trace => LevelFilter::Trace,
        }
    }
}

#[cfg(feature = "log-compat")]
impl From<LevelFilter> for log::LevelFilter {
    fn from(lf: LevelFilter) -> Self {
        match lf {
            LevelFilter::Off => log::LevelFilter::Off,
            LevelFilter::Error => log::LevelFilter::Error,
            LevelFilter::Warn => log::LevelFilter::Warn,
            LevelFilter::Info => log::LevelFilter::Info,
            LevelFilter::Debug => log::LevelFilter::Debug,
            LevelFilter::Trace => log::LevelFilter::Trace,
        }
    }
}

// -- Proc macro re-exports ----------------------------------------------------

pub use inqjet_macros::Pod;

#[doc(hidden)]
pub use inqjet_macros::__hot_log;

// -- Macro internals (not public API) ----------------------------------------

/// Implementation details used by proc macro-generated code.
///
/// **Not public API.** Any item in this module may change or disappear
/// in any release. Do not depend on it.
#[doc(hidden)]
pub mod __private {
    pub use crate::pod::{HotArg, HotDecode, HotEncode, PreFormatted, PreFormattedValue, Witness};

    #[inline]
    pub fn log_enabled(level: u8) -> bool {
        crate::logger::log_enabled(level)
    }

    pub fn write_log_prefix(
        timestamp_ns: u64,
        level: u8,
        target: &str,
        line: u32,
        out: &mut dyn std::io::Write,
    ) {
        crate::format::write_log_prefix(timestamp_ns, level, target, line, out)
    }

    pub fn fallback_stash_clear() {
        crate::pod::fallback_stash_clear()
    }

    pub fn hot_log_submit(
        level: u8,
        payload_size: usize,
        fmt_fn: fn(u64, u8, &[u8], &mut dyn std::io::Write),
        encode: impl FnOnce(&mut [u8]),
    ) {
        crate::logger::hot_log_submit(level, payload_size, fmt_fn, encode)
    }
}

/// Log at the ERROR level.
///
/// Bypasses the `log` facade — writes directly to the logbuf.
///
/// # Examples
///
/// ```rust,ignore
/// inqjet::error!("connection lost");
/// inqjet::error!("failed after {} retries", count);
/// inqjet::error!(target: "net::tcp", "timeout on fd {}", fd);
/// ```
#[macro_export]
macro_rules! error {
    (target: $target:expr, $($arg:tt)+) => {
        if $crate::__private::log_enabled(1) {
            $crate::__hot_log!(1u8, $target, line!(), $($arg)+)
        }
    };
    ($($arg:tt)+) => {
        if $crate::__private::log_enabled(1) {
            $crate::__hot_log!(1u8, module_path!(), line!(), $($arg)+)
        }
    };
}

/// Log at the WARN level.
///
/// Bypasses the `log` facade — writes directly to the logbuf.
///
/// # Examples
///
/// ```rust,ignore
/// inqjet::warn!("queue depth high: {}", depth);
/// inqjet::warn!(target: "engine", "slow tick: {}us", elapsed);
/// ```
#[macro_export]
macro_rules! warn {
    (target: $target:expr, $($arg:tt)+) => {
        if $crate::__private::log_enabled(2) {
            $crate::__hot_log!(2u8, $target, line!(), $($arg)+)
        }
    };
    ($($arg:tt)+) => {
        if $crate::__private::log_enabled(2) {
            $crate::__hot_log!(2u8, module_path!(), line!(), $($arg)+)
        }
    };
}

/// Log at the INFO level.
///
/// Bypasses the `log` facade — writes directly to the logbuf.
///
/// # Examples
///
/// ```rust,ignore
/// inqjet::info!("server started on port {}", port);
/// inqjet::info!(target: "app::init", "ready");
/// ```
#[macro_export]
macro_rules! info {
    (target: $target:expr, $($arg:tt)+) => {
        if $crate::__private::log_enabled(3) {
            $crate::__hot_log!(3u8, $target, line!(), $($arg)+)
        }
    };
    ($($arg:tt)+) => {
        if $crate::__private::log_enabled(3) {
            $crate::__hot_log!(3u8, module_path!(), line!(), $($arg)+)
        }
    };
}

/// Log at the DEBUG level.
///
/// Bypasses the `log` facade — writes directly to the logbuf.
///
/// # Examples
///
/// ```rust,ignore
/// inqjet::debug!("state: {:?}", state);
/// inqjet::debug!(target: "parser", "token: {}", tok);
/// ```
#[macro_export]
macro_rules! debug {
    (target: $target:expr, $($arg:tt)+) => {
        if $crate::__private::log_enabled(4) {
            $crate::__hot_log!(4u8, $target, line!(), $($arg)+)
        }
    };
    ($($arg:tt)+) => {
        if $crate::__private::log_enabled(4) {
            $crate::__hot_log!(4u8, module_path!(), line!(), $($arg)+)
        }
    };
}

/// Log at the TRACE level.
///
/// Bypasses the `log` facade — writes directly to the logbuf.
///
/// # Examples
///
/// ```rust,ignore
/// inqjet::trace!("enter process_order");
/// inqjet::trace!(target: "hot", "tick {}", seq);
/// ```
#[macro_export]
macro_rules! trace {
    (target: $target:expr, $($arg:tt)+) => {
        if $crate::__private::log_enabled(5) {
            $crate::__hot_log!(5u8, $target, line!(), $($arg)+)
        }
    };
    ($($arg:tt)+) => {
        if $crate::__private::log_enabled(5) {
            $crate::__hot_log!(5u8, module_path!(), line!(), $($arg)+)
        }
    };
}

/// Default logbuf capacity in bytes (64KB).
const DEFAULT_CAPACITY: usize = 65_536;

/// Default parking timeout for the archiver thread.
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5);

/// Controls ANSI color output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColorMode {
    /// Enable colors if stdout is a terminal, `NO_COLOR` is unset, and
    /// `TERM` is not `"dumb"`.
    Auto,
    /// Always emit ANSI color codes.
    Always,
    /// Never emit ANSI color codes.
    Never,
}

/// Controls producer behavior when the ring buffer is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackpressureMode {
    /// Exponential backoff via `crossbeam::Backoff::snooze()`.
    ///
    /// Spins briefly, then yields. Guarantees delivery at the cost of
    /// variable producer latency under pressure. This is the default.
    Backoff,
    /// Drop the message and return immediately.
    ///
    /// Lowest and most predictable producer latency. Use when bounded
    /// latency matters more than log completeness.
    Drop,
}

/// Updates the global log level at runtime.
///
/// Takes effect immediately for all subsequent log calls. Existing
/// in-flight records are not affected.
pub fn set_level(level: LevelFilter) {
    logger::MAX_LEVEL.store(level.as_u8(), Ordering::Relaxed);
    #[cfg(feature = "log-compat")]
    log::set_max_level(level.into());
}

/// RAII guard that manages the logger's background archiver thread.
///
/// When dropped:
/// 1. Signals the archiver to stop
/// 2. Wakes the archiver thread
/// 3. Joins the thread (drains remaining records, flushes writer)
///
/// **Shutdown race:** Records that are mid-claim (between `try_claim`
/// success and `commit`) when the guard drops may not be consumed.
/// The window is nanoseconds (a memcpy + atomic store). This matches
/// the behavior of tracing-subscriber, slog, and other loggers.
pub struct InqJetGuard {
    handle: Option<std::thread::JoinHandle<()>>,
    running: Arc<AtomicBool>,
    unparker: Unparker,
}

impl Drop for InqJetGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.unparker.unpark();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl std::fmt::Debug for InqJetGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InqJetGuard")
            .field("running", &self.running.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// Errors during logger initialization.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InqJetBuilderError {
    #[error("no configured writer for appender")]
    NoConfiguredWriter,

    #[error("no configured log level")]
    NoConfiguredLogLevel,

    #[error("logger already initialized (build() called more than once)")]
    AlreadyInitialized,

    #[error("io error: {0}")]
    IoError(#[from] io::Error),
}

/// Builder for configuring and creating an InqJet logger.
///
/// # Buffer Size
///
/// `with_buffer_size` sets the ring buffer size in **bytes** (not messages).
/// nexus-logbuf rounds up to the next power of two internally.
/// Default: 64KB (~300-600 typical log messages).
pub struct InqJetBuilder<W> {
    wr: Option<W>,
    level: Option<LevelFilter>,
    cap: Option<usize>,
    timeout: Option<std::time::Duration>,
    color_mode: ColorMode,
    backpressure: BackpressureMode,
}

impl<W> std::fmt::Debug for InqJetBuilder<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InqJetBuilder")
            .field("writer", &self.wr.as_ref().map(|_| "..."))
            .field("level", &self.level)
            .field("buffer_size", &self.cap)
            .field("timeout", &self.timeout)
            .field("color_mode", &self.color_mode)
            .field("backpressure", &self.backpressure)
            .finish()
    }
}

impl<W> Default for InqJetBuilder<W> {
    fn default() -> Self {
        Self {
            wr: None,
            level: None,
            cap: None,
            timeout: Some(DEFAULT_TIMEOUT),
            color_mode: ColorMode::Auto,
            backpressure: BackpressureMode::Backoff,
        }
    }
}

impl<W> InqJetBuilder<W>
where
    W: io::Write + Send + 'static,
{
    /// Sets the output writer for log messages.
    #[must_use]
    pub fn with_writer(mut self, wr: W) -> Self {
        self.wr = Some(wr);
        self
    }

    /// Sets the ring buffer size in bytes.
    ///
    /// Rounded up to the next power of two by nexus-logbuf.
    /// Default: 65536 (64KB).
    #[must_use]
    pub fn with_buffer_size(mut self, cap: usize) -> Self {
        self.cap = Some(cap);
        self
    }

    /// Sets the maximum log level to process.
    #[must_use]
    pub fn with_log_level(mut self, level: LevelFilter) -> Self {
        self.level = Some(level);
        self
    }

    /// Sets the archiver thread parking timeout.
    ///
    /// - `Some(duration)`: Park with timeout, periodic wakeups
    /// - `None`: Busy spin (highest CPU, lowest latency)
    #[must_use]
    pub fn with_timeout(mut self, timeout: Option<std::time::Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the color mode for log output.
    #[must_use]
    pub fn with_color_mode(mut self, color_mode: ColorMode) -> Self {
        self.color_mode = color_mode;
        self
    }

    /// Sets the backpressure strategy when the ring buffer is full.
    ///
    /// Default: [`BackpressureMode::Backoff`].
    #[must_use]
    pub fn with_backpressure(mut self, mode: BackpressureMode) -> Self {
        self.backpressure = mode;
        self
    }

    /// Builds the logger, spawns the archiver thread, and registers
    /// as the global `log` crate logger.
    ///
    /// Returns a guard that shuts down the logger when dropped.
    pub fn build(self) -> Result<InqJetGuard, InqJetBuilderError> {
        if LOGGER.get().is_some() {
            return Err(InqJetBuilderError::AlreadyInitialized);
        }
        let capacity = self.cap.unwrap_or(DEFAULT_CAPACITY);
        let writer = self.wr.ok_or(InqJetBuilderError::NoConfiguredWriter)?;
        let level = self.level.ok_or(InqJetBuilderError::NoConfiguredLogLevel)?;
        let timeout = self.timeout;

        format::COLOR_ENABLED.store(is_color_enabled(self.color_mode), Ordering::Relaxed);

        let (producer, consumer) = nexus_logbuf::queue::mpsc::new(capacity);

        let mut stream = Stream {
            consumer,
            writer: Box::new(writer),
        };

        let parker = Parker::new();
        let unparker = parker.unparker().clone();

        let running = Arc::new(AtomicBool::new(true));
        let running_archiver = running.clone();

        let handle = std::thread::spawn(move || {
            crate::consumer::archiver_loop(&mut stream, &running_archiver, timeout, parker);
        });

        let _ = LOGGER.set(LoggerState {
            source_producer: producer,
            unparker: unparker.clone(),
            backpressure: self.backpressure,
            running: running.clone(),
        });

        logger::MAX_LEVEL.store(level.as_u8(), Ordering::Release);

        // Best-effort: if another logger is already registered the standalone
        // macros still work — they bypass the log facade entirely.
        #[cfg(feature = "log-compat")]
        {
            let _ = log::set_boxed_logger(Box::new(crate::logger::Logger));
            log::set_max_level(level.into());
        }

        Ok(InqJetGuard {
            handle: Some(handle),
            running,
            unparker,
        })
    }
}

fn is_color_enabled(color_mode: ColorMode) -> bool {
    match color_mode {
        ColorMode::Auto => {
            if !std::io::stdout().is_terminal() {
                return false;
            }
            if std::env::var_os("NO_COLOR").is_some() {
                return false;
            }
            if let Ok(term) = std::env::var("TERM")
                && term == "dumb"
            {
                return false;
            }
            true
        }
        ColorMode::Always => true,
        ColorMode::Never => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_fails_without_writer() {
        let result = InqJetBuilder::<Vec<u8>>::default()
            .with_log_level(LevelFilter::Info)
            .build();
        assert!(matches!(
            result,
            Err(InqJetBuilderError::NoConfiguredWriter)
        ));
    }

    #[test]
    fn build_fails_without_log_level() {
        let result = InqJetBuilder::default()
            .with_writer(std::io::sink())
            .build();
        assert!(matches!(
            result,
            Err(InqJetBuilderError::NoConfiguredLogLevel)
        ));
    }
}
