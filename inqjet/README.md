# InqJet

High-performance logging for latency-critical Rust applications.

InqJet defers formatting to a background thread. The producer writes raw bytes
to a lock-free ring buffer, not formatted strings. The hot-path cost is
independent of message complexity.

## Performance

**6-20x faster than tracing** across realistic logging scenarios (p50, single producer):

| Scenario | inqjet | tracing | Speedup |
|---|---:|---:|---:|
| Static message | 0.16us | 0.99us | 6.2x |
| Single integer | 0.11us | 1.03us | 9.4x |
| Single &str | 0.17us | 1.07us | 6.3x |
| Realistic 4-arg | 0.18us | 1.04us | 5.8x |
| Float `{:.4}` | 0.08us | 1.62us | 20.3x |
| Debug `{:?}` Vec | 0.46us | 1.20us | 2.6x |
| Verbose 6-arg | 0.21us | 1.31us | 6.2x |

See [BENCHMARKS.md](../BENCHMARKS.md) for full results including tail latency
and methodology.

## Quick Start

```rust
use inqjet::{InqJetBuilder, LevelFilter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = InqJetBuilder::default()
        .with_writer(std::io::stdout())
        .with_log_level(LevelFilter::Info)
        .build()?;

    // Native macros — bypass the log facade, direct to ring buffer:
    inqjet::info!("Server started on port {}", 8080);
    inqjet::error!("Connection failed: {}", "timeout");

    // log crate macros also work (with `log-compat` feature, on by default):
    // log::info!("This also works");

    // Guard drop joins the archiver thread and flushes remaining records.
    Ok(())
}
```

## Why It's Fast

### Don't Format on the Hot Path

Traditional loggers (tracing, env_logger, slog) format the log message on the
caller's thread. For `info!("price: {:.4}", 99.95)`, the caller pays the cost
of float-to-string conversion before the message is handed off.

InqJet flips this. The producer writes the raw f64 bytes (8-byte memcpy) to a
ring buffer. A background archiver thread reads those bytes and formats the
output. The producer never calls `Display::fmt`.

### Three-Tier Argument Encoding

A proc macro (`inqjet::info!()`) analyzes the format string at compile time and
generates per-argument encoding via autoref dispatch:

| Tier | Types | Producer cost | Mechanism |
|------|-------|---------------|-----------|
| 1. Pod | Primitives, user structs | `memcpy` | `copy_nonoverlapping` to ring buffer |
| 1.5. String | `&str`, `String` | Length-prefix + byte copy | `[len: u32][bytes]` |
| 2. Fallback | Everything else | Eager `format_args!` | Format to TLS buffer, copy as bytes |

Tiers 1 and 1.5 are the common cases. Only Tier 2 (Debug on complex types)
formats on the producer — and even then, inqjet's ring buffer transport is
faster than tracing's channel.

### Architecture

```
Producer Thread                       Archiver Thread
---------------                       ---------------
inqjet::info!("msg: {}", val)         loop {
  |-- level check (AtomicU8 load)       record = consumer.try_read()
  |-- snap timestamp (u64 ns)           |-- read header (24 bytes)
  |-- encode args as raw bytes          |-- extract fn_ptr, timestamp, level
  |-- claim ring buffer space           +-- (fn_ptr)(ts, lvl, payload, writer)
  |-- memcpy header + payload           flush writer
  +-- commit (atomic store)             park_timeout if idle
```

Key design decisions:

- **Ring buffer, not channel.** `nexus-logbuf` MPSC ring buffer. Fixed
  allocation, no per-message malloc. CAS-based multi-producer, single consumer.
- **Thread-local producers.** Each thread lazily clones a producer on first log
  call. After that, pure thread-local — zero contention on the producer path.
- **Function pointer dispatch.** Each record header carries a
  `fn(ts, level, &[u8], &mut Write)`. The consumer calls it to format the
  payload. No vtable, no dynamic dispatch beyond the fn ptr.
- **Level gating before format_args.** The `AtomicU8` check happens before
  `format_args!()` evaluates its arguments. Filtered messages cost ~8ns.

## Pod Types

Mark your structs for zero-cost logging via memcpy:

```rust
use inqjet::Pod;

#[derive(Pod, Debug)]
struct OrderInfo {
    id: u64,
    price: f64,
    qty: i64,
}

// Producer copies 24 bytes. Consumer formats with the original format string.
inqjet::info!("order: id={} price={:.2} qty={}", order.id, order.price, order.qty);
```

`#[derive(Pod)]` enforces at compile time that the type has no `Drop` impl.

## Configuration

```rust
use inqjet::{InqJetBuilder, LevelFilter, ColorMode, BackpressureMode};
use std::time::Duration;

let _guard = InqJetBuilder::default()
    .with_writer(std::io::stdout())
    .with_log_level(LevelFilter::Info)
    .with_buffer_size(1 << 20)                       // 1MB ring buffer (default: 64KB)
    .with_timeout(Some(Duration::from_millis(5)))     // Archiver park timeout (default)
    .with_color_mode(ColorMode::Auto)                 // Auto / Always / Never
    .with_backpressure(BackpressureMode::Backoff)     // Backoff (default) or Drop
    .build()?;
```

### Buffer Size

Ring buffer size in bytes (rounded up to next power of two). Default: 64KB.

- **64KB**: Low-memory, consistent message rate
- **256KB-1MB**: Recommended for bursty workloads
- **4MB+**: High-throughput, many producers

### Backpressure

When the ring buffer is full:

- **`Backoff`** (default): Exponential backoff via `crossbeam::Backoff`. Spins
  briefly, then yields. Guarantees delivery at the cost of variable latency
  under pressure.
- **`Drop`**: Drop the message and return immediately. Bounded producer latency,
  no delivery guarantee.

### Ultra-Low Latency Mode

Busy-spin the archiver thread (dedicates a CPU core):

```rust
let _guard = InqJetBuilder::default()
    .with_writer(std::io::stdout())
    .with_log_level(LevelFilter::Info)
    .with_timeout(None)  // Busy-spin, never park
    .build()?;
```

## Archive Streams

Separate ring buffers for structured event archival — like FIX session logs.
Each archive stream has its own writer, independent of the main log output.

```rust
use inqjet::{ArchiveTag, InqJetBuilder, LevelFilter};
use std::io::Write;

#[derive(Clone, Copy)]
enum Direction { Inbound, Outbound }

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let archive_file = std::fs::File::create("binance.log")?;

    let mut builder = InqJetBuilder::default()
        .with_writer(std::io::stdout())
        .with_log_level(LevelFilter::Info);

    let mut archive = builder.add_archive::<VenueEvent>(archive_file, 65536);
    let _guard = builder.build()?;

    // Normal logging → stdout
    inqjet::info!("server started");

    // Archive records → binance.log
    archive.write(
        VenueEvent { venue: "BINANCE", direction: Direction::Inbound },
        b"8=FIX.4.4|35=W|55=BTC-USD|270=42000.50",
    );

    Ok(())
}
```

Output in `binance.log`:
```
2024-01-15T14:30:45.123456789Z [BINANCE INBOUND] 8=FIX.4.4|35=W|55=BTC-USD|270=42000.50
```

The tag is memcpy'd as raw bytes through the ring buffer. `write_label` runs on
the background archiver thread — never on the hot path. Clone the handle for
use across multiple threads.

### Runtime Level Adjustment

```rust
inqjet::set_level(LevelFilter::Debug);
```

Takes effect immediately for all subsequent log calls.

## Feature Flags

| Flag | Default | Description |
|------|---------|-------------|
| `log-compat` | Yes | Enables `log::Log` bridge so `log::info!()` routes through inqjet. |

Without `log-compat`, only the native `inqjet::info!()` macros are available.
The native macros are always faster than the bridge path.

## Log Format

```
2024-01-15T14:30:45.123456789Z [INFO] my_app::auth:127 User alice logged in
2024-01-15T14:30:45.124001234Z [ERROR] my_app::db Connection failed: timeout
```

ISO 8601 UTC timestamps with nanosecond precision. ANSI colors when writing to
a terminal (respects `NO_COLOR` and `TERM=dumb`).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
