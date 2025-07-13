use std::io::{self, Write};

use crate::{
    notifier::{EventAwaiter, EventNotifier},
    rb::{RingBuffer, RingConsumer},
    writer::LogWriter,
};

const ISO8601_FMT: &[time::format_description::FormatItem<'static>] = time::macros::format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z"
);

pub struct Logger<const N: usize, E: EventNotifier + Sync + Send + 'static> {
    rb: RingBuffer<N, E>,
}

impl<const N: usize, E: EventNotifier + Send + Sync + 'static> Logger<N, E> {
    pub(crate) fn new(rb: RingBuffer<N, E>) -> Self {
        Self { rb }
    }
}

impl<const N: usize, E: EventNotifier + Send + Sync + 'static> log::Log for Logger<N, E> {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let mut buf = [0u8; N];
        let mut wr = LogWriter::new(&mut buf);
        let now = time::OffsetDateTime::now_utc();
        now.format_into(&mut wr, &ISO8601_FMT).ok();
        match record.line() {
            Some(line) => {
                write!(
                    &mut wr,
                    " [{}] {}:{} - {}",
                    record.level(),
                    record.target(),
                    line,
                    record.args()
                )
                .ok();
            }
            None => {
                write!(
                    &mut wr,
                    " [{}] {} - {}",
                    record.level(),
                    record.target(),
                    record.args()
                )
                .ok();
            }
        }

        wr.flush().ok();
        self.rb.publish(buf);
    }

    fn flush(&self) {
        // do nothing
    }
}

pub struct Appender<const N: usize, W: io::Write, A: EventAwaiter> {
    cons: RingConsumer<N, A>,
    wr: W,
}

impl<const N: usize, W: io::Write, A: EventAwaiter> Appender<N, W, A> {
    pub(crate) fn new(cons: RingConsumer<N, A>, wr: W) -> Self {
        Self { cons, wr }
    }

    pub(crate) fn process_one(&mut self, timeout: Option<std::time::Duration>) -> io::Result<bool> {
        match self.cons.receive(timeout) {
            Ok(Some(buf)) => {
                let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
                if len > 0 {
                    self.wr.write_all(&buf[2..2 + len])?;
                    self.wr.write_all(b"\n")?;
                }
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
    use crate::notifier::OsEventAwaiter;

    use super::*;
    use log::{Level, Log, Record};

    #[test]
    fn test_logger_appender_integration() {
        // Create ring buffer and logger
        let awaiter = OsEventAwaiter::new().unwrap();
        let (rb, consumer) =
            RingBuffer::<512, _>::new(awaiter, 10).expect("failed to create buffer");
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
        assert!(output_str.contains("[INFO] test_module:23 - Test message"));
        assert!(output_str.ends_with("\n"));

        // Should have timestamp at the beginning
        assert!(output_str.contains("T")); // ISO8601 format has T between date and time
    }

    #[test]
    fn test_message_truncation() {
        // Small buffer size to test truncation
        let awaiter = OsEventAwaiter::new().unwrap();
        let (rb, consumer) =
            RingBuffer::<128, _>::new(awaiter, 10).expect("failed to create buffer");
        let logger = Logger::new(rb);
        let mut appender = Appender::new(consumer, Vec::new());

        // Log a very long message
        let long_msg = "x".repeat(200);
        logger.log(
            &Record::builder()
                .args(format_args!("{}", long_msg))
                .level(Level::Info)
                .target("test")
                .line(Some(23))
                .build(),
        );

        appender.process_one(None).expect("failed to process");

        let output_str = String::from_utf8(appender.wr).expect("invalid utf8");

        println!("{}", output_str);
        // Should be truncated and have ellipsis
        assert!(output_str.contains("..."));
        assert!(output_str.len() < 200); // Much shorter than original message
    }
}
