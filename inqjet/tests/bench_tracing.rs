//! Tracing comprehensive scenario benchmarks.
//!
//! Mirrors the scenarios in `bench_inqjet.rs` for direct comparison.
//! Tracing formats on the caller thread; inqjet defers formatting to
//! the consumer.
//!
//! Run with:
//! ```bash
//! cargo test -p inqjet --release bench_tracing -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use tracing::Level;

const USERS: &[&str] = &["alice", "bob", "charlie", "diana", "eve"];
const PATHS: &[&str] = &["/api/v1/users", "/api/v1/orders", "/health", "/api/v2/data"];
const IPS: &[&str] = &["10.0.1.1", "192.168.1.42", "172.16.0.100", "10.0.2.55"];
const ACTIONS: &[&str] = &["read", "write", "delete", "update"];

fn make_sessions(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("sess-{:08x}", i * 7 + 13)).collect()
}

macro_rules! bench {
    ($name:expr, $results:ident, $n:expr, |$i:ident| $body:expr) => {{
        for $i in 0..200usize {
            let _ = $i;
            $body;
        }
        std::thread::sleep(Duration::from_millis(100));

        let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
        for $i in 0..$n {
            let start = Instant::now();
            $body;
            hist.record(start.elapsed().as_nanos() as u64).unwrap();
            if $i % 1000 == 0 {
                std::thread::sleep(Duration::from_millis(25));
            }
        }

        std::thread::sleep(Duration::from_millis(200));
        $results.push(($name, hist));
    }};
}

fn print_table(header: &str, results: &[(&str, Histogram<u64>)]) {
    println!("\n{header}");
    println!("{}", "=".repeat(header.len()));
    println!("100,000 iterations per scenario | non_blocking(262K) | file sink\n");
    println!(
        "{:<24} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "Scenario", "p50", "p90", "p99", "p99.9", "max"
    );
    println!(
        "{:-<24} {:->8} {:->8} {:->8} {:->8} {:->8}",
        "", "", "", "", "", ""
    );
    for (name, hist) in results {
        println!(
            "{:<24} {:>6.2}us {:>6.2}us {:>6.2}us {:>6.2}us {:>6.2}us",
            name,
            hist.value_at_quantile(0.50) as f64 / 1000.0,
            hist.value_at_quantile(0.90) as f64 / 1000.0,
            hist.value_at_quantile(0.99) as f64 / 1000.0,
            hist.value_at_quantile(0.999) as f64 / 1000.0,
            hist.max() as f64 / 1000.0,
        );
    }
    println!();
}

#[ignore]
#[test]
fn bench_tracing() {
    let tmp = std::env::temp_dir().join("tracing_scenario_bench.log");
    let file = std::fs::OpenOptions::new()
        .truncate(true)
        .create(true)
        .write(true)
        .open(&tmp)
        .unwrap();
    let (writer, _nb_guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(262_144)
        .finish(file);
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_writer(writer)
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    std::thread::sleep(Duration::from_millis(200));

    let sessions = make_sessions(256);
    let data: Vec<u32> = vec![1, 2, 3, 4, 5];
    let n = 100_000usize;
    let mut results: Vec<(&str, Histogram<u64>)> = Vec::new();

    // 1. Static message
    bench!("static_msg", results, n, |i| {
        tracing::info!("health check passed")
    });

    // 2. Single integer
    bench!("single_int", results, n, |i| {
        tracing::info!("processed request in {}ms", i as u64)
    });

    // 3. Single &str
    bench!("single_str", results, n, |i| {
        tracing::info!("user {} connected", USERS[i % USERS.len()])
    });

    // 4. Single owned String
    bench!("single_string", results, n, |i| {
        tracing::info!("session {}", sessions[i % sessions.len()])
    });

    // 5. Realistic 4-arg request log
    bench!("realistic_4arg", results, n, |i| {
        tracing::info!(
            "{} {} from {} status {}",
            "GET",
            PATHS[i % PATHS.len()],
            IPS[i % IPS.len()],
            200u32
        )
    });

    // 6. Three POD fields
    bench!("pod_triple", results, n, |i| {
        tracing::info!("order id={} price={} qty={}", i as u64, 99.95f64, 100i64)
    });

    // 7. Float with precision spec
    bench!("float_precision", results, n, |i| {
        tracing::info!("latency: {:.4}ms", (i as f64) * 0.001 + 0.5)
    });

    // 8. Debug on Vec<u32>
    bench!("debug_vec", results, n, |i| {
        tracing::info!("state: {:?}", data)
    });

    // 9. Mixed Debug + Display
    bench!("debug_display_mix", results, n, |i| {
        tracing::info!(
            "request from user {:?} on instance {}: {:?}",
            USERS[i % USERS.len()],
            i as u64,
            data
        )
    });

    // 10. Verbose 6-arg audit trail
    bench!("verbose_6arg", results, n, |i| {
        tracing::info!(
            "audit: user={} action={} resource={} from={} status={} latency={}ms",
            USERS[i % USERS.len()],
            ACTIONS[i % ACTIONS.len()],
            PATHS[i % PATHS.len()],
            IPS[i % IPS.len()],
            200u32,
            i as u64
        )
    });

    print_table("Tracing Scenario Results (producer-side latency)", &results);
}
