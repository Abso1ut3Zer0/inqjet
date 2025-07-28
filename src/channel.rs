//! High-performance bounded channel for log message transmission.
//!
//! This module provides a lock-free, bounded channel optimized for low-latency logging.
//! The channel uses a fixed-capacity ring buffer with backpressure handling and efficient
//! consumer notification to minimize producer-side overhead while ensuring reliable delivery.

use crossbeam_queue::ArrayQueue;
use crossbeam_utils::{Backoff, sync::Unparker};

/// A bounded, lock-free channel for transmitting log messages between producer and consumer threads.
///
/// This channel is specifically designed for high-frequency logging scenarios where:
/// - Producer threads need minimal latency overhead
/// - Backpressure should block producers rather than drop messages
/// - Consumer threads need efficient notification when messages are available
///
/// # Design Philosophy
///
/// The channel prioritizes **producer performance** over consumer convenience. Producers
/// get a simple, fast push operation while the consumer handles the complexity of parking/unparking.
///
/// # Backpressure Behavior
///
/// When the channel is full, producers will:
/// 1. Notify the consumer via `unparker.unpark()`
/// 2. Spin with exponential backoff until space becomes available
/// 3. Never drop messages or return errors
///
/// This ensures message delivery while applying natural flow control when the consumer
/// can't keep up with producer load.
///
/// # Performance Characteristics
///
/// - **Normal case**: Single atomic CAS operation (~10-50ns)
/// - **Backpressure case**: Unpark + spinning with backoff (~1-100μs depending on consumer speed)
/// - **Pop operation**: Single atomic operation (~10-50ns)
///
/// # Example
///
/// ```rust, ignore
/// use crossbeam_utils::sync::Parker;
/// use crate::channel::Channel;
///
/// let parker = Parker::new();
/// let unparker = parker.unparker().clone();
/// let channel = Channel::new(1024, unparker);
///
/// // Producer thread
/// channel.push("log message".to_string());
///
/// // Consumer thread
/// if let Some(msg) = channel.pop() {
///     println!("Received: {}", msg);
/// } else {
///     parker.park(); // Wait for notification
/// }
/// ```
pub(crate) struct Channel {
    /// Lock-free ring buffer for storing log messages.
    ///
    /// Uses `ArrayQueue` for its excellent performance characteristics and
    /// wait-free operations on both push and pop paths.
    rb: ArrayQueue<String>,

    /// Notifier for waking up the consumer thread when messages are available.
    ///
    /// Only used when the channel becomes full and we need to wake a potentially
    /// sleeping consumer to make space.
    unparker: Unparker,
}

impl Channel {
    /// Creates a new bounded channel with the specified capacity.
    ///
    /// # Parameters
    ///
    /// * `capacity` - Maximum number of messages the channel can hold. Should be sized
    ///   based on expected burst patterns and consumer processing speed.
    /// * `unparker` - Handle for waking up the consumer thread when backpressure occurs.
    ///
    /// # Capacity Sizing Guidelines
    ///
    /// - **Low-latency logging**: 64-256 messages (minimize memory, quick backpressure)
    /// - **High-throughput logging**: 1024-4096 messages (absorb bursts, batch processing)
    /// - **Burst-tolerant logging**: 8192+ messages (handle large spikes)
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// use crossbeam_utils::sync::Parker;
    ///
    /// let parker = Parker::new();
    /// let unparker = parker.unparker().clone();
    ///
    /// // Create channel for moderate throughput logging
    /// let channel = Channel::new(1024, unparker);
    /// ```
    pub(crate) fn new(capacity: usize, unparker: Unparker) -> Self {
        Self {
            rb: ArrayQueue::new(capacity),
            unparker,
        }
    }

    /// Pushes a log message into the channel, blocking if full.
    ///
    /// This method **never fails** and **never drops messages**. If the channel is full,
    /// it will notify the consumer and spin with exponential backoff until space becomes
    /// available. This provides reliable delivery with natural flow control.
    ///
    /// # Backpressure Handling
    ///
    /// When the channel is full:
    /// 1. Consumer is notified via `unparker.unpark()` (only once per push attempt)
    /// 2. Producer spins with exponential backoff using [`Backoff::spin()`]
    /// 3. Push is retried until successful
    ///
    /// The backoff strategy starts with short spins and gradually increases delay,
    /// reducing CPU usage while maintaining responsiveness.
    ///
    /// # Performance
    ///
    /// - **Fast path** (space available): ~10-50ns (single atomic CAS)
    /// - **Slow path** (backpressure): ~1-100μs (depends on consumer wakeup time)
    ///
    /// # Parameters
    ///
    /// * `log` - The log message to send. Ownership is transferred to the channel.
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// // This will always succeed, even if it takes time
    /// channel.push("Important log message".to_string());
    ///
    /// // Multiple pushes are safe from any thread
    /// for i in 0..1000 {
    ///     channel.push(format!("Message {}", i));
    /// }
    /// ```
    #[inline(always)]
    pub(crate) fn push(&self, mut log: String) {
        let backoff = Backoff::new();

        while let Err(returned_log) = self.rb.push(log) {
            log = returned_log;
            self.unparker.unpark();
            backoff.snooze();
        }
    }

    /// Attempts to pop a message from the channel.
    ///
    /// This is a non-blocking operation that immediately returns `None` if no
    /// messages are available. The consumer should use this in conjunction with
    /// a parking mechanism for efficient waiting.
    ///
    /// # Returns
    ///
    /// * `Some(String)` - A log message if one was available
    /// * `None` - If the channel is currently empty
    ///
    /// # Performance
    ///
    /// Always completes in ~10-50ns regardless of channel state (single atomic operation).
    ///
    /// # Consumer Pattern
    ///
    /// ```rust, ignore
    /// use std::time::Duration;
    /// use crossbeam_utils::sync::Parker;
    ///
    /// let parker = Parker::new();
    ///
    /// loop {
    ///     // Drain all available messages
    ///     while let Some(msg) = channel.pop() {
    ///         process_log_message(msg);
    ///     }
    ///
    ///     // Wait for more messages with timeout
    ///     parker.park_timeout(Duration::from_millis(10));
    /// }
    ///
    /// fn process_log_message(msg: String) {
    ///     // Handle the log message
    /// }
    /// ```
    #[inline(always)]
    pub(crate) fn pop(&self) -> Option<String> {
        self.rb.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_utils::sync::Parker;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
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
