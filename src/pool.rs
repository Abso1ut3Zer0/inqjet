//! String pool for zero-allocation log message formatting.
//!
//! This module provides a thread-safe object pool for `String` instances to minimize
//! allocations during high-frequency logging operations. Strings are reused across
//! log messages, with automatic capacity management to prevent unbounded memory growth.

use crossbeam_queue::SegQueue;

/// Maximum capacity for pooled strings. Strings exceeding this size will be
/// shrunk back to this capacity when returned to the pool.
const STRING_CAPACITY: usize = 256;

/// Global thread-safe pool of reusable `String` instances.
///
/// Uses a lock-free queue to allow concurrent access from multiple producer threads
/// without contention. The pool automatically grows as needed and manages string
/// capacities to prevent memory bloat.
static POOL: SegQueue<String> = SegQueue::new();

/// Zero-allocation string pool for high-performance logging.
///
/// This pool provides reusable `String` instances to avoid allocations during
/// log message formatting. Strings are automatically returned to the pool when
/// log entries are processed, creating a efficient memory recycling system.
///
/// # Thread Safety
///
/// All operations are thread-safe and lock-free, making this suitable for
/// concurrent access from multiple logging threads.
///
/// # Memory Management
///
/// - Strings are created with [`STRING_CAPACITY`] (256 bytes) initial capacity
/// - Returned strings are shrunk if they exceed the target capacity
/// - The pool size is naturally bounded by the logging queue capacity
///
/// # Example
///
/// ```rust, ignore
/// use crate::pool::Pool;
///
/// // Get a string from the pool
/// let mut s = Pool::get();
///
/// // Use it for formatting
/// use std::fmt::Write;
/// write!(s, "Log message: {}", 42).unwrap();
///
/// // Return it to the pool when done
/// Pool::put(s);
/// ```
pub(crate) struct Pool;

impl Pool {
    /// Retrieves a `String` from the pool or creates a new one if the pool is empty.
    ///
    /// This operation is optimized for the hot path of log message creation and
    /// should be very fast in most cases where pooled strings are available.
    ///
    /// # Returns
    ///
    /// A `String` with at least [`STRING_CAPACITY`] bytes of capacity, either
    /// reused from the pool or freshly allocated.
    ///
    /// # Performance
    ///
    /// - **Pool hit**: Single atomic operation to pop from queue (~10-50ns)
    /// - **Pool miss**: New allocation with pre-sized capacity (~100-500ns)
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// let s = Pool::get();
    /// assert!(s.capacity() >= 256);
    /// assert_eq!(s.len(), 0); // Always starts empty
    /// ```
    #[inline(always)]
    pub(crate) fn get() -> String {
        POOL.pop()
            .unwrap_or_else(|| String::with_capacity(STRING_CAPACITY))
    }

    /// Returns a `String` to the pool for reuse.
    ///
    /// The string is automatically cleared and its capacity is managed to prevent
    /// memory bloat. Strings that have grown significantly larger than the target
    /// capacity are shrunk back down.
    ///
    /// # Parameters
    ///
    /// * `s` - The string to return to the pool. Will be cleared and potentially resized.
    ///
    /// # Memory Management
    ///
    /// - String contents are cleared (length becomes 0)
    /// - If capacity > [`STRING_CAPACITY`], the string is shrunk to fit
    /// - The processed string is pushed back to the pool for reuse
    ///
    /// # Performance
    ///
    /// - **Normal case**: Clear + single atomic push (~20-100ns)
    /// - **Shrink case**: Clear + reallocation + atomic push (~1-10μs)
    ///
    /// # Example
    ///
    /// ```rust, ignore
    /// let mut s = Pool::get();
    /// s.push_str("some log message");
    ///
    /// // After use, return to pool
    /// Pool::put(s); // String is cleared and ready for reuse
    /// ```
    #[inline(always)]
    pub(crate) fn put(mut s: String) {
        s.clear();
        if s.capacity() > STRING_CAPACITY {
            s.shrink_to(STRING_CAPACITY);
        }
        POOL.push(s);
    }
}
