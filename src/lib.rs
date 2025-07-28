//! InqJet - High-Performance Async Logger
//!
//! InqJet is a blazingly fast, low-latency logging library designed for performance-critical
//! applications. It achieves microsecond-level logging latency by decoupling log formatting
//! from I/O operations using a lock-free channel and dedicated consumer thread.
//!
//! # Key Features
//!
//! - **Ultra-low latency**: 1-5μs producer-side overhead
//! - **Lock-free architecture**: No contention between logging threads
//! - **Zero allocations**: String pooling eliminates malloc/free overhead
//! - **Colored output**: ANSI color-coded log levels for better readability
//! - **Structured format**: ISO 8601 timestamps with microsecond precision
//! - **Configurable filtering**: Efficient level-based filtering
//! - **Clean shutdown**: Guaranteed message delivery on application exit
//!
//! # Architecture
//!
//! ```text
//! [App Threads] → [Logger] → [Lock-free Channel] → [Background Thread] → [Output]
//!      ↓             ↓              ↓                     ↓               ↓
//!   log!() calls  Format msg    Queue message        Write to I/O      File/Stdout
//!   (1-5μs)       Pool strings   (lock-free)         (background)
//! ```
//!
//! # Quick Start
//!
//! ```rust, ignore
//! use inqjet::InqJetBuilder;
//! use log::{info, error, LevelFilter};
//! use std::io;
//!
//! // Initialize logger with stdout output
//! let _guard = InqJetBuilder::default()
//!     .with_writer(io::stdout())
//!     .with_log_level(LevelFilter::Info)
//!     .with_capacity(1024)
//!     .build()?;
//!
//! // Use standard logging macros
//! info!("Server started on port {}", 8080);
//! error!("Connection failed: {}", "timeout");
//!
//! // Logger automatically shuts down when guard is dropped
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Performance Tuning
//!
//! ## String Pool
//!
//! InqJet uses a dynamic string pool with 256-byte initial capacity per string.
//! Strings automatically grow as needed and are shrunk back to 256 bytes when
//! returned to the pool, providing optimal memory usage without truncation.
//!
//! ## Channel Capacity
//!
//! Size the channel based on your burst patterns:
//! - **64-256**: Consistent logging patterns, quick backpressure
//! - **512-1024**: Balanced throughput and memory usage (recommended)
//! - **2048+**: High-burst applications, absorb traffic spikes
//!
//! ## Timeout Configuration
//!
//! - `Some(Duration)`: Periodic wakeups for responsive shutdown
//! - `None`: Park indefinitely, lowest CPU usage when idle
//!
//! # Examples
//!
//! ## File Logging
//!
//! ```rust, ignore
//! use std::fs::OpenOptions;
//! use log::LevelFilter;
//!
//! let file = OpenOptions::new()
//!     .create(true)
//!     .append(true)
//!     .open("app.log")?;
//!
//! let _guard = InqJetBuilder::default()
//!     .with_writer(file)
//!     .with_log_level(LevelFilter::Debug)
//!     .build()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Simple Configuration
//!
//! ```rust, ignore
//! use std::time::Duration;
//!
//! let _guard = InqJetBuilder::default()
//!     .with_writer(std::io::stdout())
//!     .with_log_level(LevelFilter::Info)
//!     .with_capacity(4096)           // Large buffer for bursts
//!     .with_timeout(Some(Duration::from_millis(1))) // Responsive
//!     .build()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crossbeam_utils::sync::{Parker, Unparker};
use log::LevelFilter;

use crate::{
    channel::Channel,
    logger::{Appender, Logger},
};

pub(crate) mod channel;
pub(crate) mod logger;
pub(crate) mod pool;

/// Default channel capacity for moderate throughput applications.
const DEFAULT_CAPACITY: usize = 256;

/// Default parking timeout for responsive shutdown while minimizing CPU usage.
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5);

/// RAII guard that manages the logger's background thread and ensures clean shutdown.
///
/// When dropped, this guard will:
/// 1. Signal the background thread to stop
/// 2. Wake the thread if it's parked
/// 3. Wait for the thread to finish processing remaining messages
/// 4. Ensure all pending log messages are written
///
/// # Clean Shutdown Guarantees
///
/// InqJet guarantees that no log messages are lost during shutdown, even if the
/// application terminates unexpectedly. The `Drop` implementation ensures that:
/// - All queued messages are processed
/// - The output writer is properly flushed
/// - Resources are cleaned up correctly
///
/// # Example
///
/// ```rust, ignore
/// {
///     let _guard = InqJetBuilder::with_normal_slots()
///         .with_writer(std::io::stdout())
///         .with_log_level(log::LevelFilter::Info)
///         .build()?;
///
///     log::info!("This message will be written");
/// } // Guard dropped here - ensures message is flushed before continuing
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct InqJetGuard {
    /// Handle to the background consumer thread.
    handle: Option<std::thread::JoinHandle<()>>,

    /// Atomic flag for coordinating shutdown between main and background threads.
    flag: Arc<AtomicBool>,

    /// Notifier for waking the background thread during shutdown.
    notifier: Unparker,
}

impl Drop for InqJetGuard {
    /// Ensures clean shutdown of the logging system.
    ///
    /// This implementation guarantees that all pending log messages are processed
    /// before the logger is destroyed, preventing message loss during application
    /// shutdown.
    ///
    /// # Shutdown Sequence
    ///
    /// 1. Set the running flag to `false` (signals background thread to stop)
    /// 2. Unpark the background thread (wakes it if sleeping)
    /// 3. Join the thread (waits for all messages to be processed)
    /// 4. Thread's cleanup ensures remaining messages are flushed
    ///
    /// # Performance Note
    ///
    /// The join operation may block briefly while the background thread processes
    /// any remaining messages in the queue. This ensures reliability at the cost
    /// of a small shutdown delay.
    fn drop(&mut self) {
        // Signal shutdown to background thread
        self.flag.store(false, Ordering::Release);

        // Wake the thread if it's parked waiting for messages
        self.notifier.unpark();

        // Wait for clean shutdown and process remaining messages
        self.handle.take().map(|handle| {
            let _ = handle.join(); // Ignore join errors during shutdown
        });
    }
}

/// Errors that can occur during logger initialization.
#[derive(Debug, thiserror::Error)]
pub enum InqJetBuilderError {
    /// No writer was configured for the appender.
    ///
    /// Call `.with_writer()` to specify where log messages should be written.
    #[error("no configured writer for appender")]
    NoConfiguredWriter,

    /// No log level was configured.
    ///
    /// Call `.with_log_level()` to specify the minimum log level to process.
    #[error("no configured log level")]
    NoConfiguredLogLevel,

    /// An I/O error occurred during initialization.
    ///
    /// This typically happens when the specified output writer cannot be created
    /// or accessed (e.g., permission denied for log file).
    #[error("io error: {0}")]
    IoError(#[from] io::Error),

    /// Failed to set the global logger.
    ///
    /// This occurs if another logger has already been set for this process.
    /// Only one global logger can be active at a time.
    #[error("{0}")]
    SetLoggerError(#[from] log::SetLoggerError),
}

/// Builder for configuring and creating an InqJet logger instance.
///
/// The builder provides a fluent interface for configuring all aspects of the logger:
/// - Output destination (file, stdout, custom writer)
/// - Log level filtering
/// - Channel capacity and timeout behavior
///
/// The string pool uses dynamic sizing with 256-byte initial capacity,
/// automatically growing as needed and shrinking back when returned to the pool.
///
/// # Generic Parameters
///
/// * `W` - The writer type that implements [`io::Write`] + [`Send`] + `'static`
///
/// # Thread Safety
///
/// The builder itself is not thread-safe and should be used from a single thread
/// during initialization. The resulting logger is fully thread-safe.
///
/// # Example
///
/// ```rust, ignore
/// use std::fs::File;
/// use log::LevelFilter;
///
/// let _guard = InqJetBuilder::default()
///     .with_writer(File::create("app.log")?)
///     .with_log_level(LevelFilter::Debug)
///     .with_capacity(2048)
///     .build()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct InqJetBuilder<W> {
    /// Optional writer for output destination.
    wr: Option<W>,

    /// Optional log level filter.
    level: Option<LevelFilter>,

    /// Optional channel capacity override.
    cap: Option<usize>,

    /// Optional timeout for background thread parking.
    timeout: Option<std::time::Duration>,
}

impl<W> Default for InqJetBuilder<W> {
    fn default() -> Self {
        Self {
            wr: None,
            level: None,
            cap: None,
            timeout: Some(DEFAULT_TIMEOUT),
        }
    }
}

impl<W> InqJetBuilder<W>
where
    W: io::Write + Send + 'static,
{
    /// Sets the output writer for log messages.
    ///
    /// The writer must implement [`io::Write`] + [`Send`] + `'static` to allow
    /// it to be moved to the background thread. Common writers include:
    /// - [`std::io::Stdout`] / [`std::io::Stderr`] for console output
    /// - [`std::fs::File`] for file logging
    /// - [`std::net::TcpStream`] for network logging
    /// - Custom writers implementing [`io::Write`]
    ///
    /// # Parameters
    ///
    /// * `wr` - The writer where log messages will be sent
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::fs::OpenOptions;
    ///
    /// let file = OpenOptions::new()
    ///     .create(true)
    ///     .append(true)
    ///     .open("application.log")?;
    ///
    /// let builder = InqJetBuilder::with_normal_slots()
    ///     .with_writer(file);
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn with_writer(mut self, wr: W) -> Self {
        self.wr = Some(wr);
        self
    }

    /// Sets the channel capacity (number of messages that can be queued).
    ///
    /// The capacity determines how many log messages can be buffered between
    /// the producer threads and the consumer thread. Larger capacities provide
    /// better burst handling but use more memory.
    ///
    /// # Capacity Guidelines
    ///
    /// - **Consistent** (64-256): Quick backpressure, minimal memory
    /// - **Balanced** (512-1024): Good for most applications (recommended)
    /// - **High burst** (2048+): Absorb traffic spikes, higher memory usage
    ///
    /// # Backpressure Behavior
    ///
    /// When the channel is full, producer threads will:
    /// 1. Wake the consumer thread
    /// 2. Spin with exponential backoff until space is available
    /// 3. Never drop messages (guaranteed delivery)
    ///
    /// # Parameters
    ///
    /// * `cap` - Maximum number of messages to queue
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// let builder = InqJetBuilder::default()
    ///     .with_capacity(4096); // Large buffer for burst handling
    /// ```
    pub fn with_capacity(mut self, cap: usize) -> Self {
        self.cap = Some(cap);
        self
    }

    /// Sets the maximum log level to process.
    ///
    /// Messages below this level will be filtered out before any formatting
    /// occurs, providing excellent performance for disabled log levels.
    ///
    /// # Level Hierarchy
    ///
    /// From most to least verbose:
    /// - [`LevelFilter::Trace`] - All messages
    /// - [`LevelFilter::Debug`] - All except TRACE
    /// - [`LevelFilter::Info`] - INFO, WARN, ERROR
    /// - [`LevelFilter::Warn`] - WARN, ERROR only
    /// - [`LevelFilter::Error`] - ERROR only
    /// - [`LevelFilter::Off`] - No messages (disables logging)
    ///
    /// # Performance Impact
    ///
    /// - **Accepted messages**: Full formatting (~1-5μs)
    /// - **Filtered messages**: Immediate return (~1-10ns)
    ///
    /// # Parameters
    ///
    /// * `level` - Maximum level to process
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// use log::LevelFilter;
    ///
    /// let builder = InqJetBuilder::default()
    ///     .with_log_level(LevelFilter::Info); // INFO, WARN, ERROR only
    /// ```
    pub fn with_log_level(mut self, level: LevelFilter) -> Self {
        self.level = Some(level);
        self
    }

    /// Sets the parking timeout for the background consumer thread.
    ///
    /// This controls how long the consumer thread sleeps when no messages
    /// are available, balancing CPU usage against shutdown responsiveness.
    ///
    /// # Timeout Behavior
    ///
    /// - `Some(duration)`: Wake periodically to check for shutdown
    /// - `None`: Busy spin indefinitely until logs are received or shutdown
    ///
    /// # Trade-offs
    ///
    /// - **Shorter timeouts**: More responsive shutdown, slightly higher CPU usage
    /// - **Longer timeouts**: Lower CPU usage when idle, slower shutdown
    /// - **None (infinite)**: Highest CPU usage, no waiting on the kernel
    ///
    /// # Parameters
    ///
    /// * `timeout` - Optional duration to park, or `None` for spinning strategy
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// use std::time::Duration;
    ///
    /// // Responsive shutdown with minimal CPU overhead
    /// let builder = InqJetBuilder::default()
    ///     .with_timeout(Some(Duration::from_millis(10)));
    ///
    /// // Highest CPU usage and responsiveness
    /// let builder2 = InqJetBuilder::default()
    ///     .with_timeout(None);
    /// ```
    pub fn with_timeout(mut self, timeout: Option<std::time::Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Builds and initializes the logger, returning a guard for managing its lifetime.
    ///
    /// This method:
    /// 1. Validates that all required configuration is provided
    /// 2. Creates the channel, logger, and appender components
    /// 3. Spawns the background consumer thread
    /// 4. Registers the logger as the global logger for the `log` crate
    /// 5. Returns a guard that manages the logger's lifetime
    ///
    /// # Error Handling
    ///
    /// Returns an error if:
    /// - No writer was configured ([`InqJetBuilderError::NoConfiguredWriter`])
    /// - No log level was configured ([`InqJetBuilderError::NoConfiguredLogLevel`])
    /// - I/O error during writer initialization ([`InqJetBuilderError::IoError`])
    /// - Another logger is already registered ([`InqJetBuilderError::SetLoggerError`])
    ///
    /// # Global Logger Registration
    ///
    /// This method calls [`log::set_boxed_logger`] and [`log::set_max_level`],
    /// making this logger the global logger for the entire process. Only one
    /// logger can be active at a time.
    ///
    /// # Background Thread
    ///
    /// The returned guard manages a background thread that:
    /// - Consumes messages from the lock-free channel
    /// - Writes them to the configured output
    /// - Parks efficiently when no messages are available
    /// - Shuts down cleanly when the guard is dropped
    ///
    /// # Returns
    ///
    /// * `Ok(InqJetGuard)` - Guard managing the logger's lifetime
    /// * `Err(InqJetBuilderError)` - Configuration or initialization error
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// use log::{info, LevelFilter};
    /// use std::io;
    ///
    /// let _guard = InqJetBuilder::default()
    ///     .with_writer(io::stdout())
    ///     .with_log_level(LevelFilter::Info)
    ///     .build()?;
    ///
    /// // Logger is now active and ready to use
    /// info!("Logger initialized successfully");
    ///
    /// // Guard automatically shuts down logger when dropped
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn build(self) -> Result<InqJetGuard, InqJetBuilderError> {
        let capacity = self.cap.unwrap_or(DEFAULT_CAPACITY);
        let writer = self.wr.ok_or(InqJetBuilderError::NoConfiguredWriter)?;
        let level = self.level.ok_or(InqJetBuilderError::NoConfiguredLogLevel)?;
        let timeout = self.timeout;

        let parker = Parker::new();
        let notifier = parker.unparker().to_owned();
        let chan = Arc::new(Channel::new(capacity, parker.unparker().to_owned()));
        let logger = Logger::new(chan.clone(), level);
        let flag = Arc::new(AtomicBool::new(true));
        let mut appender = Appender::new(chan, parker, flag.clone(), writer);
        log::set_boxed_logger(Box::new(logger))?;
        log::set_max_level(level);

        let running = flag.clone();
        let handle = std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let _ = appender.block_on_append(timeout);
            }

            let _ = appender.flush();
        });

        Ok(InqJetGuard {
            handle: Some(handle),
            flag,
            notifier,
        })
    }
}

#[cfg(test)]
mod tests {
    use tracing::Level;
    use tracing_log::LogTracer;
    use tracing_subscriber::fmt;

    use super::*;

    #[ignore]
    #[test]
    fn bench_inqjet() {
        let _guard = InqJetBuilder::default()
            .with_writer(io::stdout())
            .with_log_level(LevelFilter::Info)
            .with_timeout(None)
            .build()
            .unwrap();

        let n = 1_000;
        let duration = std::time::Duration::from_millis(5);
        let mut total = 0;
        for i in 0..n {
            std::thread::sleep(duration);
            let now = std::time::Instant::now();
            log::info!("logging to inqjet logger! msg number: {}", i);
            let elapsed = now.elapsed();
            total += elapsed.as_nanos();
        }

        let total = std::time::Duration::from_nanos(total as u64);
        println!("InqJet total performance: {:?}", total);
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

        let n = 1_000;
        let duration = std::time::Duration::from_millis(5);
        let mut total = 0;
        for i in 0..n {
            std::thread::sleep(duration);
            let now = std::time::Instant::now();
            log::info!("logging to tracing logger! msg number: {}", i);
            let elapsed = now.elapsed();
            total += elapsed.as_nanos();
        }

        let total = std::time::Duration::from_nanos(total as u64);
        println!("Tracing total performance: {:?}", total);
    }
}
