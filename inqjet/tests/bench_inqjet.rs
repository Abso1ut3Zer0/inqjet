//! InqJet benchmarks: log::info! bridge vs inqjet::info! hot-path.
//!
//! Run with:
//! ```bash
//! cargo test -p inqjet --release bench_inqjet -- --ignored --nocapture
//! ```

use std::io::BufWriter;

use hdrhistogram::Histogram;
use inqjet::{ColorMode, InqJetBuilder, LevelFilter};

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
fn bench_inqjet() {
    let tmp = std::env::temp_dir().join("inqjet_bench.log");
    println!("Writing to {}", tmp.display());
    let file = std::fs::OpenOptions::new()
        .truncate(true)
        .create(true)
        .write(true)
        .open(tmp)
        .expect("Failed to open file");
    let _guard = InqJetBuilder::default()
        .with_writer(BufWriter::new(file))
        .with_log_level(LevelFilter::Info)
        .with_buffer_size(262_144) // 256KB ring buffer
        .with_timeout(None)
        .with_color_mode(ColorMode::Never)
        .build()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(100));

    let n = 100_000;

    // -- log::info! bridge path ------------------------------------------------

    {
        let mut hist = Histogram::<u64>::new_with_bounds(1, 60 * 1000 * 1000, 3).unwrap();

        println!("Warming up log::info! bridge...");
        for i in 0..100 {
            log::info!("warmup message {}", i);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));

        println!(
            "Starting log::info! bridge benchmark with {} iterations...",
            n
        );
        for i in 0..n {
            let start = std::time::Instant::now();
            log::info!("logging to inqjet logger! msg number: {}", i);
            let elapsed = start.elapsed();
            hist.record(elapsed.as_nanos() as u64).unwrap();

            if i % 1000 == 0 {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(200));
        print_histogram_stats("InqJet (log::info! bridge)", &hist);
    }

    // -- inqjet::info! hot-path ------------------------------------------------

    {
        let mut hist = Histogram::<u64>::new_with_bounds(1, 60 * 1000 * 1000, 3).unwrap();

        println!("Warming up inqjet::info! hot-path...");
        for i in 0..100 {
            inqjet::info!("warmup message {}", i);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));

        println!(
            "Starting inqjet::info! hot-path benchmark with {} iterations...",
            n
        );
        for i in 0..n {
            let start = std::time::Instant::now();
            inqjet::info!("logging to inqjet logger! msg number: {}", i);
            let elapsed = start.elapsed();
            hist.record(elapsed.as_nanos() as u64).unwrap();

            if i % 1000 == 0 {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(1));
        print_histogram_stats("InqJet (inqjet::info! hot-path)", &hist);
    }
}
