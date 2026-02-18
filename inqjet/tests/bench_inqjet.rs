//! InqJet comprehensive scenario benchmarks.
//!
//! Measures producer-side latency across diverse message types and argument
//! combinations. Run alongside `bench_tracing` for comparison.
//!
//! Run with:
//! ```bash
//! cargo test -p inqjet --release bench_inqjet -- --ignored --nocapture
//! ```

use std::io::{BufWriter, Write};
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use inqjet::{ArchiveTag, ColorMode, InqJetBuilder, LevelFilter};

const USERS: &[&str] = &["alice", "bob", "charlie", "diana", "eve"];
const PATHS: &[&str] = &["/api/v1/users", "/api/v1/orders", "/health", "/api/v2/data"];
const IPS: &[&str] = &["10.0.1.1", "192.168.1.42", "172.16.0.100", "10.0.2.55"];
const ACTIONS: &[&str] = &["read", "write", "delete", "update"];

fn make_sessions(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("sess-{:08x}", i * 7 + 13)).collect()
}

// -- Archive tag types for benchmarking --

#[derive(Clone, Copy)]
struct SimpleTag(&'static str);

impl ArchiveTag for SimpleTag {
    fn write_label(&self, out: &mut dyn Write) {
        let _ = out.write_all(self.0.as_bytes());
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    const fn as_str(self) -> &'static str {
        match self {
            Direction::Inbound => "INBOUND",
            Direction::Outbound => "OUTBOUND",
        }
    }
}

#[derive(Clone, Copy)]
struct VenueEvent {
    venue: &'static str,
    direction: Direction,
}

impl ArchiveTag for VenueEvent {
    fn write_label(&self, out: &mut dyn Write) {
        let _ = write!(out, "{} {}", self.venue, self.direction.as_str());
    }
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
    println!("100,000 iterations per scenario | 1MB ring buffer | busy-spin consumer\n");
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
fn bench_inqjet() {
    let tmp = std::env::temp_dir().join("inqjet_scenario_bench.log");
    let file = std::fs::OpenOptions::new()
        .truncate(true)
        .create(true)
        .write(true)
        .open(&tmp)
        .unwrap();

    let archive_tmp = std::env::temp_dir().join("inqjet_archive_bench.log");
    let archive_file = std::fs::OpenOptions::new()
        .truncate(true)
        .create(true)
        .write(true)
        .open(&archive_tmp)
        .unwrap();

    let mut builder = InqJetBuilder::default()
        .with_writer(BufWriter::new(file))
        .with_log_level(LevelFilter::Trace)
        .with_buffer_size(1 << 20) // 1MB ring buffer
        .with_timeout(None) // busy-spin consumer
        .with_color_mode(ColorMode::Never);

    let mut simple_archive =
        builder.add_archive::<SimpleTag>(BufWriter::new(archive_file), 1 << 20);
    let mut venue_archive = builder.add_archive::<VenueEvent>(std::io::sink(), 1 << 20);

    let _guard = builder.build().unwrap();

    std::thread::sleep(Duration::from_millis(200));

    let sessions = make_sessions(256);
    let data: Vec<u32> = vec![1, 2, 3, 4, 5];
    let n = 100_000usize;
    let mut results: Vec<(&str, Histogram<u64>)> = Vec::new();

    // 1. Static message — no format args, minimal payload
    bench!("static_msg", results, n, |i| {
        inqjet::info!("health check passed")
    });

    // 2. Single integer — Pod memcpy path (8 bytes)
    bench!("single_int", results, n, |i| {
        inqjet::info!("processed request in {}ms", i as u64)
    });

    // 3. Single &str — Tier 1.5 direct-copy (length-prefixed)
    bench!("single_str", results, n, |i| {
        inqjet::info!("user {} connected", USERS[i % USERS.len()])
    });

    // 4. Single owned String — Tier 1.5 direct-copy
    bench!("single_string", results, n, |i| {
        inqjet::info!("session {}", sessions[i % sessions.len()])
    });

    // 5. Realistic 4-arg request log — mixed &str and u32
    bench!("realistic_4arg", results, n, |i| {
        inqjet::info!(
            "{} {} from {} status {}",
            "GET",
            PATHS[i % PATHS.len()],
            IPS[i % IPS.len()],
            200u32
        )
    });

    // 6. Three POD fields — order-like (u64 + f64 + i64 = 24 bytes)
    bench!("pod_triple", results, n, |i| {
        inqjet::info!("order id={} price={} qty={}", i as u64, 99.95f64, 100i64)
    });

    // 7. Float with precision spec — Pod on producer, {:.4} on consumer
    bench!("float_precision", results, n, |i| {
        inqjet::info!("latency: {:.4}ms", (i as f64) * 0.001 + 0.5)
    });

    // 8. Debug on Vec<u32> — Tier 2 fallback (eager format to stash)
    bench!("debug_vec", results, n, |i| {
        inqjet::info!("state: {:?}", data)
    });

    // 9. Mixed Debug + Display — all three tiers in one call
    //    &str {:?} = Tier 1.5, u64 {} = Tier 1, Vec {:?} = Tier 2
    bench!("debug_display_mix", results, n, |i| {
        inqjet::info!(
            "request from user {:?} on instance {}: {:?}",
            USERS[i % USERS.len()],
            i as u64,
            data
        )
    });

    // 10. Verbose 6-arg audit trail — stress test
    bench!("verbose_6arg", results, n, |i| {
        inqjet::info!(
            "audit: user={} action={} resource={} from={} status={} latency={}ms",
            USERS[i % USERS.len()],
            ACTIONS[i % ACTIONS.len()],
            PATHS[i % PATHS.len()],
            IPS[i % IPS.len()],
            200u32,
            i as u64
        )
    });

    // 11. log::info! bridge path — for comparison with hot-path above
    bench!("log_bridge", results, n, |i| {
        log::info!("processed request in {}ms", i)
    });

    print_table("InqJet Scenario Results (producer-side latency)", &results);

    // -- Archive benchmarks --------------------------------------------------
    let small_payload = b"heartbeat";
    let fix_payload = b"8=FIX.4.4|9=148|35=D|49=SENDER|56=TARGET|34=12345|52=20240115-14:30:45.123|55=BTC-USD|54=1|44=42000.50|38=1.5|40=2|10=042|";

    let mut archive_results: Vec<(&str, Histogram<u64>)> = Vec::new();

    // 12. Simple tag + small payload (heartbeat)
    bench!("archive_simple", archive_results, n, |i| {
        simple_archive.write(SimpleTag("HEARTBEAT"), small_payload)
    });

    // 13. Composite tag + small payload
    bench!("archive_venue_small", archive_results, n, |i| {
        venue_archive.write(
            VenueEvent {
                venue: "BINANCE",
                direction: Direction::Inbound,
            },
            small_payload,
        )
    });

    // 14. Composite tag + FIX-sized payload (~140 bytes)
    bench!("archive_venue_fix", archive_results, n, |i| {
        venue_archive.write(
            VenueEvent {
                venue: "BINANCE",
                direction: Direction::Outbound,
            },
            fix_payload,
        )
    });

    print_table(
        "InqJet Archive Results (producer-side latency)",
        &archive_results,
    );
}
