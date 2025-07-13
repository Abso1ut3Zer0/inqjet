use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

pub trait EventNotifier {
    fn notify(&self);
}

pub trait EventAwaiter {
    type Notifier: EventNotifier;

    fn wait(&mut self, timeout: Option<std::time::Duration>) -> io::Result<()>;

    fn notifier(&self) -> Arc<Self::Notifier>;
}

pub struct OsEventNotifier(mio::Waker);

impl EventNotifier for OsEventNotifier {
    fn notify(&self) {
        let _ = self.0.wake();
    }
}

pub struct OsEventAwaiter {
    poll: mio::Poll,
    notifier: Arc<OsEventNotifier>,
}

impl OsEventAwaiter {
    pub fn new() -> io::Result<Self> {
        let poll = mio::Poll::new()?;
        let waker = mio::Waker::new(poll.registry(), mio::Token(0))?;
        Ok(Self {
            poll,
            notifier: Arc::new(OsEventNotifier(waker)),
        })
    }
}

impl EventAwaiter for OsEventAwaiter {
    type Notifier = OsEventNotifier;

    fn wait(&mut self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        let mut events = mio::Events::with_capacity(1);
        self.poll.poll(&mut events, timeout)?;
        Ok(())
    }

    fn notifier(&self) -> Arc<Self::Notifier> {
        self.notifier.clone()
    }
}

impl EventNotifier for AtomicUsize {
    fn notify(&self) {
        self.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct SpinEventAwaiter(Arc<AtomicUsize>);

impl SpinEventAwaiter {
    pub fn new() -> Self {
        let counter = Arc::new(AtomicUsize::default());
        Self(counter)
    }
}

impl EventAwaiter for SpinEventAwaiter {
    type Notifier = AtomicUsize;

    fn wait(&mut self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        let now = std::time::Instant::now();
        let backoff = crossbeam_utils::Backoff::new();
        loop {
            let counter = self.0.load(Ordering::Acquire);

            if counter > 0 {
                self.0.fetch_sub(1, Ordering::Release);
                return Ok(());
            }

            match timeout {
                Some(timeout) => {
                    if now.elapsed() >= timeout {
                        return Ok(());
                    }
                }
                None => backoff.snooze(),
            }
        }
    }

    fn notifier(&self) -> Arc<Self::Notifier> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn test_os_waker() {
        let mut awaiter = OsEventAwaiter::new().expect("could not initialize event awaiter");
        let notifier = awaiter.notifier();

        let flag = Arc::new(AtomicBool::new(true));
        let counter = Arc::new(AtomicUsize::new(0));

        let running = flag.clone();
        let cloned = counter.clone();
        let mutex = Arc::new(std::sync::Mutex::new(()));
        let mutex_cloned = mutex.clone();
        let _guard = std::thread::spawn(move || {
            let _guard = mutex_cloned.lock().unwrap();
            while running.load(Ordering::Acquire) {
                awaiter.wait(None).expect("could not await event");
                println!("received event!");
                cloned.fetch_add(1, Ordering::Release);
            }
        });

        for i in 0..3 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if i == 2 {
                flag.store(false, Ordering::Release);
            }

            notifier.notify();
        }

        let _guard = mutex.lock().unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_spin_waker() {
        let mut awaiter = SpinEventAwaiter::new();
        let notifier = awaiter.notifier();
        let flag = Arc::new(AtomicBool::new(true));
        let counter = Arc::new(AtomicUsize::new(0));

        let running = flag.clone();
        let cloned = counter.clone();
        let mutex = Arc::new(std::sync::Mutex::new(()));
        let mutex_cloned = mutex.clone();
        let _guard = std::thread::spawn(move || {
            let _guard = mutex_cloned.lock().unwrap();
            while running.load(Ordering::Acquire) {
                let _ = awaiter.wait(None);
                println!("received event!");
                cloned.fetch_add(1, Ordering::Release);
            }
        });

        for i in 0..3 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if i == 2 {
                flag.store(false, Ordering::Release);
            }

            notifier.notify();
        }

        let _guard = mutex.lock().unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }
}
