use crossbeam_queue::ArrayQueue;
use crossbeam_utils::{Backoff, sync::Unparker};

pub(crate) struct Channel {
    rb: ArrayQueue<String>,
    unparker: Unparker,
}

impl Channel {
    pub(crate) fn new(capacity: usize, unparker: Unparker) -> Self {
        Self {
            rb: ArrayQueue::new(capacity),
            unparker,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&self, mut log: String) {
        let backoff = Backoff::new();

        while let Err(returned_log) = self.rb.push(log) {
            log = returned_log;
            self.unparker.unpark();
            backoff.spin();
        }
    }

    #[inline(always)]
    pub(crate) fn pop(&self) -> Option<String> {
        self.rb.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_utils::sync::Parker;
    use serial_test::serial;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    #[serial]
    fn test_basic_publish_receive() {
        println!("\n=== Test 1: Basic Publish/Receive ===");

        let parker = Parker::new();
        let unparker = parker.unparker().clone();
        let chan = Channel::new(4, unparker);

        // Publish a message
        println!("Publishing message: 'hello'");
        chan.push("hello".to_string());

        // Receive it
        let received = chan.pop().expect("should have message");
        println!("Received message: '{}'", received);
        assert_eq!(received.as_str(), "hello");
        println!("✓ Test passed\n");
    }

    #[test]
    #[serial]
    fn test_event_wakeup_across_threads() {
        println!("\n=== Test 2: Event Wakeup Across Threads ===");

        let parker = Parker::new();
        let unparker = parker.unparker().clone();
        let chan = Arc::new(Channel::new(4, unparker));

        let counter = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicBool::new(true));

        let counter_clone = counter.clone();
        let running_clone = running.clone();

        // Spawn consumer thread
        let cons = chan.clone();
        let consumer_thread = thread::spawn(move || {
            println!("[Consumer] Thread started");
            while running_clone.load(Ordering::Acquire) {
                if let Some(msg) = cons.pop() {
                    let count = counter_clone.fetch_add(1, Ordering::Release) + 1;
                    println!("[Consumer] Received message #{} with value: {}", count, msg);
                } else {
                    // No message, park with timeout
                    parker.park_timeout(Duration::from_millis(100));
                }
            }
            println!("[Consumer] Thread exiting");
        });

        // Publish messages from main thread
        thread::sleep(Duration::from_millis(100)); // Let consumer start

        for i in 0..5 {
            let msg = format!("{}", i + 1);
            println!("[Publisher] Publishing message with value: {}", msg);
            chan.push(msg);
            thread::sleep(Duration::from_millis(50));
        }

        // Give consumer time to process last messages
        thread::sleep(Duration::from_millis(200));

        // Shutdown
        println!("[Main] Signaling shutdown");
        running.store(false, Ordering::Release);
        // Wake consumer to check shutdown flag
        chan.unparker.unpark();

        consumer_thread.join().unwrap();

        let final_count = counter.load(Ordering::Relaxed);
        println!("Total messages processed: {}", final_count);
        assert_eq!(final_count, 5);
        println!("✓ Test passed\n");
    }

    #[test]
    #[serial]
    fn test_blocking_when_full() {
        println!("\n=== Test 3: Blocking When Full ===");

        let parker = Parker::new();
        let unparker = parker.unparker().clone();
        let chan = Arc::new(Channel::new(4, unparker));

        // Fill the buffer
        println!("Filling buffer with 2 messages (capacity: 2)");
        chan.push("first".to_string());
        chan.push("second".to_string());
        chan.push("third".to_string());
        chan.push("fourth".to_string());
        println!("Buffer is now full");

        let cons = chan.clone();

        // This should block (spin with backoff)
        let publisher = thread::spawn(move || {
            println!("[Publisher] Attempting to publish 3rd message (should block/spin)...");
            let start = Instant::now();
            chan.push("fifth".to_string());
            println!(
                "[Publisher] Unblocked after {:.2?}! Message published",
                start.elapsed()
            );
        });

        // Verify it's blocking/spinning
        parker.park();
        println!("[Main] Verified publisher unparked consumer");

        // Consumer drains one
        println!("[Main] Consuming one message to make space...");
        let msg = cons.pop().unwrap();
        println!("[Main] Consumed message with value: {}", msg);

        // Publisher should unblock
        publisher.join().unwrap();
        println!("✓ Test passed\n");
    }

    #[test]
    #[serial]
    fn test_multiple_publishers_single_consumer() {
        println!("\n=== Test 4: Multiple Publishers, Single Consumer ===");

        let parker = Parker::new();
        let unparker = parker.unparker().clone();
        let chan = Arc::new(Channel::new(4, unparker));
        let cons = chan.clone();

        let received = Arc::new(AtomicUsize::new(0));
        let messages_per_thread = 10;
        let num_threads = 3;

        // Spawn multiple publishers
        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let chan = chan.clone();
                thread::spawn(move || {
                    println!("[Publisher {}] Starting", thread_id);
                    for i in 0..messages_per_thread {
                        let msg = format!("{},{}", thread_id, i);
                        chan.push(msg);
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
                if let Some(_msg) = cons.pop() {
                    let count = received_clone.fetch_add(1, Ordering::Release) + 1;
                    if count % 10 == 0 {
                        println!("[Consumer] Processed {} messages", count);
                    }
                } else {
                    // No message, park with timeout
                    parker.park_timeout(Duration::from_millis(10));
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
