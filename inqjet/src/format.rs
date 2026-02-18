//! Consumer-side formatting: timestamp, colors, standard format function.
//!
//! All formatting runs on the archiver thread, never on the producer.
//! The standard format function reads the payload layout written by the
//! producer and outputs colored/uncolored log lines.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(any(feature = "log-compat", test))]
use crate::record;

/// Global color flag, set once at initialization.
pub(crate) static COLOR_ENABLED: AtomicBool = AtomicBool::new(false);

const RESET: &[u8] = b"\x1b[0m";
const GRAY: &[u8] = b"\x1b[90m";

fn level_color(level: u8) -> &'static [u8] {
    match level {
        1 => b"\x1b[31m", // Red — Error
        2 => b"\x1b[33m", // Yellow — Warn
        3 => b"\x1b[32m", // Green — Info
        4 => b"\x1b[36m", // Cyan — Debug
        _ => GRAY,        // Gray — Trace
    }
}

fn level_str(level: u8) -> &'static str {
    match level {
        1 => "ERROR",
        2 => "WARN",
        3 => "INFO",
        4 => "DEBUG",
        5 => "TRACE",
        _ => "?????",
    }
}

/// Snaps current time as u64 nanoseconds since Unix epoch.
///
/// u64 nanos overflow in ~584 years (year ~2554). Current timestamps
/// use ~1.7e18 of the 1.8e19 capacity.
pub(crate) fn snap_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos() as u64
}

/// Formats u64 nanos as ISO 8601 UTC: `2024-01-15T14:30:45.123456789Z`
pub(crate) fn format_timestamp(nanos: u64, out: &mut dyn Write) {
    let total_secs = (nanos / 1_000_000_000) as i64;
    let subsec_nanos = (nanos % 1_000_000_000) as u32;

    let days = total_secs.div_euclid(86400) as i32;
    let secs = total_secs.rem_euclid(86400) as u32;

    let (year, month, day) = civil_from_days(days);
    let hour = secs / 3600;
    let min = (secs % 3600) / 60;
    let sec = secs % 60;

    let _ = write!(
        out,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        year, month, day, hour, min, sec, subsec_nanos
    );
}

/// Converts days since Unix epoch to (year, month, day).
///
/// Algorithm from Howard Hinnant:
/// <http://howardhinnant.github.io/date_algorithms.html>
fn civil_from_days(days: i32) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i32 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Standard format function for records produced via `format_args!()`.
///
/// Only used by the `log` crate bridge path (`log_impl`).
///
/// Payload layout:
/// ```text
/// [line: u32 LE][target_len: u16 LE][target bytes][message bytes]
/// ```
///
/// Output (no color):
/// ```text
/// 2024-01-15T14:30:45.123456789Z [INFO] my_app::module:42 message text
/// ```
#[cfg(feature = "log-compat")]
pub(crate) fn standard_format_fn(
    timestamp_ns: u64,
    level: u8,
    payload: &[u8],
    out: &mut dyn Write,
) {
    let colored = COLOR_ENABLED.load(Ordering::Relaxed);
    format_record(timestamp_ns, level, payload, out, colored);
}

/// Writes the log line prefix: `timestamp [LEVEL] target:line `.
///
/// Includes a trailing space. The caller appends the message and newline.
/// Used by both the standard format path and hot-path generated format functions.
fn write_prefix_inner(
    timestamp_ns: u64,
    level: u8,
    target: &str,
    line: u32,
    out: &mut dyn Write,
    colored: bool,
) {
    if colored {
        let _ = out.write_all(GRAY);
        format_timestamp(timestamp_ns, out);
        let _ = out.write_all(RESET);

        let _ = out.write_all(b" ");
        let _ = out.write_all(level_color(level));
        let _ = write!(out, "[{}]", level_str(level));
        let _ = out.write_all(RESET);

        let _ = out.write_all(b" ");
        let _ = out.write_all(GRAY);
        if line > 0 {
            let _ = write!(out, "{}:{}", target, line);
        } else {
            let _ = out.write_all(target.as_bytes());
        }
        let _ = out.write_all(RESET);

        let _ = out.write_all(b" ");
    } else {
        format_timestamp(timestamp_ns, out);
        if line > 0 {
            let _ = write!(out, " [{}] {}:{} ", level_str(level), target, line);
        } else {
            let _ = write!(out, " [{}] {} ", level_str(level), target);
        }
    }
}

/// Writes the log line prefix, reading color mode from the global flag.
///
/// Called by proc macro-generated format functions on the consumer thread.
pub(crate) fn write_log_prefix(
    timestamp_ns: u64,
    level: u8,
    target: &str,
    line: u32,
    out: &mut dyn Write,
) {
    let colored = COLOR_ENABLED.load(Ordering::Relaxed);
    write_prefix_inner(timestamp_ns, level, target, line, out, colored);
}

/// Inner formatting logic, takes an explicit color flag for testability.
#[cfg(any(feature = "log-compat", test))]
pub(crate) fn format_record(
    timestamp_ns: u64,
    level: u8,
    payload: &[u8],
    out: &mut dyn Write,
    colored: bool,
) {
    let (line, target, message) = record::read_standard_payload(payload);
    write_prefix_inner(timestamp_ns, level, target, line, out, colored);
    let _ = writeln!(out, "{}", message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_snap_is_reasonable() {
        let ts = snap_timestamp();
        // After 2024-01-01
        assert!(ts > 1_704_067_200_000_000_000);
        // Before 2100-01-01
        assert!(ts < 4_102_444_800_000_000_000);
    }

    #[test]
    fn timestamp_format_known_value() {
        // 1705329045 = 19737 days * 86400 + 52245 secs (14:30:45)
        let nanos: u64 = 1_705_329_045_123_456_789;
        let mut buf = Vec::new();
        format_timestamp(nanos, &mut buf);
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "2024-01-15T14:30:45.123456789Z");
    }

    #[test]
    fn timestamp_format_epoch() {
        let mut buf = Vec::new();
        format_timestamp(0, &mut buf);
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "1970-01-01T00:00:00.000000000Z");
    }

    #[test]
    fn standard_format_no_color() {
        let ts: u64 = 1_705_329_045_123_456_789;
        let mut payload = [0u8; 64];
        let n = record::write_standard_payload(&mut payload, 42, "my_app", "hello world");

        let mut out = Vec::new();
        format_record(ts, 3, &payload[..n], &mut out, false);

        let s = String::from_utf8(out).unwrap();
        assert_eq!(
            s,
            "2024-01-15T14:30:45.123456789Z [INFO] my_app:42 hello world\n"
        );
    }

    #[test]
    fn standard_format_no_line() {
        let ts: u64 = 1_705_329_045_123_456_789;
        let mut payload = [0u8; 64];
        let n = record::write_standard_payload(&mut payload, 0, "app", "msg");

        let mut out = Vec::new();
        format_record(ts, 1, &payload[..n], &mut out, false);

        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "2024-01-15T14:30:45.123456789Z [ERROR] app msg\n");
    }

    #[test]
    fn standard_format_with_color() {
        let ts: u64 = 1_705_329_045_123_456_789;
        let mut payload = [0u8; 64];
        let n = record::write_standard_payload(&mut payload, 10, "srv", "ok");

        let mut out = Vec::new();
        format_record(ts, 3, &payload[..n], &mut out, true);

        let s = String::from_utf8(out).unwrap();
        // Should contain ANSI codes
        assert!(s.contains("\x1b["));
        // Should still contain the content
        assert!(s.contains("[INFO]"));
        assert!(s.contains("srv:10"));
        assert!(s.contains("ok\n"));
    }

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn civil_from_days_known_date() {
        // 2024-01-15 = 19737 days since epoch
        assert_eq!(civil_from_days(19737), (2024, 1, 15));
    }

    #[test]
    fn civil_from_days_leap_year() {
        // 2024-02-29 = 19782 days since epoch
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
    }
}
