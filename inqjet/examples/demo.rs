use std::io::Write;

use inqjet::{ArchiveTag, InqJetBuilder, LevelFilter, Pod};

// -- Pod type: zero-cost memcpy through ring buffer --

#[derive(Pod, Debug, Clone)]
struct OrderInfo {
    id: u64,
    price: f64,
    qty: i64,
}

#[derive(Pod, Debug, Clone)]
struct Tick {
    bid: f64,
    ask: f64,
    seq: u64,
}

// -- Archive tags --

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

#[derive(Clone, Copy)]
struct Heartbeat;

impl ArchiveTag for Heartbeat {
    fn write_label(&self, out: &mut dyn Write) {
        let _ = out.write_all(b"HEARTBEAT");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let venue_file = std::fs::File::create("/tmp/inqjet_demo_venue.log")?;
    let heartbeat_file = std::fs::File::create("/tmp/inqjet_demo_heartbeat.log")?;

    let mut builder = InqJetBuilder::default()
        .with_writer(std::io::stdout())
        .with_log_level(LevelFilter::Trace);

    let mut venue_archive = builder.add_archive::<VenueEvent>(venue_file, 65536);
    let mut heartbeat_archive = builder.add_archive::<Heartbeat>(heartbeat_file, 4096);

    let guard = builder.build()?;

    // =====================================================================
    // 1. All log levels
    // =====================================================================
    eprintln!("--- all levels ---");
    inqjet::error!("something went wrong");
    inqjet::warn!("approaching memory limit");
    inqjet::info!("server ready");
    inqjet::debug!("connection pool initialized");
    inqjet::trace!("entering main loop");

    // =====================================================================
    // 2. Custom targets
    // =====================================================================
    eprintln!("--- custom targets ---");
    inqjet::info!(target: "app::startup", "binding to port {}", 8080u32);
    inqjet::error!(target: "app::db", "connection refused: {}", "timeout");
    inqjet::warn!(target: "net::ws::binance", "slow frame: {}ms", 47u64);
    inqjet::debug!(target: "engine::matching", "order book depth: {}", 1247u32);
    inqjet::trace!(target: "hot::path", "tick processed in {}ns", 832u64);

    // =====================================================================
    // 3. Primitive POD types (Tier 1 — memcpy)
    // =====================================================================
    eprintln!("--- POD primitives ---");
    inqjet::info!(
        "u8={} u16={} u32={} u64={}",
        255u8,
        65535u16,
        42u32,
        99999u64
    );
    inqjet::info!(
        "i8={} i16={} i32={} i64={}",
        -1i8,
        -100i16,
        -42i32,
        -99999i64
    );
    inqjet::info!("f32={} f64={}", 3.14f32, 2.718281828f64);
    inqjet::info!("bool true={} false={}", true, false);
    inqjet::info!("u128={}", u128::MAX);
    inqjet::info!("i128={}", i128::MIN);

    // =====================================================================
    // 4. User-defined Pod structs (Tier 1 — memcpy)
    // =====================================================================
    eprintln!("--- Pod structs ---");
    let order = OrderInfo {
        id: 98321,
        price: 42150.75,
        qty: 25,
    };
    inqjet::info!(
        "order filled: id={} price={:.2} qty={}",
        order.id,
        order.price,
        order.qty
    );

    let tick = Tick {
        bid: 42000.50,
        ask: 42001.25,
        seq: 7743921,
    };
    inqjet::debug!(
        "tick: bid={:.2} ask={:.2} seq={}",
        tick.bid,
        tick.ask,
        tick.seq
    );

    // =====================================================================
    // 5. String types (Tier 1.5 — length-prefixed copy)
    // =====================================================================
    eprintln!("--- strings ---");
    inqjet::info!("user {} logged in", "alice");
    inqjet::info!("venue: {}", "BINANCE");

    let owned = String::from("dynamic-session-id-abc123");
    inqjet::info!("session: {}", owned);

    let empty_str = "";
    inqjet::debug!("empty string: [{}]", empty_str);

    let long_str = "the quick brown fox jumps over the lazy dog ".repeat(5);
    inqjet::debug!("long string ({} chars): {}", long_str.len(), long_str);

    // =====================================================================
    // 6. Float formatting
    // =====================================================================
    eprintln!("--- float formatting ---");
    inqjet::info!("price: {:.2}", 42150.756789f64);
    inqjet::info!("precise: {:.8}", std::f64::consts::PI);
    inqjet::info!("scientific: {:e}", 1.23456e-10f64);
    inqjet::info!("scientific upper: {:E}", 9.87654e20f64);
    inqjet::info!("no decimals: {:.0}", 42.999f64);

    // =====================================================================
    // 7. Hex, octal, binary formatting
    // =====================================================================
    eprintln!("--- hex/octal/binary ---");
    inqjet::info!("hex: {:x}", 0xDEADBEEFu32);
    inqjet::info!("HEX: {:X}", 0xCAFEBABEu32);
    inqjet::info!("hex prefixed: {:#x}", 255u32);
    inqjet::info!("hex padded: {:08x}", 0x42u32);
    inqjet::info!("octal: {:o}", 511u32);
    inqjet::info!("binary: {:b}", 0b1010_1100u8);
    inqjet::info!("binary prefixed: {:#b}", 42u8);

    // =====================================================================
    // 8. Width and alignment
    // =====================================================================
    eprintln!("--- width/alignment ---");
    inqjet::info!("right-aligned: [{:>10}]", 42u32);
    inqjet::info!("left-aligned: [{:<10}]", 42u32);
    inqjet::info!("zero-padded: [{:010}]", 42u32);

    // =====================================================================
    // 9. Debug formatting (Tier 2 — fallback / eager format)
    // =====================================================================
    eprintln!("--- debug formatting ---");
    let data = vec![1u32, 2, 3, 4, 5];
    inqjet::info!("vec: {:?}", data);

    let nested: Vec<Vec<u32>> = vec![vec![1, 2], vec![3, 4, 5]];
    inqjet::debug!("nested: {:?}", nested);

    let tuple = (42u32, "hello", true);
    inqjet::debug!("tuple: {:?}", tuple);

    let map: std::collections::HashMap<&str, u32> =
        [("alice", 1), ("bob", 2)].into_iter().collect();
    inqjet::debug!("map: {:?}", map);

    let option_some: Option<u32> = Some(42);
    let option_none: Option<u32> = None;
    inqjet::info!("some={:?} none={:?}", option_some, option_none);

    // =====================================================================
    // 10. Mixed tiers in one call
    // =====================================================================
    eprintln!("--- mixed tiers ---");
    // Pod u64 (T1) + &str (T1.5) + Vec debug (T2)
    inqjet::info!(
        "request #{} from user {} matched: {:?}",
        12345u64,
        "alice",
        vec![100u32, 200, 300]
    );

    // f64 with precision (T1) + String (T1.5) + bool (T1)
    inqjet::info!(
        "trade: price={:.4} symbol={} aggressive={}",
        42150.7568f64,
        "BTC-USD",
        true
    );

    // Multiple strings + pod
    inqjet::info!(
        "{} {} {} from {} status={} latency={}us",
        "POST",
        "/api/v2/orders",
        "HTTP/1.1",
        "10.0.1.42",
        201u32,
        847u64
    );

    // =====================================================================
    // 11. Static messages (no args)
    // =====================================================================
    eprintln!("--- static messages ---");
    inqjet::info!("health check passed");
    inqjet::trace!("entering critical section");
    inqjet::error!("fatal: unrecoverable state");

    // =====================================================================
    // 12. log crate bridge (log-compat feature)
    // =====================================================================
    eprintln!("--- log bridge ---");
    log::error!("bridge error: {}", "something broke");
    log::warn!("bridge warn: retries={}", 3);
    log::info!("bridge info: user={}", "bob");
    log::debug!("bridge debug: {:?}", vec![1, 2, 3]);
    log::trace!("bridge trace");

    // =====================================================================
    // 13. Runtime level change
    // =====================================================================
    eprintln!("--- level change ---");
    inqjet::info!("about to restrict to WARN only");
    inqjet::set_level(LevelFilter::Warn);
    inqjet::trace!("this trace is filtered");
    inqjet::debug!("this debug is filtered");
    inqjet::info!("this info is filtered");
    inqjet::warn!("this warn gets through");
    inqjet::error!("this error gets through");
    inqjet::set_level(LevelFilter::Trace);
    inqjet::info!("back to trace level");

    // =====================================================================
    // 14. Multi-threaded logging
    // =====================================================================
    eprintln!("--- multi-threaded ---");
    let mut handles = Vec::new();
    for tid in 0..4u32 {
        let mut archive_clone = venue_archive.clone();
        handles.push(std::thread::spawn(move || {
            for msg in 0..5u32 {
                inqjet::info!("thread-{} message-{}", tid, msg);
                archive_clone.write(
                    VenueEvent {
                        venue: if tid % 2 == 0 { "BINANCE" } else { "COINBASE" },
                        direction: if msg % 2 == 0 {
                            Direction::Inbound
                        } else {
                            Direction::Outbound
                        },
                    },
                    format!("thread-{tid} msg-{msg}").as_bytes(),
                );
            }
        }));
    }
    for h in handles {
        h.join().expect("thread panicked");
    }

    // =====================================================================
    // 15. Multiple archive streams
    // =====================================================================
    eprintln!("--- multiple archives ---");
    venue_archive.write(
        VenueEvent {
            venue: "BINANCE",
            direction: Direction::Inbound,
        },
        b"8=FIX.4.4|35=W|55=BTC-USD|270=42150.75",
    );
    venue_archive.write(
        VenueEvent {
            venue: "BINANCE",
            direction: Direction::Outbound,
        },
        b"8=FIX.4.4|35=D|55=BTC-USD|44=42149.00|38=1.5",
    );
    venue_archive.write(
        VenueEvent {
            venue: "COINBASE",
            direction: Direction::Inbound,
        },
        b"8=FIX.4.4|35=8|55=ETH-USD|31=2850.25|32=10.0",
    );

    heartbeat_archive.write(Heartbeat, b"seq=1");
    heartbeat_archive.write(Heartbeat, b"seq=2");
    heartbeat_archive.write(Heartbeat, b"seq=3");

    // =====================================================================
    // 16. Unicode / multi-byte content
    // =====================================================================
    eprintln!("--- unicode ---");
    inqjet::info!("greeting: {}", "こんにちは");
    inqjet::info!("emoji: {}", "🚀💰📊");
    inqjet::info!("accented: {}", "café résumé naïve");
    inqjet::info!(
        "mixed: {} traded {} at {:.2}",
        "münchen_desk",
        "BTC€",
        42000.50f64
    );

    // Guard drop joins archiver thread, flushes everything
    drop(guard);

    // Print archive contents
    eprintln!("\n=== venue archive ===");
    eprint!("{}", std::fs::read_to_string("/tmp/inqjet_demo_venue.log")?);

    eprintln!("\n=== heartbeat archive ===");
    eprint!(
        "{}",
        std::fs::read_to_string("/tmp/inqjet_demo_heartbeat.log")?
    );

    Ok(())
}
