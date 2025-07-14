use std::{
    fmt::Display,
    io,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use derive_builder::Builder;

use crate::{
    logger::{Appender, FmtLogger},
    rb::RingBuffer,
};

mod logger;
pub(crate) mod notifier;
pub(crate) mod rb;
pub(crate) mod writer;

pub use notifier::{EventAwaiter, EventNotifier};
pub use writer::{CtxWriter, format_ctx};

const DEFAULT_CAPACITY: usize = 256;
static LOGGER: OnceLock<Box<dyn Logger + Send + Sync + 'static>> = OnceLock::new();

/// An enum representing the available verbosity levels of the logger.
#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Level {
    /// The "error" level.
    ///
    /// Designates very serious errors.
    // This way these line up with the discriminants for LevelFilter below
    // This works because Rust treats field-less enums the same way as C does:
    // https://doc.rust-lang.org/reference/items/enumerations.html#custom-discriminant-values-for-field-less-enumerations
    Error = 1,
    /// The "warn" level.
    ///
    /// Designates hazardous situations.
    Warn,
    /// The "info" level.
    ///
    /// Designates useful information.
    Info,
    /// The "debug" level.
    ///
    /// Designates lower priority information.
    Debug,
    /// The "trace" level.
    ///
    /// Designates very low priority, often extremely verbose, information.
    Trace,
}

impl Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Level::Error => "ERROR".fmt(f),
            Level::Warn => "WARN".fmt(f),
            Level::Info => "INFO".fmt(f),
            Level::Debug => "DEBUG".fmt(f),
            Level::Trace => "TRACE".fmt(f),
        }
    }
}

#[derive(Debug, Builder)]
pub struct LogRecord<'a> {
    level: Level,
    path: &'static str,
    line: u32,
    msg: &'static str,
    args: std::fmt::Arguments<'a>,
}

#[derive(Debug, Builder)]
pub struct FmtRecord<const N: usize> {
    level: Level,
    path: &'static str,
    line: u32,
    msg: &'static str,
    ctx: [u8; N],
}

impl<const N: usize> From<LogRecord<'_>> for FmtRecord<N> {
    fn from(value: LogRecord) -> Self {
        Self {
            level: value.level,
            path: value.path,
            line: value.line,
            msg: value.msg,
            ctx: format_ctx(value.args),
        }
    }
}

pub trait Logger {
    fn log(&self, record: LogRecord<'_>);
}

pub struct InqJetGuard {
    handle: Option<std::thread::JoinHandle<()>>,
    flag: Arc<AtomicBool>,
    notifier: EventNotifier,
}

impl Drop for InqJetGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
        self.notifier.notify(); // tell background thread to wakeup
        self.handle.take().map(|handle| {
            let _ = handle.join();
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InqJetBuilderError {
    #[error("no configured writer for appender")]
    NoConfiguredWriter,
    #[error("no configured log level")]
    NoConfiguredLogLevel,
    #[error("io error: {0}")]
    IoError(#[from] io::Error),
    #[error("logger already set")]
    LoggerAlreadySet,
}

pub struct InqJetBuilder<W, const N: usize = 512> {
    wr: Option<W>,
    level: Option<Level>,
    cap: Option<usize>,
}

impl<W> InqJetBuilder<W, 512>
where
    W: io::Write + Send + 'static,
{
    pub fn with_normal_slots() -> Self {
        InqJetBuilder::<_, 512>::with_custom_slots()
    }
}

impl<W> InqJetBuilder<W, 256>
where
    W: io::Write + Send + 'static,
{
    pub fn with_small_slots() -> Self {
        InqJetBuilder::<_, 256>::with_custom_slots()
    }
}

impl<W> InqJetBuilder<W, 1024>
where
    W: io::Write + Send + 'static,
{
    pub fn with_large_slots() -> Self {
        InqJetBuilder::<_, 1024>::with_custom_slots()
    }
}

impl<W, const N: usize> InqJetBuilder<W, N>
where
    W: io::Write + Send + 'static,
{
    pub fn with_custom_slots() -> Self {
        // Have to configure via type generic yourself!
        Self {
            wr: None,
            level: None,
            cap: None,
        }
    }

    pub fn with_writer(mut self, wr: W) -> Self {
        self.wr = Some(wr);
        self
    }

    pub fn with_capacity(mut self, cap: usize) -> Self {
        self.cap = Some(cap);
        self
    }

    pub fn with_log_level(mut self, level: Level) -> Self {
        self.level = Some(level);
        self
    }

    pub fn build(self) -> Result<InqJetGuard, InqJetBuilderError> {
        let capacity = self.cap.unwrap_or(DEFAULT_CAPACITY);
        let writer = self.wr.ok_or(InqJetBuilderError::NoConfiguredWriter)?;
        let level = self.level.ok_or(InqJetBuilderError::NoConfiguredLogLevel)?;
        let awaiter = EventAwaiter::new()?;

        let (rb, cons) = RingBuffer::<N>::new(awaiter, capacity)?;
        let notifier = rb.notifier();
        let logger = FmtLogger::new(rb, level);
        let mut appender = Appender::new(cons, writer);
        let flag = Arc::new(AtomicBool::new(true));
        LOGGER
            .set(Box::new(logger))
            .map_err(|_| InqJetBuilderError::LoggerAlreadySet)?;

        let running = flag.clone();
        let handle = std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let _ = appender.process_one(None);
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

#[inline(always)]
pub fn try_log(record: LogRecord<'_>) {
    if let Some(logger) = LOGGER.get() {
        logger.log(record);
    }
}

#[macro_export]
macro_rules! log {
    ($level:expr, $msg:expr) => {{
        $crate::try_log($crate::LogRecord {
            level: $level,
            path: module_path!(),
            line: line!(),
            msg: $msg,
            args: format_args!(""),
        });
    }};

    ($level:expr, $msg:expr, $($arg:tt)*) => {{
        $crate::try_log($crate::LogRecord {
            level: $level,
            path: module_path!(),
            line: line!(),
            msg: $msg,
            args: format_args!($($arg)*),
        });
    }};

    ($level:expr, $($arg:tt)*) => {{
        $crate::try_log($crate::LogRecord {
            level: $level,
            path: module_path!(),
            line: line!(),
            msg: "",
            args: format_args!($($arg)*),
        });
    }};
}

#[macro_export]
macro_rules! info {
    // Static message only
    ($msg:literal) => {
        $crate::log!($crate::Level::Info, $msg)
    };
    // Static message + named context
    ($msg:literal, ctx: $($arg:tt)*) => {
        $crate::log!($crate::Level::Info, $msg, $($arg)*)
    };
    // Everything else - fully dynamic
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Info, $($arg)*)
    };
}

#[macro_export]
macro_rules! warn {
    // Static message only
    ($msg:literal) => {
        $crate::log!($crate::Level::Warn, $msg)
    };
    // Static message + named context
    ($msg:literal, ctx: $($arg:tt)*) => {
        $crate::log!($crate::Level::Warn, $msg, $($arg)*)
    };
    // Everything else - fully dynamic
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Warn, $($arg)*)
    };
}

#[macro_export]
macro_rules! error {
    // Static message only
    ($msg:literal) => {
        $crate::log!($crate::Level::Error, $msg)
    };
    // Static message + named context
    ($msg:literal, ctx: $($arg:tt)*) => {
        $crate::log!($crate::Level::Error, $msg, $($arg)*)
    };
    // Everything else - fully dynamic
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Error, $($arg)*)
    };
}

#[macro_export]
macro_rules! debug {
    // Static message only
    ($msg:literal) => {
        $crate::log!($crate::Level::Debug, $msg)
    };
    // Static message + named context
    ($msg:literal, ctx: $($arg:tt)*) => {
        $crate::log!($crate::Level::Debug, $msg, $($arg)*)
    };
    // Everything else - fully dynamic
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Debug, $($arg)*)
    };
}

#[macro_export]
macro_rules! trace {
    // Static message only
    ($msg:literal) => {
        $crate::log!($crate::Level::Trace, $msg)
    };
    // Static message + named context
    ($msg:literal, ctx: $($arg:tt)*) => {
        $crate::log!($crate::Level::Trace, $msg, $($arg)*)
    };
    // Everything else - fully dynamic
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Trace, $($arg)*)
    };
}

#[cfg(test)]
mod tests {
    use tracing::Level;
    use tracing_log::LogTracer;
    use tracing_subscriber::fmt;

    use super::*;

    // #[ignore]
    #[test]
    fn bench_inqjet() {
        let _guard = InqJetBuilder::with_normal_slots()
            .with_writer(io::stdout())
            .with_log_level(crate::Level::Info)
            .build()
            .unwrap();

        let n = 1_000;
        let duration = std::time::Duration::from_millis(5);
        let mut total = 0;
        for i in 0..n {
            std::thread::sleep(duration);
            let now = std::time::Instant::now();
            // crate::info!("logging to inqjet logger!", "msg number: {}", i);
            crate::info!("logging to inqjet logger! msg number: {}");
            let elapsed = now.elapsed();
            total += elapsed.as_nanos();
        }

        let total = std::time::Duration::from_nanos(total as u64);
        println!("InqJet (EventFD) total performance: {:?}", total);
    }

    // #[ignore]
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
            tracing::info!("logging to tracing logger! msg number: {}", i);
            let elapsed = now.elapsed();
            total += elapsed.as_nanos();
        }

        let total = std::time::Duration::from_nanos(total as u64);
        println!("Tracing total performance: {:?}", total);
    }
}
