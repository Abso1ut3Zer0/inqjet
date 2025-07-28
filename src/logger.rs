use std::io;
use std::{fmt::Write, sync::Arc};

use crossbeam_utils::sync::Parker;
use log::Level;

use crate::channel::Channel;

const ISO8601_FMT: &[time::format_description::FormatItem<'static>] = time::macros::format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z"
);

pub struct Logger {
    chan: Arc<Channel>,
}

impl Logger {
    pub(crate) fn new(chan: Arc<Channel>) -> Self {
        Self { chan }
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

        let mut s = crate::pool::Pool::get();
        let wr = &mut s;
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

        s.write_str("\n").ok();
        self.chan.push(s);
    }

    fn flush(&self) {
        // do nothing
    }
}

pub struct Appender<W: io::Write> {
    chan: Arc<Channel>,
    parker: Parker,
    wr: W,
}

impl<W: io::Write> Appender<W> {
    pub(crate) fn new(chan: Arc<Channel>, parker: Parker, wr: W) -> Self {
        Self { chan, parker, wr }
    }

    pub(crate) fn block_on_append(
        &mut self,
        timeout: Option<std::time::Duration>,
    ) -> io::Result<()> {
        let mut written = false;
        match timeout {
            Some(timeout) => loop {
                while let Some(s) = self.chan.pop() {
                    self.wr.write(s.as_bytes())?;
                    crate::pool::Pool::put(s);
                    written = true;
                }

                if written {
                    return self.wr.flush();
                }

                self.parker.park_timeout(timeout);
            },
            None => loop {
                while let Some(s) = self.chan.pop() {
                    self.wr.write(s.as_bytes())?;
                    crate::pool::Pool::put(s);
                    written = true;
                }

                if written {
                    return self.wr.flush();
                }
            },
        }
    }

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
        let logger = Logger::new(chan.clone());

        // Create appender with a Vec as writer (for testing)
        let output = Vec::new();
        let mut appender = Appender::new(chan, parker, output);

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
        assert!(output_str.contains(" - Test message"));
        assert!(output_str.ends_with("\n"));

        // Should have timestamp at the beginning
        assert!(output_str.contains("T")); // ISO8601 format has T between date and time
    }
}
