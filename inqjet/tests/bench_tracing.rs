//! Tracing comparison benchmark.
//!
//! Run with:
//! ```bash
//! cargo test -p inqjet --release bench_tracing -- --ignored --nocapture
//! ```

use std::io;

use hdrhistogram::Histogram;
use tracing::Level;
use tracing_log::LogTracer;
use tracing_subscriber::fmt;

fn print_histogram_stats(name: &str, hist: &Histogram<u64>) {
    println!("\n=== {} Performance Profile ===", name);
    println!("Count: {}", hist.len());
    println!("Min: {:>8.2}us", hist.min() as f64 / 1000.0);
    println!("Mean: {:>7.2}us", hist.mean() / 1000.0);
    println!("Max: {:>8.2}us", hist.max() as f64 / 1000.0);
    println!("StdDev: {:>5.2}us", hist.stdev() / 1000.0);
    println!();
    println!("Percentiles:");
    println!(
        "  50th: {:>6.2}us",
        hist.value_at_quantile(0.50) as f64 / 1000.0
    );
    println!(
        "  90th: {:>6.2}us",
        hist.value_at_quantile(0.90) as f64 / 1000.0
    );
    println!(
        "  95th: {:>6.2}us",
        hist.value_at_quantile(0.95) as f64 / 1000.0
    );
    println!(
        "  99th: {:>6.2}us",
        hist.value_at_quantile(0.99) as f64 / 1000.0
    );
    println!(
        " 99.9th: {:>5.2}us",
        hist.value_at_quantile(0.999) as f64 / 1000.0
    );
    println!(
        " 99.99th: {:>4.2}us",
        hist.value_at_quantile(0.9999) as f64 / 1000.0
    );
    println!();
}

#[ignore]
#[test]
fn bench_tracing() {
    LogTracer::init().unwrap();
    let builder =
        tracing_appender::non_blocking::NonBlockingBuilder::default().buffered_lines_limit(262_144);
    let (writer, _guard) = builder.finish(io::stdout());

    let subscriber = fmt::Subscriber::builder()
        .with_writer(writer)
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(100));

    let n = 100_000;
    let mut hist = Histogram::<u64>::new_with_bounds(1, 60 * 1000 * 1000, 3).unwrap();

    println!("Warming up Tracing...");
    for i in 0..100 {
        log::info!("warmup message {}", i);
    }
    std::thread::sleep(std::time::Duration::from_millis(50));

    println!("Starting Tracing benchmark with {} iterations...", n);
    for i in 0..n {
        let start = std::time::Instant::now();
        log::info!("logging to tracing logger! msg number: {}", i);
        let elapsed = start.elapsed();
        hist.record(elapsed.as_nanos() as u64).unwrap();

        if i % 1000 == 0 {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    std::thread::sleep(std::time::Duration::from_secs(1));
    print_histogram_stats("Tracing", &hist);
}
