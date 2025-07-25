use std::{io, sync::Arc};

#[derive(Clone)]
pub struct EventNotifier(Arc<mio::Waker>);

impl EventNotifier {
    #[inline(always)]
    pub fn notify(&self) {
        let _ = self.0.wake();
    }
}

pub struct EventAwaiter {
    poll: mio::Poll,
    notifier: EventNotifier,
}

impl EventAwaiter {
    pub fn new() -> io::Result<Self> {
        let poll = mio::Poll::new()?;
        let waker = mio::Waker::new(poll.registry(), mio::Token(0))?;
        Ok(Self {
            poll,
            notifier: EventNotifier(Arc::new(waker)),
        })
    }

    #[inline(always)]
    pub fn wait(&mut self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        let mut events = mio::Events::with_capacity(1);
        self.poll.poll(&mut events, timeout)?;
        Ok(())
    }

    pub fn notifier(&self) -> EventNotifier {
        self.notifier.clone()
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
    fn test_waker() {
        let mut awaiter = EventAwaiter::new().expect("could not initialize event awaiter");
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
}
