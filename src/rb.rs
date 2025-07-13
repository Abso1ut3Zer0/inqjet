use crossbeam_queue::ArrayQueue;
use crossbeam_utils::Backoff;
use std::{io, sync::Arc};

use crate::notifier::{EventAwaiter, EventNotifier};

#[derive(Clone)]
pub struct RingBuffer<const N: usize = 512> {
    inner: Arc<ArrayQueue<[u8; N]>>,
    notifier: Arc<EventNotifier>,
}

pub struct RingConsumer<const N: usize> {
    inner: Arc<ArrayQueue<[u8; N]>>,
    awaiter: EventAwaiter,
}

impl<const N: usize> RingBuffer<N> {
    pub fn new(capacity: usize) -> io::Result<(Self, RingConsumer<N>)> {
        assert!(N >= 128, "log slots must be at least 128 bytes");
        let queue = Arc::new(ArrayQueue::new(capacity));
        let (awaiter, notifier) = EventAwaiter::new()?;
        Ok((
            Self {
                inner: queue.clone(),
                notifier: Arc::new(notifier),
            },
            RingConsumer {
                inner: queue,
                awaiter,
            },
        ))
    }

    pub fn publish(&self, mut log: [u8; N]) {
        let backoff = Backoff::new();
        while let Err(returned_log) = self.inner.push(log) {
            log = returned_log;
            backoff.snooze();
        }

        self.notifier.notify();
    }
}

impl<const N: usize> RingConsumer<N> {
    pub fn receive(&mut self, timeout: Option<std::time::Duration>) -> io::Result<Option<[u8; N]>> {
        if let Some(msg) = self.inner.pop() {
            return Ok(Some(msg));
        }

        // No message available, so wait for one
        self.awaiter.wait(timeout)?;
        Ok(self.inner.pop())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn test_basic_publish_receive() {
        println!("\n=== Test: Basic Publish/Receive ===");
        let (buffer, mut consumer) = RingBuffer::<256>::new(4).expect("failed to create buffer");

        // Publish a message
        let mut msg = [0u8; 256];
        msg[..5].copy_from_slice(b"hello");
        println!("Publishing message: 'hello'");
        buffer.publish(msg);

        // Receive it
        let received = consumer
            .receive(None)
            .expect("receive failed")
            .expect("should have message");
        println!(
            "Received message: '{}'",
            std::str::from_utf8(&received[..5]).unwrap()
        );
        assert_eq!(&received[..5], b"hello");
        println!("✓ Test passed\n");
    }

    #[test]
    fn test_event_wakeup_across_threads() {
        println!("\n=== Test: Event Wakeup Across Threads ===");
        let (buffer, mut consumer) = RingBuffer::<256>::new(4).expect("failed to create buffer");
        let counter = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicBool::new(true));

        let counter_clone = counter.clone();
        let running_clone = running.clone();

        // Spawn consumer thread
        let consumer_thread = thread::spawn(move || {
            println!("[Consumer] Thread started");
            while running_clone.load(Ordering::Acquire) {
                match consumer.receive(Some(Duration::from_millis(100))) {
                    Ok(Some(msg)) => {
                        let count = counter_clone.fetch_add(1, Ordering::Release) + 1;
                        println!(
                            "[Consumer] Received message #{} with value: {}",
                            count, msg[0]
                        );
                    }
                    Ok(None) => {
                        println!("[Consumer] Timeout waiting for message");
                    }
                    Err(e) => {
                        println!("[Consumer] Error: {:?}", e);
                        break;
                    }
                }
            }
            println!("[Consumer] Thread exiting");
        });

        // Publish messages from main thread
        thread::sleep(Duration::from_millis(100)); // Let consumer start

        for i in 0..5 {
            let mut msg = [0u8; 256];
            msg[0] = i + 1;
            println!("[Publisher] Publishing message with value: {}", msg[0]);
            buffer.publish(msg);
            thread::sleep(Duration::from_millis(50));
        }

        // Give consumer time to process last messages
        thread::sleep(Duration::from_millis(200));

        // Shutdown
        println!("[Main] Signaling shutdown");
        running.store(false, Ordering::Release);
        buffer.notifier.notify(); // Wake consumer to check shutdown flag

        consumer_thread.join().unwrap();

        let final_count = counter.load(Ordering::Relaxed);
        println!("Total messages processed: {}", final_count);
        assert_eq!(final_count, 5);
        println!("✓ Test passed\n");
    }

    #[test]
    fn test_blocking_when_full() {
        println!("\n=== Test: Blocking When Full ===");
        let (buffer, mut consumer) = RingBuffer::<256>::new(2).expect("failed to create buffer");
        let published = Arc::new(AtomicBool::new(false));
        let published_clone = published.clone();

        // Fill the buffer
        println!("Filling buffer with 2 messages (capacity: 2)");
        buffer.publish([1u8; 256]);
        buffer.publish([2u8; 256]);
        println!("Buffer is now full");

        // This should block
        let publisher = thread::spawn(move || {
            println!("[Publisher] Attempting to publish 3rd message (should block)...");
            let start = Instant::now();
            buffer.publish([3u8; 256]);
            println!(
                "[Publisher] Unblocked after {:.2?}! Message published",
                start.elapsed()
            );
            published_clone.store(true, Ordering::Release);
        });

        // Verify it's blocking
        thread::sleep(Duration::from_millis(200));
        assert!(!published.load(Ordering::Acquire));
        println!("[Main] Verified publisher is blocked");

        // Consumer drains one
        println!("[Main] Consuming one message to make space...");
        let msg = consumer.receive(None).unwrap().unwrap();
        println!("[Main] Consumed message with value: {}", msg[0]);

        // Publisher should unblock
        publisher.join().unwrap();
        assert!(published.load(Ordering::Acquire));
        println!("✓ Test passed\n");
    }

    #[test]
    fn test_multiple_publishers_single_consumer() {
        println!("\n=== Test: Multiple Publishers, Single Consumer ===");
        let (buffer, mut consumer) = RingBuffer::<256>::new(10).expect("failed to create buffer");
        let received = Arc::new(AtomicUsize::new(0));
        let messages_per_thread = 10;
        let num_threads = 3;

        // Spawn multiple publishers
        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let buffer = buffer.clone();
                thread::spawn(move || {
                    println!("[Publisher {}] Starting", thread_id);
                    for i in 0..messages_per_thread {
                        let mut msg = [0u8; 256];
                        msg[0] = thread_id;
                        msg[1] = i;
                        buffer.publish(msg);
                        if i % 3 == 0 {
                            println!("[Publisher {}] Published {} messages", thread_id, i + 1);
                        }
                    }
                    println!("[Publisher {}] Finished", thread_id);
                })
            })
            .collect();

        // Consumer
        let received_clone = received.clone();
        let consumer_thread = thread::spawn(move || {
            println!("[Consumer] Starting");
            let start = Instant::now();
            let expected = num_threads * messages_per_thread;

            while received_clone.load(Ordering::Acquire) < expected as usize {
                match consumer.receive(Some(Duration::from_millis(100))) {
                    Ok(Some(_)) => {
                        let count = received_clone.fetch_add(1, Ordering::Release) + 1;
                        if count % 10 == 0 {
                            println!("[Consumer] Processed {} messages", count);
                        }
                    }
                    Ok(None) => {
                        println!(
                            "[Consumer] Timeout, received so far: {}",
                            received_clone.load(Ordering::Acquire)
                        );
                    }
                    Err(e) => {
                        panic!("Consumer error: {:?}", e);
                    }
                }

                if start.elapsed() > Duration::from_secs(5) {
                    panic!(
                        "Timeout! Only received {} messages",
                        received_clone.load(Ordering::Acquire)
                    );
                }
            }
            println!("[Consumer] Finished");
        });

        // Wait for completion
        for handle in handles {
            handle.join().unwrap();
        }
        consumer_thread.join().unwrap();

        let total = received.load(Ordering::Relaxed);
        println!("Total messages processed: {}", total);
        assert_eq!(total, 30);
        println!("✓ Test passed\n");
    }

    #[test]
    #[should_panic(expected = "log slots must be at least 128 bytes")]
    fn test_minimum_size_assertion() {
        let _ = RingBuffer::<64>::new(10);
    }
}
