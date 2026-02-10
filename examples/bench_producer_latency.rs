//! Comprehensive producer latency benchmarks for InqJet.
//!
//! Measures the cost of log!() calls across different types, format specifiers,
//! payload sizes, and contention levels using rdtscp cycle-accurate timing and
//! HDR histogram for full latency distribution.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example bench_producer_latency
//!
//! # For accurate results: pin cores, disable turbo
//! taskset -c 0,2 cargo run --release --example bench_producer_latency
//! ```

use std::hint::black_box;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use inqjet::{ColorMode, InqJetBuilder};
use log::LevelFilter;

const WARMUP: usize = 10_000;
const SAMPLES: usize = 100_000;

// ─── Infrastructure ─────────────────────────────────────────────────────────

/// No-op writer. Consumer formats but discards output.
/// Eliminates I/O variance from producer measurements.
struct NullWriter;

impl io::Write for NullWriter {
    #[inline(always)]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    #[inline(always)]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Read the CPU timestamp counter. Serializing — waits for prior instructions
/// to retire before reading. Cost: ~18-20 cycles.
#[inline(always)]
fn rdtscp() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let mut aux: u32 = 0;
        std::arch::x86_64::__rdtscp(&mut aux)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // Fallback for non-x86: nanoseconds since first call.
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_nanos() as u64
    }
}

fn new_hist() -> Histogram<u64> {
    Histogram::new(3).expect("failed to create histogram")
}

fn print_header() {
    println!(
        "  {:<44} {:>7} {:>7} {:>7} {:>7} {:>8} {:>8}",
        "benchmark", "p50", "p75", "p95", "p99", "p999", "max"
    );
    println!("  {}", "-".repeat(100));
}

fn print_hist(label: &str, hist: &Histogram<u64>) {
    println!(
        "  {:<44} {:>7} {:>7} {:>7} {:>7} {:>8} {:>8}",
        label,
        hist.value_at_quantile(0.50),
        hist.value_at_quantile(0.75),
        hist.value_at_quantile(0.95),
        hist.value_at_quantile(0.99),
        hist.value_at_quantile(0.999),
        hist.max(),
    );
}

// ─── Type Spectrum ──────────────────────────────────────────────────────────

fn bench_static_message() -> Histogram<u64> {
    let mut hist = new_hist();
    for _ in 0..WARMUP {
        log::info!("checkpoint reached");
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("checkpoint reached");
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

fn bench_single_u64() -> Histogram<u64> {
    let mut hist = new_hist();
    let val: u64 = 1_234_567_890;
    for _ in 0..WARMUP {
        log::info!("value: {}", val);
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("value: {}", black_box(val));
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

fn bench_single_f64_precision() -> Histogram<u64> {
    let mut hist = new_hist();
    let val: f64 = 3.14159265;
    for _ in 0..WARMUP {
        log::info!("price: {:.4}", val);
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("price: {:.4}", black_box(val));
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

fn bench_str_ref() -> Histogram<u64> {
    let mut hist = new_hist();
    let val: &str = "alice";
    for _ in 0..WARMUP {
        log::info!("name: {}", val);
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("name: {}", black_box(val));
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

fn bench_string_owned() -> Histogram<u64> {
    let mut hist = new_hist();
    let val: String = "bob_the_builder".to_string();
    for _ in 0..WARMUP {
        log::info!("name: {}", val);
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("name: {}", black_box(&val));
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

fn bench_multi_primitives() -> Histogram<u64> {
    let mut hist = new_hist();
    let id: u64 = 99_001;
    let qty: u32 = 500;
    let price: f64 = 42_195.75;
    let active: bool = true;
    for _ in 0..WARMUP {
        log::info!("id={} qty={} price={:.2} active={}", id, qty, price, active);
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!(
            "id={} qty={} price={:.2} active={}",
            black_box(id),
            black_box(qty),
            black_box(price),
            black_box(active)
        );
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct BenchOrder {
    id: u64,
    price: f64,
    qty: u32,
    side: u8,
}

fn bench_debug_struct() -> Histogram<u64> {
    let mut hist = new_hist();
    let order = BenchOrder {
        id: 12345,
        price: 42_195.75,
        qty: 500,
        side: 1,
    };
    for _ in 0..WARMUP {
        log::info!("order: {:?}", order);
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("order: {:?}", black_box(order));
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

fn bench_hex() -> Histogram<u64> {
    let mut hist = new_hist();
    let val: u64 = 0xDEAD_BEEF_CAFE_BABE;
    for _ in 0..WARMUP {
        log::info!("addr: {:#x}", val);
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("addr: {:#x}", black_box(val));
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

fn bench_padded() -> Histogram<u64> {
    let mut hist = new_hist();
    let val: &str = "active";
    for _ in 0..WARMUP {
        log::info!("status: {:>20}", val);
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("status: {:>20}", black_box(val));
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

// ─── Payload Size ───────────────────────────────────────────────────────────

fn bench_payload_size(target_bytes: usize) -> Histogram<u64> {
    let mut hist = new_hist();
    // Construct a message body that produces roughly target_bytes of output.
    // Account for ~80 bytes of overhead (timestamp, level, target, separators).
    let body_len = target_bytes.saturating_sub(80);
    let body: String = "x".repeat(body_len);
    for _ in 0..WARMUP {
        log::info!("{}", body);
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("{}", black_box(body.as_str()));
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

// ─── Level Filtering ────────────────────────────────────────────────────────

fn bench_accepted_level() -> Histogram<u64> {
    let mut hist = new_hist();
    for _ in 0..WARMUP {
        log::info!("accepted message");
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::info!("accepted message");
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

fn bench_filtered_debug() -> Histogram<u64> {
    let mut hist = new_hist();
    for _ in 0..WARMUP {
        log::debug!("filtered debug message");
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::debug!("filtered debug message");
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

fn bench_filtered_trace() -> Histogram<u64> {
    let mut hist = new_hist();
    for _ in 0..WARMUP {
        log::trace!("filtered trace message");
    }
    for _ in 0..SAMPLES {
        let s = rdtscp();
        log::trace!("filtered trace message");
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }
    hist
}

// ─── Multi-Producer Contention ──────────────────────────────────────────────

fn bench_contention(num_producers: usize) -> Histogram<u64> {
    let mut hist = new_hist();
    let running = Arc::new(AtomicBool::new(true));

    // Spawn background producer threads that continuously log.
    let handles: Vec<_> = (1..num_producers)
        .map(|id| {
            let running = Arc::clone(&running);
            std::thread::spawn(move || {
                let mut i = 0u64;
                while running.load(Ordering::Relaxed) {
                    log::info!("background producer {} msg {}", id, i);
                    i = i.wrapping_add(1);
                    // Yield occasionally to prevent starving the consumer.
                    if i % 64 == 0 {
                        std::hint::spin_loop();
                    }
                }
            })
        })
        .collect();

    // Let background threads ramp up.
    std::thread::sleep(Duration::from_millis(50));

    // Warmup the measured producer.
    for _ in 0..WARMUP {
        log::info!("measured producer warmup");
    }

    // Measure.
    for i in 0..SAMPLES {
        let s = rdtscp();
        log::info!("measured producer sample {}", black_box(i));
        let e = rdtscp();
        let _ = hist.record(e.wrapping_sub(s));
    }

    // Stop background threads.
    running.store(false, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }

    hist
}

// ─── Sustained Throughput ───────────────────────────────────────────────────

fn bench_throughput() {
    let n = 1_000_000u64;

    // Warmup
    for i in 0..10_000u64 {
        log::info!("throughput warmup {}", i);
    }
    std::thread::sleep(Duration::from_millis(100));

    let start = Instant::now();
    for i in 0..n {
        log::info!("throughput message {}", black_box(i));
    }
    let elapsed = start.elapsed();

    let msgs_per_sec = n as f64 / elapsed.as_secs_f64();
    println!(
        "  {} messages in {:.2}s = {:.0} msg/s ({:.2} us/msg avg)",
        n,
        elapsed.as_secs_f64(),
        msgs_per_sec,
        elapsed.as_secs_f64() / n as f64 * 1_000_000.0,
    );
}

// ─── Main ───────────────────────────────────────────────────────────────────

fn main() {
    let _guard = InqJetBuilder::default()
        .with_writer(NullWriter)
        .with_log_level(LevelFilter::Info)
        .with_buffer_size(65_536)
        .with_timeout(None)
        .with_color_mode(ColorMode::Never)
        .build()
        .expect("failed to initialize logger");

    // Let consumer thread start and stabilize.
    std::thread::sleep(Duration::from_millis(100));

    println!("=== InqJet Producer Latency Benchmarks ===");
    println!(
        "warmup: {}  samples: {}  units: cycles (rdtscp)",
        WARMUP, SAMPLES
    );
    println!();

    // ── Type Spectrum ──
    println!("Type Spectrum (single-threaded, no contention)");
    print_header();
    print_hist("static &str (no args)", &bench_static_message());
    print_hist("single u64 (Display)", &bench_single_u64());
    print_hist(
        "single f64 {:.4} (precision)",
        &bench_single_f64_precision(),
    );
    print_hist("single &str arg", &bench_str_ref());
    print_hist("single String arg", &bench_string_owned());
    print_hist("4x mixed primitives", &bench_multi_primitives());
    print_hist("struct {:?} (Debug)", &bench_debug_struct());
    print_hist("u64 {:#x} (hex)", &bench_hex());
    print_hist("{:>20} (padded)", &bench_padded());
    println!();

    // ── Payload Size ──
    println!("Payload Size (single-threaded)");
    print_header();
    for &size in &[32, 64, 128, 256, 512, 1024] {
        let label = format!("~{size} byte output");
        print_hist(&label, &bench_payload_size(size));
    }
    println!();

    // ── Level Filtering ──
    // Logger is set to Info, so debug/trace are filtered at the log macro level.
    println!("Level Filtering (global level = Info)");
    print_header();
    print_hist("info! (accepted)", &bench_accepted_level());
    print_hist("debug! (filtered)", &bench_filtered_debug());
    print_hist("trace! (filtered)", &bench_filtered_trace());
    println!();

    // ── Multi-Producer Contention ──
    println!("Multi-Producer Contention");
    print_header();
    print_hist("1 producer (baseline)", &bench_contention(1));
    print_hist("2 producers", &bench_contention(2));
    print_hist("4 producers", &bench_contention(4));
    println!();

    // ── Sustained Throughput ──
    println!("Sustained Throughput (single-threaded, 1M messages)");
    bench_throughput();
    println!();
}
