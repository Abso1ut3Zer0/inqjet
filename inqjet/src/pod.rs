//! Hot-path argument encoding via autoref specialization.
//!
//! Two populations, one dispatch mechanism:
//! - **POD types** (`InqJetPod`): fixed-size memcpy, no formatting
//! - **Bytes-like types** (String, &str): length-prefixed content copy, no formatting
//!
//! # Dispatch Mechanism
//!
//! The hot-path macro expands each argument to `(&HotArg(&val)).hot_method()`.
//! Method resolution picks the right path at compile time:
//!
//! 1. **Inherent methods** on `HotArg<&T> where T: InqJetPod` — memcpy (highest priority)
//! 2. **Trait methods** (`HotEncode`) on `&HotArg<&T>` — bytes-like (via autoref, lower priority)
//!
//! Inherent methods always win over trait methods in Rust's method resolution.
//! This is the same pattern used by serde and other crates for zero-cost
//! compile-time dispatch without specialization.

/// Marker trait for types safe to log as raw bytes via memcpy.
///
/// Types implementing this trait are encoded to the hot-path ring buffer
/// as a fixed-size memcpy of their byte representation. Decoding happens
/// on the consumer thread via `read_unaligned`.
///
/// # Contract
///
/// - Type must not have `Drop` (directly or transitively)
/// - Enforced at compile time: `needs_drop` assertion fires when the type
///   is used with a hot-path macro
///
/// # Example
///
/// ```
/// use inqjet::InqJetPod;
///
/// #[derive(Clone)]
/// struct OrderInfo {
///     id: u64,
///     price: f64,
///     qty: i64,
/// }
///
/// impl InqJetPod for OrderInfo {}
/// ```
pub trait InqJetPod: 'static {}

// -- Autoref dispatch machinery ---------------------------------------------

/// Wrapper enabling autoref-based dispatch between POD and bytes-like types.
///
/// Not user-facing — used by hot-path macro expansions.
#[doc(hidden)]
pub struct HotArg<T>(pub T);

/// Trait for bytes-like argument encoding. Lower priority than inherent
/// methods on `HotArg` due to autoref dispatch.
///
/// Not user-facing — used by hot-path macro expansions.
#[doc(hidden)]
pub trait HotEncode {
    fn hot_size(&self) -> usize;
    fn hot_encode(&self, buf: &mut [u8]);
}

// -- Priority 1: Inherent methods for InqJetPod types -----------------------

impl<T: InqJetPod> HotArg<&T> {
    #[inline]
    pub fn hot_size(&self) -> usize {
        const {
            assert!(
                !std::mem::needs_drop::<T>(),
                "InqJetPod: type must not require drop"
            )
        };
        std::mem::size_of::<T>()
    }

    #[inline]
    pub fn hot_encode(&self, buf: &mut [u8]) {
        const {
            assert!(
                !std::mem::needs_drop::<T>(),
                "InqJetPod: type must not require drop"
            )
        };
        let size = std::mem::size_of::<T>();
        // SAFETY: T: InqJetPod, validated !needs_drop above. Source is a valid
        // T, destination is a caller-owned buffer. Both valid for `size` bytes.
        // Same-binary constraint ensures layout consistency.
        unsafe {
            std::ptr::copy_nonoverlapping(self.0 as *const T as *const u8, buf.as_mut_ptr(), size);
        }
    }
}

// -- Priority 2: Trait impls for bytes-like types ---------------------------

impl HotEncode for &HotArg<&String> {
    #[inline]
    fn hot_size(&self) -> usize {
        4 + self.0.len()
    }

    #[inline]
    fn hot_encode(&self, buf: &mut [u8]) {
        let bytes = self.0.as_bytes();
        let len = bytes.len() as u32;
        buf[..4].copy_from_slice(&len.to_le_bytes());
        buf[4..4 + bytes.len()].copy_from_slice(bytes);
    }
}

impl HotEncode for &HotArg<&&str> {
    #[inline]
    fn hot_size(&self) -> usize {
        4 + self.0.len()
    }

    #[inline]
    fn hot_encode(&self, buf: &mut [u8]) {
        let bytes = self.0.as_bytes();
        let len = bytes.len() as u32;
        buf[..4].copy_from_slice(&len.to_le_bytes());
        buf[4..4 + bytes.len()].copy_from_slice(bytes);
    }
}

// -- Primitive InqJetPod impls ----------------------------------------------

macro_rules! impl_pod_primitive {
    ($($ty:ty),*) => {
        $(impl InqJetPod for $ty {})*
    };
}

impl_pod_primitive!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, f32, f64, bool);

// -- Consumer-side decoding helpers -----------------------------------------
// Used by consumer-side formatters when hot-path macros are wired up.

/// Reconstruct a POD value from bytes written by `HotArg::hot_encode`.
///
/// Uses `read_unaligned` — zero-cost on x86-64, handles misaligned buffer
/// offsets by copying to an aligned stack location.
#[allow(dead_code)]
pub(crate) fn pod_from_bytes<T: InqJetPod>(bytes: &[u8]) -> T {
    debug_assert!(bytes.len() >= std::mem::size_of::<T>());
    // SAFETY: bytes were written from a valid T via copy_nonoverlapping in
    // the same binary. Layout matches. read_unaligned handles alignment.
    unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const T) }
}

/// Read a length-prefixed string from the buffer. Returns (content, bytes_consumed).
#[allow(dead_code)]
pub(crate) fn bytes_from_buf(buf: &[u8]) -> (&str, usize) {
    let len = u32::from_le_bytes(
        buf[..4]
            .try_into()
            .expect("buffer too short for length prefix"),
    ) as usize;
    let content = std::str::from_utf8(&buf[4..4 + len]).expect("invalid UTF-8 in hot-path string");
    (content, 4 + len)
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

    impl InqJetPod for OrderInfo {}

    // -- POD dispatch tests --------------------------------------------------

    #[test]
    fn pod_u64() {
        let val: u64 = 42;
        let size = (&HotArg(&val)).hot_size();
        assert_eq!(size, 8);

        let mut buf = [0u8; 8];
        (&HotArg(&val)).hot_encode(&mut buf);
        assert_eq!(u64::from_ne_bytes(buf), 42);
    }

    #[test]
    fn pod_f64() {
        let val: f64 = 3.14;
        let size = (&HotArg(&val)).hot_size();
        assert_eq!(size, 8);

        let mut buf = [0u8; 8];
        (&HotArg(&val)).hot_encode(&mut buf);
        let decoded = f64::from_ne_bytes(buf);
        assert!((decoded - 3.14).abs() < f64::EPSILON);
    }

    #[test]
    fn pod_bool() {
        let val = true;
        assert_eq!((&HotArg(&val)).hot_size(), 1);

        let mut buf = [0u8; 1];
        (&HotArg(&val)).hot_encode(&mut buf);
        assert_eq!(buf[0], 1);
    }

    #[test]
    fn pod_user_struct() {
        let order = OrderInfo {
            id: 12345,
            price: 99.95,
            qty: -100,
        };
        let size = (&HotArg(&order)).hot_size();
        assert_eq!(size, std::mem::size_of::<OrderInfo>());

        let mut buf = vec![0u8; size];
        (&HotArg(&order)).hot_encode(&mut buf);

        let decoded: OrderInfo = pod_from_bytes(&buf);
        assert_eq!(decoded, order);
    }

    #[test]
    fn pod_round_trip_all_primitives() {
        macro_rules! test_primitive {
            ($ty:ty, $val:expr) => {{
                let val: $ty = $val;
                let size = (&HotArg(&val)).hot_size();
                assert_eq!(size, std::mem::size_of::<$ty>());

                let mut buf = vec![0u8; size];
                (&HotArg(&val)).hot_encode(&mut buf);

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

    // -- Bytes-like dispatch tests -------------------------------------------

    #[test]
    fn bytes_string() {
        let val = String::from("hello world");
        let size = (&HotArg(&val)).hot_size();
        assert_eq!(size, 4 + 11);

        let mut buf = vec![0u8; size];
        (&HotArg(&val)).hot_encode(&mut buf);

        let (content, consumed) = bytes_from_buf(&buf);
        assert_eq!(content, "hello world");
        assert_eq!(consumed, 15);
    }

    #[test]
    fn bytes_str_ref() {
        let val: &str = "hello";
        let size = (&HotArg(&val)).hot_size();
        assert_eq!(size, 4 + 5);

        let mut buf = vec![0u8; size];
        (&HotArg(&val)).hot_encode(&mut buf);

        let (content, consumed) = bytes_from_buf(&buf);
        assert_eq!(content, "hello");
        assert_eq!(consumed, 9);
    }

    #[test]
    fn bytes_empty_string() {
        let val = String::new();
        let size = (&HotArg(&val)).hot_size();
        assert_eq!(size, 4); // just the length prefix

        let mut buf = vec![0u8; size];
        (&HotArg(&val)).hot_encode(&mut buf);

        let (content, consumed) = bytes_from_buf(&buf);
        assert_eq!(content, "");
        assert_eq!(consumed, 4);
    }

    // -- Mixed dispatch (simulates macro expansion) --------------------------

    #[test]
    fn mixed_dispatch_sequence() {
        // Simulates: hot_info!("order {} price {} name {}", order_id, price, name)
        let order_id: u64 = 42;
        let price: f64 = 123.45;
        let name = String::from("BTC-USD");

        let total = (&HotArg(&order_id)).hot_size()
            + (&HotArg(&price)).hot_size()
            + (&HotArg(&name)).hot_size();
        assert_eq!(total, 8 + 8 + (4 + 7));

        let mut buf = vec![0u8; total];
        let mut offset = 0;

        (&HotArg(&order_id)).hot_encode(&mut buf[offset..]);
        offset += (&HotArg(&order_id)).hot_size();

        (&HotArg(&price)).hot_encode(&mut buf[offset..]);
        offset += (&HotArg(&price)).hot_size();

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
        let seq: u32 = 7;
        let symbol: &str = "ETH";

        let total = (&HotArg(&seq)).hot_size() + (&HotArg(&symbol)).hot_size();
        assert_eq!(total, 4 + (4 + 3));

        let mut buf = vec![0u8; total];
        (&HotArg(&seq)).hot_encode(&mut buf[0..]);
        (&HotArg(&symbol)).hot_encode(&mut buf[4..]);

        let decoded_seq: u32 = pod_from_bytes(&buf[0..4]);
        assert_eq!(decoded_seq, 7);

        let (decoded_sym, _) = bytes_from_buf(&buf[4..]);
        assert_eq!(decoded_sym, "ETH");
    }
}
