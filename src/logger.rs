use std::fmt::Write;
use std::io;

use log::Level;

use crate::rb::{RingBuffer, RingConsumer};

const ISO8601_FMT: &[time::format_description::FormatItem<'static>] = time::macros::format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z"
);

pub struct Logger {
    rb: RingBuffer,
}

impl Logger {
    pub(crate) fn new(rb: RingBuffer) -> Self {
        Self { rb }
    }
}

impl log::Log for Logger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
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

        let mut string = crate::pool::Pool::get();
        let wr = &mut string;
        let now = time::OffsetDateTime::now_utc();
        write!(wr, "{}", GRAY).ok();
        unsafe {
            now.format_into(wr.as_mut_vec(), &ISO8601_FMT).ok();
        }
        write!(wr, "{}", RESET).ok();
        let level = record.level();
        match record.line() {
            Some(line) => {
                write!(
                    wr,
                    " {}[{}]{} {}{}:{}{} - {}",
                    color(level),
                    level,
                    RESET,
                    GRAY,
                    record.target(),
                    line,
                    RESET,
                    record.args()
                )
                .ok();
            }
            None => {
                write!(
                    wr,
                    " {}[{}]{} {}{}{} - {}",
                    color(level),
                    level,
                    RESET,
                    GRAY,
                    record.target(),
                    RESET,
                    record.args()
                )
                .ok();
            }
        }

        string.write_str("\n").ok();
        self.rb.publish(string);
    }

    fn flush(&self) {
        // do nothing
    }
}

pub struct Appender<W: io::Write> {
    cons: RingConsumer,
    wr: W,
}

impl<W: io::Write> Appender<W> {
    pub(crate) fn new(cons: RingConsumer, wr: W) -> Self {
        Self { cons, wr }
    }

    pub(crate) fn process_one(&mut self, timeout: Option<std::time::Duration>) -> io::Result<bool> {
        match self.cons.receive(timeout) {
            Ok(Some(s)) => {
                self.wr.write(s.as_bytes())?;
                crate::pool::Pool::put(s);
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
    use crate::notifier::EventAwaiter;

    use super::*;
    use log::{Level, Log, Record};

    #[test]
    fn test_logger_appender_integration() {
        // Create ring buffer and logger
        let awaiter = EventAwaiter::new().unwrap();
        let (rb, consumer) = RingBuffer::new(awaiter, 10).expect("failed to create buffer");
        let logger = Logger::new(rb);

        // Create appender with a Vec as writer (for testing)
        let output = Vec::new();
        let mut appender = Appender::new(consumer, output);

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
        let processed = appender.process_one(None).expect("failed to process");
        assert!(processed, "Should have processed a message");

        // Check output
        let output_str = String::from_utf8(appender.wr.clone()).expect("invalid utf8");
        println!("Output: {}", output_str);

        // Should contain our message
        assert!(output_str.contains("[INFO]"));
        assert!(output_str.contains("test_module:"));
        assert!(output_str.contains(" - Test message"));
        assert!(output_str.ends_with("\n"));

        // Should have timestamp at the beginning
        assert!(output_str.contains("T")); // ISO8601 format has T between date and time
    }
}
