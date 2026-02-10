//! InqJet — High-performance logging for latency-critical Rust applications.
//!
//! All log data flows through a nexus-logbuf ring buffer as raw bytes with
//! function pointer dispatch. Formatting happens on a background archiver
//! thread, not the producer.
//!
//! # Quick Start
//!
//! ```rust, ignore
//! use inqjet::InqJetBuilder;
//! use log::{info, error, LevelFilter};
//!
//! let _guard = InqJetBuilder::default()
//!     .with_writer(std::io::stdout())
//!     .with_log_level(LevelFilter::Info)
//!     .build()?;
//!
//! info!("Server started on port {}", 8080);
//! error!("Connection failed: {}", "timeout");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crossbeam_utils::sync::{Parker, Unparker};
use log::LevelFilter;
use std::io::{self, IsTerminal};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::consumer::Stream;
use crate::logger::{Logger, LoggerState, LOGGER};

pub(crate) mod consumer;
pub(crate) mod format;
pub(crate) mod logger;
pub(crate) mod record;

/// Default logbuf capacity in bytes (64KB).
const DEFAULT_CAPACITY: usize = 65_536;

/// Default parking timeout for the archiver thread.
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5);

/// Controls ANSI color output.
pub enum ColorMode {
    /// Enable colors if stdout is a terminal, `NO_COLOR` is unset, and
    /// `TERM` is not `"dumb"`.
    Auto,
    /// Always emit ANSI color codes.
    Always,
    /// Never emit ANSI color codes.
    Never,
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

/// Errors during logger initialization.
#[derive(Debug, thiserror::Error)]
pub enum InqJetBuilderError {
    #[error("no configured writer for appender")]
    NoConfiguredWriter,

    #[error("no configured log level")]
    NoConfiguredLogLevel,

    #[error("io error: {0}")]
    IoError(#[from] io::Error),

    #[error("{0}")]
    SetLoggerError(#[from] log::SetLoggerError),
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
}

impl<W> Default for InqJetBuilder<W> {
    fn default() -> Self {
        Self {
            wr: None,
            level: None,
            cap: None,
            timeout: Some(DEFAULT_TIMEOUT),
            color_mode: ColorMode::Auto,
        }
    }
}

impl<W> InqJetBuilder<W>
where
    W: io::Write + Send + 'static,
{
    /// Sets the output writer for log messages.
    pub fn with_writer(mut self, wr: W) -> Self {
        self.wr = Some(wr);
        self
    }

    /// Sets the ring buffer size in bytes.
    ///
    /// Rounded up to the next power of two by nexus-logbuf.
    /// Default: 65536 (64KB).
    pub fn with_buffer_size(mut self, cap: usize) -> Self {
        self.cap = Some(cap);
        self
    }

    /// Sets the maximum log level to process.
    pub fn with_log_level(mut self, level: LevelFilter) -> Self {
        self.level = Some(level);
        self
    }

    /// Sets the archiver thread parking timeout.
    ///
    /// - `Some(duration)`: Park with timeout, periodic wakeups
    /// - `None`: Busy spin (highest CPU, lowest latency)
    pub fn with_timeout(mut self, timeout: Option<std::time::Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the color mode for log output.
    pub fn with_color_mode(mut self, color_mode: ColorMode) -> Self {
        self.color_mode = color_mode;
        self
    }

    /// Builds the logger, spawns the archiver thread, and registers
    /// as the global `log` crate logger.
    ///
    /// Returns a guard that shuts down the logger when dropped.
    pub fn build(self) -> Result<InqJetGuard, InqJetBuilderError> {
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
            max_level: level,
            running: running.clone(),
        });

        log::set_boxed_logger(Box::new(Logger))?;
        log::set_max_level(level);

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
            if std::env::var("NO_COLOR").is_ok() {
                return false;
            }
            if let Ok(term) = std::env::var("TERM") {
                if term == "dumb" {
                    return false;
                }
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
    use std::io::BufWriter;

    use hdrhistogram::Histogram;
    use tracing::Level;
    use tracing_log::LogTracer;
    use tracing_subscriber::fmt;

    fn print_histogram_stats(name: &str, hist: &Histogram<u64>) {
        println!("\n=== {} Performance Profile ===", name);
        println!("Count: {}", hist.len());
        println!("Min: {:>8.2}us", hist.min() as f64 / 1000.0);
        println!("Mean: {:>7.2}us", hist.mean() / 1000.0);
        println!("Max: {:>8.2}us", hist.max() as f64 / 1000.0);
        println!("StdDev: {:>5.2}us", hist.stdev() / 1000.0);
        println!();
        println!("Percentiles:");
        println!(
            "  50th: {:>6.2}us",
            hist.value_at_quantile(0.50) as f64 / 1000.0
        );
        println!(
            "  90th: {:>6.2}us",
            hist.value_at_quantile(0.90) as f64 / 1000.0
        );
        println!(
            "  95th: {:>6.2}us",
            hist.value_at_quantile(0.95) as f64 / 1000.0
        );
        println!(
            "  99th: {:>6.2}us",
            hist.value_at_quantile(0.99) as f64 / 1000.0
        );
        println!(
            " 99.9th: {:>5.2}us",
            hist.value_at_quantile(0.999) as f64 / 1000.0
        );
        println!(
            " 99.99th: {:>4.2}us",
            hist.value_at_quantile(0.9999) as f64 / 1000.0
        );
        println!();
    }

    #[ignore]
    #[test]
    fn bench_inqjet() {
        let tmp = std::env::temp_dir().join("inqjet_bench.log");
        println!("Writing to {}", tmp.display());
        let file = std::fs::OpenOptions::new()
            .truncate(true)
            .create(true)
            .write(true)
            .open(tmp)
            .expect("Failed to open file");
        let _guard = InqJetBuilder::default()
            .with_writer(BufWriter::new(file))
            .with_log_level(LevelFilter::Info)
            .with_buffer_size(262_144) // 256KB ring buffer
            .with_timeout(None)
            .with_color_mode(ColorMode::Never)
            .build()
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(100));

        let n = 100_000;
        let mut hist = Histogram::<u64>::new_with_bounds(1, 60 * 1000 * 1000, 3).unwrap();

        println!("Warming up InqJet...");
        for i in 0..100 {
            log::info!("warmup message {}", i);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));

        println!("Starting InqJet benchmark with {} iterations...", n);
        for i in 0..n {
            let start = std::time::Instant::now();
            log::info!("logging to inqjet logger! msg number: {}", i);
            let elapsed = start.elapsed();
            hist.record(elapsed.as_nanos() as u64).unwrap();

            if i % 1000 == 0 {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(1));
        print_histogram_stats("InqJet", &hist);
    }

    #[ignore]
    #[test]
    fn bench_tracing() {
        LogTracer::init().unwrap();
        let builder = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .buffered_lines_limit(262_144);
        let (writer, _guard) = builder.finish(io::stdout());

        let subscriber = fmt::Subscriber::builder()
            .with_writer(writer)
            .with_max_level(Level::INFO)
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(100));

        let n = 100_000;
        let mut hist = Histogram::<u64>::new_with_bounds(1, 60 * 1000 * 1000, 3).unwrap();

        println!("Warming up Tracing...");
        for i in 0..100 {
            log::info!("warmup message {}", i);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));

        println!("Starting Tracing benchmark with {} iterations...", n);
        for i in 0..n {
            let start = std::time::Instant::now();
            log::info!("logging to tracing logger! msg number: {}", i);
            let elapsed = start.elapsed();
            hist.record(elapsed.as_nanos() as u64).unwrap();

            if i % 1000 == 0 {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(1));
        print_histogram_stats("Tracing", &hist);
    }
}
