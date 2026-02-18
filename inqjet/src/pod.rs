//! Hot-path argument encoding via autoref specialization.
//!
//! Two tiers, one dispatch mechanism:
//! - **POD types** (`Pod`): fixed-size memcpy, no formatting
//! - **Everything else**: eagerly formatted on producer, written as length-prefixed bytes
//!
//! # Dispatch Mechanism (serde-style autoref)
//!
//! The hot-path macro expands each argument to `(&HotArg(&val)).method()`.
//! Method resolution picks the right path at compile time:
//!
//! 1. **Inherent methods** on `HotArg<&T> where T: Pod` — memcpy (highest priority)
//! 2. **Trait methods** (`HotEncode`) on `HotArg<&T>` — stash-based fallback
//!
//! Inherent methods always win over trait methods in Rust's method resolution.
//! When the `Pod` bound isn't satisfied, the compiler falls through to
//! the blanket trait impl — same pattern serde uses for specialization.

use std::cell::{Cell, RefCell};

/// Marker trait for types safe to log as raw bytes via memcpy.
///
/// Types implementing this trait are encoded to the hot-path ring buffer
/// as a fixed-size memcpy of their byte representation. Decoding happens
/// on the consumer thread via `read_unaligned`.
///
/// # Contract
///
/// - Type must not have `Drop` (directly or transitively)
/// - Enforced at compile time: `needs_drop` assertion in `#[derive(Pod)]`
///   and in the `HotArg` inherent methods
///
/// # Example
///
/// ```
/// use inqjet::Pod;
///
/// #[derive(Pod)]
/// struct OrderInfo {
///     id: u64,
///     price: f64,
///     qty: i64,
/// }
/// ```
pub trait Pod: 'static {}

// -- Autoref dispatch machinery -----------------------------------------------

/// Wrapper enabling autoref-based dispatch between POD and fallback types.
///
/// Not user-facing — used by hot-path macro expansions.
#[doc(hidden)]
pub struct HotArg<T>(pub T);

/// Zero-sized witness marker for consumer-side type routing.
///
/// The proc macro calls `hot_witness()` on each arg, getting back a
/// `Witness<T>` that carries the `HotDecode` type through to the witness
/// function without needing a reference to the actual value.
#[doc(hidden)]
pub struct Witness<T>(std::marker::PhantomData<T>);

/// Marker type for fallback-encoded arguments on the consumer side.
///
/// When a type isn't `Pod`, it gets eagerly formatted on the
/// producer thread and written as length-prefixed bytes. `PreFormatted`
/// decodes to `PreFormattedValue`.
#[doc(hidden)]
pub struct PreFormatted;

/// Consumer-side wrapper for eagerly-formatted arguments.
///
/// Implements all 9 `std::fmt` traits as passthrough — writes the
/// pre-formatted content verbatim. This avoids double-formatting:
/// the producer already applied the format spec.
#[doc(hidden)]
pub struct PreFormattedValue(pub String);

macro_rules! impl_passthrough_fmt {
    ($($trait:ident),*) => {
        $(impl std::fmt::$trait for PreFormattedValue {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        })*
    };
}

impl_passthrough_fmt!(
    Display, Debug, LowerHex, UpperHex, Octal, Binary, LowerExp, UpperExp, Pointer
);

// -- Tier 2: Blanket trait for non-POD types ----------------------------------

/// Trait for non-POD argument encoding. Lower priority than inherent
/// methods on `HotArg` due to Rust's method resolution rules.
///
/// Not user-facing — used by hot-path macro expansions.
#[doc(hidden)]
pub trait HotEncode {
    fn hot_size_with<F: FnOnce(&mut String)>(&self, fmt: F) -> usize
    where
        Self: Sized;
    fn hot_encode(&self, buf: &mut [u8]);
    fn hot_witness(&self) -> Witness<PreFormatted>;
}

// -- Tier 1: Inherent methods for Pod types -----------------------------

impl<T: Pod> HotArg<&T> {
    #[inline]
    pub fn hot_size(&self) -> usize {
        const {
            assert!(
                !std::mem::needs_drop::<T>(),
                "Pod: type must not require drop"
            )
        };
        std::mem::size_of::<T>()
    }

    #[inline]
    pub fn hot_encode(&self, buf: &mut [u8]) {
        const {
            assert!(
                !std::mem::needs_drop::<T>(),
                "Pod: type must not require drop"
            )
        };
        let size = std::mem::size_of::<T>();
        // SAFETY: T: Pod, validated !needs_drop above. Source is a valid
        // T, destination is a caller-owned buffer. Both valid for `size` bytes.
        // Same-binary constraint ensures layout consistency.
        unsafe {
            std::ptr::copy_nonoverlapping(self.0 as *const T as *const u8, buf.as_mut_ptr(), size);
        }
    }

    #[inline]
    pub fn hot_size_with<F: FnOnce(&mut String)>(&self, _fmt: F) -> usize {
        self.hot_size()
    }

    #[inline]
    pub fn hot_witness(&self) -> Witness<T> {
        Witness(std::marker::PhantomData)
    }
}

// -- Tier 2: Blanket impl for everything else ---------------------------------

impl<T> HotEncode for HotArg<&T> {
    #[inline]
    fn hot_size_with<F: FnOnce(&mut String)>(&self, fmt: F) -> usize {
        FALLBACK_BUF.with(|buf| {
            let mut buf = buf.borrow_mut();
            let start = buf.len();
            FALLBACK_OFFSETS.with(|offsets| offsets.borrow_mut().push(start));
            fmt(&mut buf);
            4 + (buf.len() - start)
        })
    }

    #[inline]
    fn hot_encode(&self, buf: &mut [u8]) {
        fallback_stash_encode(buf);
    }

    #[inline]
    fn hot_witness(&self) -> Witness<PreFormatted> {
        Witness(std::marker::PhantomData)
    }
}

// -- Primitive Pod impls ------------------------------------------------

macro_rules! impl_pod_primitive {
    ($($ty:ty),*) => {
        $(impl Pod for $ty {})*
    };
}

impl_pod_primitive!(
    u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, f32, f64, bool
);

// -- Consumer-side decoding helpers -------------------------------------------

/// Reconstruct a POD value from bytes written by `HotArg::hot_encode`.
///
/// Uses `read_unaligned` — zero-cost on x86-64, handles misaligned buffer
/// offsets by copying to an aligned stack location.
pub(crate) fn pod_from_bytes<T: Pod>(bytes: &[u8]) -> T {
    debug_assert!(bytes.len() >= std::mem::size_of::<T>());
    // SAFETY: bytes were written from a valid T via copy_nonoverlapping in
    // the same binary. Layout matches. read_unaligned handles alignment.
    unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const T) }
}

/// Read a length-prefixed string from the buffer. Returns (content, bytes_consumed).
pub(crate) fn bytes_from_buf(buf: &[u8]) -> (&str, usize) {
    let len = u32::from_le_bytes(
        buf[..4]
            .try_into()
            .expect("buffer too short for length prefix"),
    ) as usize;
    let content = std::str::from_utf8(&buf[4..4 + len]).expect("invalid UTF-8 in hot-path string");
    (content, 4 + len)
}

// -- Consumer-side decoding trait ---------------------------------------------

/// Consumer-side decoding of hot-path encoded arguments.
///
/// Each type that can be encoded via `HotArg` implements this trait
/// so the consumer-side formatter can reconstruct values from the
/// byte payload.
///
/// `Decoded` must implement `Debug`. Additional formatting traits
/// (Display, LowerHex, etc.) are enforced per-arg by the proc macro
/// based on the format specifier used.
#[doc(hidden)]
pub trait HotDecode {
    type Decoded: std::fmt::Debug;
    fn hot_decode(buf: &[u8]) -> (Self::Decoded, usize);
}

/// Blanket impl: all Pod types that implement Debug can be decoded
/// from their raw byte representation.
impl<T: Pod + std::fmt::Debug> HotDecode for T {
    type Decoded = T;
    fn hot_decode(buf: &[u8]) -> (T, usize) {
        (pod_from_bytes::<T>(buf), std::mem::size_of::<T>())
    }
}

impl HotDecode for String {
    type Decoded = String;
    fn hot_decode(buf: &[u8]) -> (String, usize) {
        let (s, n) = bytes_from_buf(buf);
        (s.to_owned(), n)
    }
}

impl HotDecode for PreFormatted {
    type Decoded = PreFormattedValue;
    fn hot_decode(buf: &[u8]) -> (PreFormattedValue, usize) {
        let (s, n) = bytes_from_buf(buf);
        (PreFormattedValue(s.to_owned()), n)
    }
}

// -- Thread-local fallback stash ----------------------------------------------

thread_local! {
    static FALLBACK_BUF: RefCell<String> = const { RefCell::new(String::new()) };
    static FALLBACK_OFFSETS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static FALLBACK_READ_IDX: Cell<usize> = const { Cell::new(0) };
}

/// Pre-allocates the thread-local fallback formatting buffer.
///
/// Call once per hot thread at startup. Avoids allocation on the first
/// log call that hits the fallback path.
///
/// ```rust,ignore
/// // In your hot thread's init:
/// inqjet::reserve_fallback_capacity(1024);
/// ```
pub fn reserve_fallback_capacity(bytes: usize) {
    FALLBACK_BUF.with(|b| b.borrow_mut().reserve(bytes));
    FALLBACK_OFFSETS.with(|o| o.borrow_mut().reserve(16));
}

/// Clears the fallback stash. Called at the start of each log block.
#[doc(hidden)]
pub fn fallback_stash_clear() {
    FALLBACK_BUF.with(|b| b.borrow_mut().clear());
    FALLBACK_OFFSETS.with(|o| o.borrow_mut().clear());
    FALLBACK_READ_IDX.with(|i| i.set(0));
}

/// Encodes the next stashed fallback value into `buf` as length-prefixed bytes.
///
/// Reads from the TLS stash in order — each call advances the read index.
fn fallback_stash_encode(buf: &mut [u8]) {
    FALLBACK_READ_IDX.with(|idx| {
        let i = idx.get();
        idx.set(i + 1);
        FALLBACK_BUF.with(|stash| {
            let stash = stash.borrow();
            FALLBACK_OFFSETS.with(|offsets| {
                let offsets = offsets.borrow();
                let start = offsets[i];
                let end = offsets.get(i + 1).copied().unwrap_or(stash.len());
                let content = &stash.as_bytes()[start..end];
                let len = content.len() as u32;
                buf[..4].copy_from_slice(&len.to_le_bytes());
                buf[4..4 + content.len()].copy_from_slice(content);
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- User-defined POD struct for testing ---------------------------------

    #[derive(Clone, Debug, PartialEq)]
    struct OrderInfo {
        id: u64,
        price: f64,
        qty: i64,
    }

    impl Pod for OrderInfo {}

    // -- POD dispatch tests --------------------------------------------------

    #[test]
    fn pod_u64() {
        let val: u64 = 42;
        let size = HotArg(&val).hot_size();
        assert_eq!(size, 8);

        let mut buf = [0u8; 8];
        HotArg(&val).hot_encode(&mut buf);
        assert_eq!(u64::from_ne_bytes(buf), 42);
    }

    #[test]
    fn pod_f64() {
        let val: f64 = 3.14;
        let size = HotArg(&val).hot_size();
        assert_eq!(size, 8);

        let mut buf = [0u8; 8];
        HotArg(&val).hot_encode(&mut buf);
        let decoded = f64::from_ne_bytes(buf);
        assert!((decoded - 3.14).abs() < f64::EPSILON);
    }

    #[test]
    fn pod_bool() {
        let val = true;
        assert_eq!(HotArg(&val).hot_size(), 1);

        let mut buf = [0u8; 1];
        HotArg(&val).hot_encode(&mut buf);
        assert_eq!(buf[0], 1);
    }

    #[test]
    fn pod_user_struct() {
        let order = OrderInfo {
            id: 12345,
            price: 99.95,
            qty: -100,
        };
        let size = HotArg(&order).hot_size();
        assert_eq!(size, std::mem::size_of::<OrderInfo>());

        let mut buf = vec![0u8; size];
        HotArg(&order).hot_encode(&mut buf);

        let decoded: OrderInfo = pod_from_bytes(&buf);
        assert_eq!(decoded, order);
    }

    #[test]
    fn pod_round_trip_all_primitives() {
        macro_rules! test_primitive {
            ($ty:ty, $val:expr) => {{
                let val: $ty = $val;
                let size = HotArg(&val).hot_size();
                assert_eq!(size, std::mem::size_of::<$ty>());

                let mut buf = vec![0u8; size];
                HotArg(&val).hot_encode(&mut buf);

                let decoded: $ty = pod_from_bytes(&buf);
                assert_eq!(decoded, val);
            }};
        }

        test_primitive!(u8, 255);
        test_primitive!(u16, 65535);
        test_primitive!(u32, 42);
        test_primitive!(u64, u64::MAX);
        test_primitive!(u128, u128::MAX);
        test_primitive!(i8, -1);
        test_primitive!(i16, -100);
        test_primitive!(i32, i32::MIN);
        test_primitive!(i64, -999);
        test_primitive!(i128, i128::MIN);
        test_primitive!(f32, 1.5);
        test_primitive!(f64, std::f64::consts::PI);
        test_primitive!(bool, false);
    }

    // -- Stash-based dispatch tests (String, &str, everything else) ----------

    #[test]
    fn stash_string() {
        fallback_stash_clear();
        let val = String::from("hello world");
        let size = (&HotArg(&val)).hot_size_with(|stash| {
            use std::fmt::Write;
            write!(stash, "{}", val).ok();
        });
        assert_eq!(size, 4 + 11);

        let mut buf = vec![0u8; size];
        (&HotArg(&val)).hot_encode(&mut buf);

        let (content, consumed) = bytes_from_buf(&buf);
        assert_eq!(content, "hello world");
        assert_eq!(consumed, 15);
    }

    #[test]
    fn stash_str_ref() {
        fallback_stash_clear();
        let val: &str = "hello";
        let size = (&HotArg(&val)).hot_size_with(|stash| {
            use std::fmt::Write;
            write!(stash, "{}", val).ok();
        });
        assert_eq!(size, 4 + 5);

        let mut buf = vec![0u8; size];
        (&HotArg(&val)).hot_encode(&mut buf);

        let (content, consumed) = bytes_from_buf(&buf);
        assert_eq!(content, "hello");
        assert_eq!(consumed, 9);
    }

    #[test]
    fn stash_empty_string() {
        fallback_stash_clear();
        let val = String::new();
        let size = (&HotArg(&val)).hot_size_with(|stash| {
            use std::fmt::Write;
            write!(stash, "{}", val).ok();
        });
        assert_eq!(size, 4); // just the length prefix

        let mut buf = vec![0u8; size];
        (&HotArg(&val)).hot_encode(&mut buf);

        let (content, consumed) = bytes_from_buf(&buf);
        assert_eq!(content, "");
        assert_eq!(consumed, 4);
    }

    #[test]
    fn stash_vec() {
        fallback_stash_clear();
        let val = vec![1, 2, 3];
        let size = (&HotArg(&val)).hot_size_with(|stash| {
            use std::fmt::Write;
            write!(stash, "{:?}", val).ok();
        });
        assert_eq!(size, 4 + 9); // "[1, 2, 3]" is 9 bytes

        let mut buf = vec![0u8; size];
        (&HotArg(&val)).hot_encode(&mut buf);

        let (content, consumed) = bytes_from_buf(&buf);
        assert_eq!(content, "[1, 2, 3]");
        assert_eq!(consumed, 13);
    }

    #[test]
    fn stash_multiple_args() {
        fallback_stash_clear();
        let a = vec![1, 2];
        let b = String::from("test");

        let sa = (&HotArg(&a)).hot_size_with(|stash| {
            use std::fmt::Write;
            write!(stash, "{:?}", a).ok();
        });
        let sb = (&HotArg(&b)).hot_size_with(|stash| {
            use std::fmt::Write;
            write!(stash, "{}", b).ok();
        });

        let mut buf_a = vec![0u8; sa];
        (&HotArg(&a)).hot_encode(&mut buf_a);
        let (content_a, _) = bytes_from_buf(&buf_a);
        assert_eq!(content_a, "[1, 2]");

        let mut buf_b = vec![0u8; sb];
        (&HotArg(&b)).hot_encode(&mut buf_b);
        let (content_b, _) = bytes_from_buf(&buf_b);
        assert_eq!(content_b, "test");
    }

    // -- Mixed dispatch (simulates macro expansion) --------------------------

    #[test]
    fn mixed_pod_and_stash() {
        fallback_stash_clear();
        let order_id: u64 = 42;
        let price: f64 = 123.45;
        let name = String::from("BTC-USD");

        // POD args use inherent hot_size, stash args use hot_size_with
        let s0 = HotArg(&order_id).hot_size();
        let s1 = HotArg(&price).hot_size();
        let s2 = (&HotArg(&name)).hot_size_with(|stash| {
            use std::fmt::Write;
            write!(stash, "{}", name).ok();
        });
        let total = s0 + s1 + s2;
        assert_eq!(total, 8 + 8 + (4 + 7));

        let mut buf = vec![0u8; total];
        let mut offset = 0;

        HotArg(&order_id).hot_encode(&mut buf[offset..]);
        offset += s0;

        HotArg(&price).hot_encode(&mut buf[offset..]);
        offset += s1;

        (&HotArg(&name)).hot_encode(&mut buf[offset..]);

        // Decode
        let decoded_id: u64 = pod_from_bytes(&buf[0..8]);
        assert_eq!(decoded_id, 42);

        let decoded_price: f64 = pod_from_bytes(&buf[8..16]);
        assert!((decoded_price - 123.45).abs() < f64::EPSILON);

        let (decoded_name, _) = bytes_from_buf(&buf[16..]);
        assert_eq!(decoded_name, "BTC-USD");
    }

    #[test]
    fn mixed_pod_and_str() {
        fallback_stash_clear();
        let seq: u32 = 7;
        let symbol: &str = "ETH";

        let s0 = HotArg(&seq).hot_size();
        let s1 = (&HotArg(&symbol)).hot_size_with(|stash| {
            use std::fmt::Write;
            write!(stash, "{}", symbol).ok();
        });
        let total = s0 + s1;
        assert_eq!(total, 4 + (4 + 3));

        let mut buf = vec![0u8; total];
        HotArg(&seq).hot_encode(&mut buf[0..]);
        (&HotArg(&symbol)).hot_encode(&mut buf[4..]);

        let decoded_seq: u32 = pod_from_bytes(&buf[0..4]);
        assert_eq!(decoded_seq, 7);

        let (decoded_sym, _) = bytes_from_buf(&buf[4..]);
        assert_eq!(decoded_sym, "ETH");
    }

    // -- Tier 1 hot_size_with ignores closure --------------------------------

    #[test]
    fn pod_hot_size_with_ignores_closure() {
        let val: u64 = 42;
        let called = Cell::new(false);
        let size = HotArg(&val).hot_size_with(|_| {
            called.set(true);
        });
        assert_eq!(size, 8);
        assert!(!called.get(), "closure should not be called for POD");
    }

    // -- PreFormattedValue passthrough ---------------------------------------

    #[test]
    fn preformatted_value_passthrough() {
        let pf = PreFormattedValue("hello".into());
        assert_eq!(format!("{}", pf), "hello");
        assert_eq!(format!("{:?}", pf), "hello");
        assert_eq!(format!("{:x}", pf), "hello");
    }

    // -- Reserve capacity ----------------------------------------------------

    #[test]
    fn reserve_capacity_doesnt_panic() {
        reserve_fallback_capacity(4096);
    }
}
