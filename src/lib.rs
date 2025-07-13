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
    notifier::EventNotifier,
    rb::RingBuffer,
};

mod logger;
pub(crate) mod notifier;
pub(crate) mod rb;
pub(crate) mod writer;

const DEFAULT_CAPACITY: usize = 256;

pub struct InqJetGuard {
    handle: Option<std::thread::JoinHandle<()>>,
    flag: Arc<AtomicBool>,
    notifier: Arc<EventNotifier>,
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

pub struct InqJetBuilder<W: io::Write, const N: usize = 512> {
    wr: Option<W>,
    level: Option<LevelFilter>,
    cap: Option<usize>,
}

impl<W: io::Write + Send + 'static> InqJetBuilder<W, 512> {
    pub fn with_normal_slots() -> Self {
        InqJetBuilder::<_, 512>::with_custom_slots()
    }
}

impl<W: io::Write + Send + 'static> InqJetBuilder<W, 256> {
    pub fn with_small_slots() -> Self {
        InqJetBuilder::<_, 256>::with_custom_slots()
    }
}

impl<W: io::Write + Send + 'static> InqJetBuilder<W, 1024> {
    pub fn with_large_slots() -> Self {
        InqJetBuilder::<_, 1024>::with_custom_slots()
    }
}

impl<W: io::Write + Send + 'static, const N: usize> InqJetBuilder<W, N> {
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

        let (rb, cons) = RingBuffer::<N>::new(capacity)?;
        let notifier = rb.notifier();
        let logger = Logger::new(rb, level);
        let mut appender = Appender::new(cons, writer);
        let flag = Arc::new(AtomicBool::new(true));
        log::set_boxed_logger(Box::new(logger))?;

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
    use std::io::BufWriter;

    use super::*;

    #[test]
    fn test_inqjet_logger() {
        let stdout = io::stdout();
        let writer = BufWriter::new(stdout);
        let _guard = InqJetBuilder::with_normal_slots()
            .with_writer(writer)
            .with_log_level(LevelFilter::Info)
            .build()
            .unwrap();

        for i in 0..10 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            log::info!("logging to inqjet logger! msg number: {}", i);
        }
    }
}
