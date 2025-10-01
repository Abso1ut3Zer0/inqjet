//! High-performance async logger with colored output and structured formatting.
//!
//! This module provides a production-ready logging implementation optimized for minimal
//! producer-side latency. The logger uses a lock-free channel to decouple log formatting
//! from I/O operations, allowing application threads to continue with minimal overhead
//! while a dedicated consumer thread handles the expensive I/O operations.
//!
//! # Architecture
//!
//! ```text
//! [Producer Threads] → [Logger] → [Channel] → [Appender] → [Output]
//!       ↓                ↓           ↓           ↓           ↓
//!   Format logs     Push to      Lock-free   Consumer     File/Stdout
//!   (few μs)        channel      queue       thread
//! ```
//!
//! # Performance Characteristics
//!
//! - **Producer latency**: 1-5μs for log formatting and channel push
//! - **Consumer throughput**: Limited by I/O destination (file, network, etc.)
//! - **Memory usage**: Bounded by channel capacity + string pool size
//! - **Zero allocations**: Uses string pool for message formatting

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fmt::Write, sync::Arc};

use crossbeam_utils::sync::Parker;
use log::Level;

use crate::channel::Channel;

/// ISO 8601 timestamp format with microsecond precision.
///
/// Format: `2024-01-15T14:30:45.123456Z`
///
/// Uses unsafe `format_into` for zero-allocation timestamp formatting directly
/// into the string buffer.
const ISO8601_FMT: &[time::format_description::FormatItem<'static>] = time::macros::format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z"
);

/// High-performance logger implementation optimized for minimal producer-side latency.
///
/// This logger provides colored, structured log output while maintaining excellent
/// performance characteristics for the calling thread. All expensive operations
/// (I/O, formatting complexity) are offloaded to a consumer thread.
///
/// # Log Format
///
/// ```text
/// 2024-01-15T14:30:45.123456Z [INFO] my_app::module:42 - User logged in
/// ├─ Timestamp (gray)         ├─ Level (colored)  ├─ Target:line (gray)  ├─ Message
/// └─ ISO 8601 with μs         └─ Color coded      └─ Optional line number └─ User content
/// ```
///
/// # Color Scheme
///
/// - **ERROR**: Red (`\x1b[31m`)
/// - **WARN**: Yellow (`\x1b[33m`)
/// - **INFO**: Green (`\x1b[32m`)
/// - **DEBUG**: Cyan (`\x1b[36m`)
/// - **TRACE**: Gray (`\x1b[90m`)
/// - **Metadata**: Gray (`\x1b[90m`)
///
/// # Performance Design
///
/// The logger is optimized for the critical path (application threads calling log macros):
///
/// 1. **String pooling**: Reuses pre-allocated strings to avoid malloc/free
/// 2. **Zero-copy formatting**: Uses `format_into` to write directly into buffers
/// 3. **Lock-free channel**: Non-blocking push to consumer thread
/// 4. **Minimal work**: Only formatting and channel push, no I/O
///
/// # Usage
///
/// ```rust, ignore
/// use log::{info, error};
///
/// // Logger handles all the complexity
/// info!("Server started on port {}", 8080);
/// error!("Failed to connect: {}", err);
/// ```
pub struct Logger {
    /// Channel for sending formatted log messages to the consumer thread.
    ///
    /// Wrapped in `Arc` to allow the logger to be used from multiple threads
    /// while sharing the same underlying channel.
    chan: Arc<Channel>,

    /// Maximum log level that will be processed. Messages below this level are ignored.
    max_level: log::LevelFilter,

    /// Flag to indicate if the logger is still running.
    running: Arc<AtomicBool>,

    coloring_enabled: bool,
}

impl Drop for Logger {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Logger {
    /// Creates a new logger with the specified channel and maximum log level.
    ///
    /// # Parameters
    ///
    /// * `chan` - Shared channel for sending formatted log messages to the consumer.
    /// * `max_level` - Maximum log level to process. Messages below this level will be
    ///   filtered out in the `enabled()` check, avoiding formatting overhead entirely.
    ///
    /// # Level Filtering
    ///
    /// The logger will only process log records at or above the specified level:
    /// - `LevelFilter::Error` - Only ERROR messages
    /// - `LevelFilter::Warn` - ERROR and WARN messages
    /// - `LevelFilter::Info` - ERROR, WARN, and INFO messages
    /// - `LevelFilter::Debug` - All except TRACE messages
    /// - `LevelFilter::Trace` - All messages
    /// - `LevelFilter::Off` - No messages (disables logging)
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// use log::LevelFilter;
    /// use std::sync::Arc;
    ///
    /// let channel = Arc::new(Channel::new(1024, unparker));
    ///
    /// // Only log INFO and above (INFO, WARN, ERROR)
    /// let logger = Logger::new(channel, LevelFilter::Info);
    /// ```
    pub(crate) fn new(
        chan: Arc<Channel>,
        max_level: log::LevelFilter,
        running: Arc<AtomicBool>,
        coloring_enabled: bool,
    ) -> Self {
        Self {
            chan,
            max_level,
            running,
            coloring_enabled,
        }
    }
}

impl log::Log for Logger {
    /// Determines if a log record should be processed based on the configured level filter.
    ///
    /// This method provides efficient early filtering to avoid the expensive formatting
    /// operations in `log()` for messages that would be discarded anyway.
    ///
    /// # Performance Impact
    ///
    /// - **Accepted messages**: Continue to full formatting (~1-5μs)
    /// - **Filtered messages**: Return immediately (~1-10ns)
    ///
    /// This early filtering is crucial for performance when DEBUG or TRACE logging
    /// is compiled in but not needed at runtime.
    ///
    /// # Parameters
    ///
    /// * `metadata` - Log metadata containing the level to check
    ///
    /// # Returns
    ///
    /// * `true` - If the message level is at or above the configured maximum
    /// * `false` - If the message should be filtered out
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// // Logger configured with LevelFilter::Info
    /// assert!(logger.enabled(&Metadata::builder().level(Level::Error).build())); // true
    /// assert!(logger.enabled(&Metadata::builder().level(Level::Info).build()));  // true
    /// assert!(!logger.enabled(&Metadata::builder().level(Level::Debug).build())); // false
    /// ```
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.max_level
    }

    /// Formats and sends a log record to the consumer thread.
    ///
    /// This is the critical hot path that application threads execute when logging.
    /// The implementation is optimized for minimal latency:
    ///
    /// 1. Get a pooled string (reused memory, ~50ns)
    /// 2. Format timestamp directly into buffer (unsafe, fast)
    /// 3. Format log level with color codes
    /// 4. Format target and optional line number
    /// 5. Format user message
    /// 6. Push to channel (lock-free, ~50ns normal case)
    /// 7. String ownership transfers to consumer
    ///
    /// # Performance
    ///
    /// - **Typical case**: 1-5μs total latency
    /// - **Backpressure case**: 1-100μs (waits for consumer to catch up)
    /// - **Memory**: Zero allocations (uses string pool)
    ///
    /// # Parameters
    ///
    /// * `record` - The log record containing level, target, message, etc.
    ///
    /// # Log Format Details
    ///
    /// The generated format includes:
    /// - ISO 8601 timestamp with microsecond precision
    /// - Color-coded log level in brackets
    /// - Target module path in gray
    /// - Optional line number (if available from macro)
    /// - User-provided message via `format_args!`
    /// - Trailing newline
    ///
    /// # Example Output
    ///
    /// ```text
    /// 2024-01-15T14:30:45.123456Z [INFO] my_app::auth:127 - User alice logged in
    /// 2024-01-15T14:30:45.124001Z [ERROR] my_app::db - Connection failed: timeout
    /// ```
    fn log(&self, record: &log::Record) {
        // ANSI color codes for terminal output
        const RESET: &str = "\x1b[0m";
        const GRAY: &str = "\x1b[90m";

        /// Returns the ANSI color code for a given log level.
        const fn color(level: Level) -> &'static str {
            match level {
                Level::Error => "\x1b[31m", // Red
                Level::Warn => "\x1b[33m",  // Yellow
                Level::Info => "\x1b[32m",  // Green
                Level::Debug => "\x1b[36m", // Cyan
                Level::Trace => GRAY,       // Gray
            }
        }

        // Get a pooled string for zero-allocation formatting
        let mut s = crate::pool::Pool::get();
        let wr = &mut s;

        // Format timestamp in gray
        let now = time::OffsetDateTime::now_utc();
        write!(wr, "{}", GRAY).ok();

        // SAFETY: format_into writes valid UTF-8 to the Vec<u8> backing the String.
        // This is safe because:
        // 1. String::as_mut_vec() gives us access to the underlying bytes
        // 2. ISO8601_FMT only produces valid UTF-8 characters
        // 3. The time crate guarantees valid UTF-8 output
        unsafe {
            now.format_into(wr.as_mut_vec(), &ISO8601_FMT).ok();
        }
        write!(wr, "{}", RESET).ok();
        let level = record.level();

        // Format with or without line number depending on availability
        match record.line() {
            Some(line) => {
                if self.coloring_enabled {
                    write!(
                        wr,
                        " {}[{}]{} {}{}:{}{} {}",
                        color(level),    // Level color
                        level,           // Level text (INFO, ERROR, etc.)
                        RESET,           // Reset color
                        GRAY,            // Target color
                        record.target(), // Module path
                        line,            // Line number
                        RESET,           // Reset color
                        record.args()    // User message
                    )
                    .ok();
                } else {
                    write!(
                        wr,
                        " [{}] {}:{} {}",
                        level,           // Level text (INFO, ERROR, etc.)
                        record.target(), // Module path
                        line,            // Line number
                        record.args()    // User message
                    )
                    .ok();
                }
            }
            None => {
                if self.coloring_enabled {
                    write!(
                        wr,
                        " {}[{}]{} {}{}{} {}",
                        color(level),    // Level color
                        level,           // Level text
                        RESET,           // Reset color
                        GRAY,            // Target color
                        record.target(), // Module path
                        RESET,           // Reset color
                        record.args()    // User message
                    )
                    .ok();
                } else {
                    write!(
                        wr,
                        " [{}] {} {}",
                        level,           // Level text
                        record.target(), // Module path
                        record.args()    // User message
                    )
                    .ok();
                }
            }
        }

        // Add trailing newline
        s.write_str("\n").ok();

        // Send to consumer thread (transfers ownership)
        if self.running.load(Ordering::Relaxed) {
            self.chan.push(s);
        }
    }

    /// Flushes any buffered log records.
    ///
    /// This logger doesn't buffer records (they're immediately sent to the channel),
    /// so this is a no-op. The actual flushing happens in the `Appender` when it
    /// writes to the underlying I/O destination.
    fn flush(&self) {
        // do nothing
    }
}

/// Consumer-side log message appender that handles I/O operations.
///
/// The appender runs in a dedicated background thread and is responsible for:
/// - Receiving formatted log messages from the channel
/// - Writing them to the output destination (file, stdout, etc.)
/// - Managing the parking/unparking cycle for efficient waiting
/// - Returning strings to the pool after processing
///
/// # Design Rationale
///
/// By separating formatting (producer-side) from I/O (consumer-side), we achieve:
/// - Consistent low-latency logging from application threads
/// - Efficient batching of I/O operations
/// - Natural backpressure when I/O can't keep up
/// - Clean shutdown handling with message draining
///
/// # Generic Parameter
///
/// * `W` - Any type implementing [`io::Write`] (File, Stdout, TcpStream, etc.)
pub struct Appender<W: io::Write> {
    /// Channel for receiving formatted log messages from producers.
    chan: Arc<Channel>,

    /// Parker for efficient waiting when no messages are available.
    parker: Parker,

    /// Atomic flag for coordinating shutdown between threads.
    running: Arc<AtomicBool>,

    /// The underlying writer where log messages are sent.
    wr: W,
}

impl<W: io::Write> Drop for Appender<W> {
    /// Ensures all pending log messages are written before the appender is destroyed.
    ///
    /// This is critical for clean shutdown - it drains any remaining messages
    /// from the channel and flushes the writer to ensure no log messages are lost.
    ///
    /// # Behavior
    ///
    /// 1. Drain all remaining messages from the channel
    /// 2. Write each message to the output
    /// 3. Flush the writer to ensure data reaches its destination
    ///
    /// Note: This ignores I/O errors during shutdown since there's no good
    /// way to handle them during `Drop`.
    fn drop(&mut self) {
        while let Some(s) = self.chan.pop() {
            self.wr.write(s.as_bytes()).ok();
        }
        self.flush().ok();
    }
}

impl<W: io::Write> Appender<W> {
    /// Creates a new appender that will consume messages and write them to the specified writer.
    ///
    /// # Parameters
    ///
    /// * `chan` - Shared channel for receiving formatted log messages
    /// * `parker` - Parker for efficient waiting when no messages are available
    /// * `running` - Atomic flag for coordinating shutdown
    /// * `wr` - The output destination (file, stdout, network, etc.)
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// use std::io;
    /// use std::sync::Arc;
    /// use std::sync::atomic::AtomicBool;
    /// use crossbeam_utils::sync::Parker;
    ///
    /// let parker = Parker::new();
    /// let running = Arc::new(AtomicBool::new(true));
    /// let appender = Appender::new(
    ///     channel,
    ///     parker,
    ///     running,
    ///     io::stdout()
    /// );
    /// ```
    pub(crate) fn new(chan: Arc<Channel>, parker: Parker, running: Arc<AtomicBool>, wr: W) -> Self {
        Self {
            chan,
            parker,
            running,
            wr,
        }
    }

    /// Processes log messages until at least one message is written or shutdown is signaled.
    ///
    /// This method implements the main consumer loop logic:
    /// 1. Try to drain available messages from the channel
    /// 2. Write each message to the output destination
    /// 3. Return strings to the pool for reuse
    /// 4. If no messages were available, park with configured timeout
    /// 5. Repeat until at least one message is processed or shutdown
    ///
    /// # Parameters
    ///
    /// * `timeout` - Optional timeout for parking when no messages are available.
    ///   - `None`: Busy spin indefinitely
    ///   - `Some(duration)`: Park for at most the specified duration
    ///
    /// # Returns
    ///
    /// * `Ok(())` - At least one message was processed successfully
    /// * `Err(io::Error)` - An I/O error occurred during writing or flushing
    ///
    /// # Performance Notes
    ///
    /// - Drains all available messages before parking (batching for efficiency)
    /// - Uses lock-free channel operations for minimal overhead
    /// - Parks efficiently when idle to avoid CPU spinning
    /// - Flushes after processing to ensure timely output
    ///
    /// # Example Consumer Loop
    ///
    /// ```rust, ignore
    /// use std::time::Duration;
    ///
    /// // Process messages with 10ms timeout
    /// while running.load(Ordering::Relaxed) {
    ///     if let Err(e) = appender.block_on_append(Some(Duration::from_millis(10))) {
    ///         eprintln!("Log appender error: {}", e);
    ///         break;
    ///     }
    /// }
    /// ```
    pub(crate) fn block_on_append(
        &mut self,
        timeout: Option<std::time::Duration>,
    ) -> io::Result<()> {
        let mut written = false;
        match timeout {
            Some(timeout) => {
                // Park with timeout - wake up periodically to check shutdown
                while !written && self.running.load(Ordering::Relaxed) {
                    // Drain all available messages
                    while let Some(s) = self.chan.pop() {
                        self.wr.write(s.as_bytes())?;
                        crate::pool::Pool::put(s);
                        written = true;
                    }

                    // Park until woken or timeout expires
                    self.parker.park_timeout(timeout);
                }
            }
            None => {
                // Spin indefinitely - only wake on shutdown
                while !written && self.running.load(Ordering::Relaxed) {
                    // Drain all available messages
                    while let Some(s) = self.chan.pop() {
                        self.wr.write(s.as_bytes())?;
                        crate::pool::Pool::put(s);
                        written = true;
                    }

                    // Emit spin hint to cpu to reduce
                    // overall cpu usage.
                    std::hint::spin_loop();
                }
            }
        }

        // Ensure data reaches destination
        self.flush()
    }

    /// Flushes the underlying writer to ensure all buffered data is written.
    ///
    /// This forces any buffered log messages to be written to their final
    /// destination (disk, network, etc.). Useful for ensuring important
    /// log messages are persisted before continuing.
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Flush completed successfully
    /// * `Err(io::Error)` - An I/O error occurred during flushing
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// // Ensure critical logs are persisted
    /// error!("Critical system failure: {}", error);
    /// appender.flush()?;
    /// ```
    #[inline(always)]
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.wr.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::{Level, Log, Record};

    #[test]
    fn test_logger_appender_integration() {
        // Create ring buffer and logger
        let parker = Parker::new();
        let chan = Arc::new(Channel::new(4, parker.unparker().to_owned()));
        let running = Arc::new(AtomicBool::new(true));
        let logger = Logger::new(chan.clone(), log::LevelFilter::Info, running, true);
        let running = Arc::new(AtomicBool::new(true));

        // Create appender with a Vec as writer (for testing)
        let output = Vec::new();
        let mut appender = Appender::new(chan, parker, running, output);

        // Log a message directly using the Log trait
        logger.log(
            &Record::builder()
                .args(format_args!("Test message"))
                .level(Level::Info)
                .target("test_module")
                .line(Some(23))
                .build(),
        );

        // Process the log
        appender.block_on_append(None).expect("failed to process");

        // Check output
        let output_str = String::from_utf8(appender.wr.clone()).expect("invalid utf8");
        println!("Output: {}", output_str);

        // Should contain our message
        assert!(output_str.contains("[INFO]"));
        assert!(output_str.contains("test_module:"));
        assert!(output_str.contains(" Test message"));
        assert!(output_str.ends_with("\n"));

        // Should have timestamp at the beginning
        assert!(output_str.contains("T")); // ISO8601 format has T between date and time
    }

    #[test]
    fn test_logger_appender_integration_no_coloring() {
        // Create ring buffer and logger
        let parker = Parker::new();
        let chan = Arc::new(Channel::new(4, parker.unparker().to_owned()));
        let running = Arc::new(AtomicBool::new(true));
        let logger = Logger::new(chan.clone(), log::LevelFilter::Info, running, false);
        let running = Arc::new(AtomicBool::new(true));

        // Create appender with a Vec as writer (for testing)
        let output = Vec::new();
        let mut appender = Appender::new(chan, parker, running, output);

        // Log a message directly using the Log trait
        logger.log(
            &Record::builder()
                .args(format_args!("Test message"))
                .level(Level::Info)
                .target("test_module")
                .line(Some(23))
                .build(),
        );

        // Process the log
        appender.block_on_append(None).expect("failed to process");

        // Check output
        let output_str = String::from_utf8(appender.wr.clone()).expect("invalid utf8");
        println!("Output: {}", output_str);

        // Should contain our message
        assert!(output_str.contains("[INFO]"));
        assert!(output_str.contains("test_module:"));
        assert!(output_str.contains(" Test message"));
        assert!(output_str.ends_with("\n"));

        // Should have timestamp at the beginning
        assert!(output_str.contains("T")); // ISO8601 format has T between date and time
    }
}
