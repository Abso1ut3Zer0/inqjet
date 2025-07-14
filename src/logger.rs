use std::io;

use crate::{
    FmtRecord, Level, LogRecord, Logger,
    rb::{RingBuffer, RingConsumer},
};

const ISO8601_FMT: &[time::format_description::FormatItem<'static>] = time::macros::format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z"
);

pub struct FmtLogger<const N: usize> {
    rb: RingBuffer<N>,
    level: Level,
}

impl<const N: usize> Logger for FmtLogger<N> {
    fn log(&self, record: LogRecord<'_>) {
        self.publish_log(record.into());
    }
}

impl<const N: usize> FmtLogger<N> {
    pub(crate) fn new(rb: RingBuffer<N>, level: Level) -> Self {
        Self { rb, level }
    }

    #[inline(always)]
    pub(crate) fn publish_log(&self, record: FmtRecord<N>) {
        if record.level <= self.level {
            self.rb.publish(record);
        }
    }
}

pub struct Appender<const N: usize, W: io::Write> {
    cons: RingConsumer<N>,
    wr: W,
}

impl<const N: usize, W: io::Write> Appender<N, W> {
    pub(crate) fn new(cons: RingConsumer<N>, wr: W) -> Self {
        Self { cons, wr }
    }

    pub(crate) fn process_one(&mut self, timeout: Option<std::time::Duration>) -> io::Result<bool> {
        const RESET: &str = "\x1b[0m";
        const GRAY: &str = "\x1b[90m";
        const fn color(level: Level) -> &'static str {
            match level {
                Level::Error => "\x1b[31m",
                Level::Warn => "\x1b[33m",
                Level::Info => "\x1b[32m",
                Level::Debug => "\x1b[36m",
                Level::Trace => GRAY,
            }
        }

        match self.cons.receive(timeout) {
            Ok(Some(record)) => {
                write!(&mut self.wr, "{}", GRAY).ok();
                let now = time::OffsetDateTime::now_utc();
                now.format_into(&mut self.wr, &ISO8601_FMT).ok();
                write!(&mut self.wr, "{}", RESET).ok();
                write!(
                    &mut self.wr,
                    " {}[{}]{} {}{}:{}{} - {} ",
                    color(record.level),
                    record.level,
                    RESET,
                    GRAY,
                    record.path,
                    record.line,
                    RESET,
                    record.msg,
                )
                .ok();
                let len = u16::from_le_bytes([record.ctx[0], record.ctx[1]]) as usize;
                if len > 0 {
                    self.wr.write_all(&record.ctx[2..2 + len])?;
                }
                self.wr.write_all(b"\n")?;
                Ok(true)
            }
            Ok(None) => Ok(false), // spurious wakeup
            Err(err) => Err(err),
        }
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.wr.flush()
    }
}

#[cfg(test)]
mod tests {
    use crate::{FmtRecordBuilder, format_ctx, notifier::EventAwaiter};

    use super::*;

    #[test]
    fn test_logger_appender_integration() {
        // Create ring buffer and logger
        let awaiter = EventAwaiter::new().unwrap();
        let (rb, consumer) = RingBuffer::<512>::new(awaiter, 10).expect("failed to create buffer");
        let logger = FmtLogger::new(rb, Level::Info);

        // Create appender with a Vec as writer (for testing)
        let output = Vec::new();
        let mut appender = Appender::new(consumer, output);

        // Log a message directly using the Log trait
        logger.publish_log(
            FmtRecordBuilder::default()
                .level(Level::Info)
                .path("path::to::module")
                .line(32)
                .msg("Test message")
                .ctx(format_ctx!("Test context"))
                .build()
                .unwrap(),
        );

        // Process the log
        let processed = appender.process_one(None).expect("failed to process");
        assert!(processed, "Should have processed a message");

        // Check output
        let output_str = String::from_utf8(appender.wr.clone()).expect("invalid utf8");
        println!("Output: {}", output_str);

        // Should contain our message
        assert!(output_str.contains("[INFO]"));
        assert!(output_str.contains("path::to::module"));
        assert!(output_str.contains(" - Test message: "));
        assert!(output_str.contains("Test context"));
        assert!(output_str.ends_with("\n"));

        // Should have timestamp at the beginning
        assert!(output_str.contains("T")); // ISO8601 format has T between date and time
    }

    #[test]
    fn test_message_truncation() {
        // Small buffer size to test truncation
        let awaiter = EventAwaiter::new().unwrap();
        let (rb, consumer) = RingBuffer::<128>::new(awaiter, 10).expect("failed to create buffer");
        let logger = FmtLogger::new(rb, Level::Info);
        let mut appender = Appender::new(consumer, Vec::new());

        // Log a very long message
        let long_msg = "x".repeat(500);
        logger.publish_log(
            FmtRecordBuilder::default()
                .level(Level::Info)
                .path("path::to::module")
                .line(32)
                .ctx(format_ctx!("{}", long_msg))
                .build()
                .unwrap(),
        );

        appender.process_one(None).expect("failed to process");

        let output_str = String::from_utf8(appender.wr.clone()).expect("invalid utf8");
        println!("Output (len: {}): {}", output_str.len(), output_str);

        // Should be truncated and have ellipsis
        assert!(output_str.contains("..."));
        assert!(output_str.len() < 500); // Much shorter than original message
    }

    #[test]
    fn test_logger_filter() {
        // Create ring buffer and logger
        let awaiter = EventAwaiter::new().unwrap();
        let (rb, consumer) = RingBuffer::<512>::new(awaiter, 10).expect("failed to create buffer");
        let logger = FmtLogger::new(rb, Level::Info);

        // Create appender with a Vec as writer (for testing)
        let output = Vec::new();
        let mut appender = Appender::new(consumer, output);

        // Log a message directly using the Log trait
        logger.publish_log(
            FmtRecordBuilder::default()
                .level(Level::Trace)
                .path("path::to::module")
                .line(32)
                .ctx(format_ctx!("Test message"))
                .build()
                .unwrap(),
        );

        // Process the log
        let processed = appender
            .process_one(Some(std::time::Duration::from_secs(1)))
            .expect("failed to process");
        assert!(!processed, "Should not have processed a message");

        logger.publish_log(
            FmtRecordBuilder::default()
                .level(Level::Info)
                .path("path::to::module")
                .line(32)
                .ctx(format_ctx!("Test message"))
                .build()
                .unwrap(),
        );

        // Process the log
        let processed = appender
            .process_one(Some(std::time::Duration::from_secs(1)))
            .expect("failed to process");
        assert!(processed, "Should have processed a message");
    }
}
