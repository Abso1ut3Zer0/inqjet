use crossbeam_queue::ArrayQueue;
use crossbeam_utils::Backoff;
use std::{io, sync::Arc};

use crate::notifier::{EventAwaiter, EventNotifier};

pub struct RingBuffer {
    inner: Arc<ArrayQueue<String>>,
    notifier: EventNotifier,
}

impl Clone for RingBuffer {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            notifier: self.notifier.clone(),
        }
    }
}

impl RingBuffer {
    pub(crate) fn new(awaiter: EventAwaiter, capacity: usize) -> io::Result<(Self, RingConsumer)> {
        let queue = Arc::new(ArrayQueue::new(capacity));
        Ok((
            Self {
                inner: queue.clone(),
                notifier: awaiter.notifier(),
            },
            RingConsumer {
                inner: queue,
                awaiter,
            },
        ))
    }

    pub(crate) fn notifier(&self) -> EventNotifier {
        self.notifier.clone()
    }

    pub(crate) fn publish(&self, mut log: String) {
        let backoff = Backoff::new();
        while let Err(returned_log) = self.inner.push(log) {
            log = returned_log;
            backoff.snooze();
        }

        self.notifier.notify();
    }
}

pub struct RingConsumer {
    inner: Arc<ArrayQueue<String>>,
    awaiter: EventAwaiter,
}

impl RingConsumer {
    pub(crate) fn receive(
        &mut self,
        timeout: Option<std::time::Duration>,
    ) -> io::Result<Option<String>> {
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
    use crate::notifier::EventAwaiter;

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
        let awaiter = EventAwaiter::new().unwrap();
        let (buffer, mut consumer) = RingBuffer::new(awaiter, 4).expect("failed to create buffer");

        // Publish a message
        let msg = "hello".to_string();
        println!("Publishing message: 'hello'");
        buffer.publish(msg);

        // Receive it
        let received = consumer
            .receive(None)
            .expect("receive failed")
            .expect("should have message");
        println!("Received message: '{}'", received,);
        assert_eq!(received.as_str(), "hello");
        println!("✓ Test passed\n");
    }

    #[test]
    fn test_event_wakeup_across_threads() {
        println!("\n=== Test: Event Wakeup Across Threads ===");
        let awaiter = EventAwaiter::new().unwrap();
        let (buffer, mut consumer) = RingBuffer::new(awaiter, 4).expect("failed to create buffer");
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
                        println!("[Consumer] Received message #{} with value: {}", count, msg);
                    }
                    Ok(None) => {
                        println!("[Consumer] Timeout waiting for message");
                    }
                    Err(e) => {
                        println!("[Cosumer] Error: {:?}", e);
                        break;
                    }
                }
            }
            println!("[Consumer] Thread exiting");
        });

        // Publish messages from main thread
        thread::sleep(Duration::from_millis(100)); // Let consumer start

        for i in 0..5 {
            let msg = format!("{}", i + 1);
            println!("[Publisher] Publishing message with value: {}", msg);
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
        let awaiter = EventAwaiter::new().unwrap();
        let (buffer, mut consumer) = RingBuffer::new(awaiter, 2).expect("failed to create buffer");
        let published = Arc::new(AtomicBool::new(false));
        let published_clone = published.clone();

        // Fill the buffer
        println!("Filling buffer with 2 messages (capacity: 2)");
        buffer.publish("first".to_string());
        buffer.publish("second".to_string());
        println!("Buffer is now full");

        // This should block
        let publisher = thread::spawn(move || {
            println!("[Publisher] Attempting to publish 3rd message (should block)...");
            let start = Instant::now();
            buffer.publish("third".to_string());
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
        println!("[Main] Consumed message with value: {}", msg);

        // Publisher should unblock
        publisher.join().unwrap();
        assert!(published.load(Ordering::Acquire));
        println!("✓ Test passed\n");
    }

    #[test]
    fn test_multiple_publishers_single_consumer() {
        println!("\n=== Test: Multiple Publishers, Single Consumer ===");
        let awaiter = EventAwaiter::new().unwrap();
        let (buffer, mut consumer) = RingBuffer::new(awaiter, 4).expect("failed to create buffer");
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
                        let msg = format!("{}, {}", thread_id, i);
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
}
