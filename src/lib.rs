use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use log::LevelFilter;

use crate::{
    logger::{Appender, Logger},
    rb::RingBuffer,
};

mod logger;
pub(crate) mod notifier;
pub(crate) mod rb;
pub(crate) mod writer;

pub use notifier::{EventAwaiter, EventNotifier};

const DEFAULT_CAPACITY: usize = 256;

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
    #[error("{0}")]
    SetLoggerError(#[from] log::SetLoggerError),
}

pub struct InqJetBuilder<W, const N: usize = 512> {
    wr: Option<W>,
    level: Option<LevelFilter>,
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

    pub fn with_log_level(mut self, level: LevelFilter) -> Self {
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
        let logger = Logger::new(rb);
        let mut appender = Appender::new(cons, writer);
        let flag = Arc::new(AtomicBool::new(true));
        log::set_boxed_logger(Box::new(logger))?;
        log::set_max_level(level);

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

#[cfg(test)]
mod tests {
    use tracing::Level;
    use tracing_log::LogTracer;
    use tracing_subscriber::fmt;

    use super::*;

    #[ignore]
    #[test]
    fn bench_inqjet() {
        let _guard = InqJetBuilder::with_normal_slots()
            .with_writer(io::stdout())
            .with_log_level(LevelFilter::Info)
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
        println!("InqJet (EventFD) total performance: {:?}", total);
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
