# InqJet Benchmarks

## Running

### Quick

```bash
cargo run --release --example bench_producer_latency
```

### Accurate (recommended)

```bash
# Disable turbo boost (Intel)
echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo
# Disable turbo boost (AMD)
echo 0 | sudo tee /sys/devices/system/cpu/cpufreq/boost

# Pin to physical cores — avoids hyperthread sibling contention.
# Core 0 for producer (main thread), core 2 for consumer (background).
# Adjust core IDs based on your topology (check: lscpu -e).
taskset -c 0,2 cargo run --release --example bench_producer_latency
```

### Comparison with tracing

```bash
cargo test --release bench_tracing -- --ignored --nocapture
```

---

## Methodology

### Timing: rdtscp

All per-operation measurements use `rdtscp` (Read Time-Stamp Counter and
Processor ID). This is a serializing instruction on x86-64 that reads the CPU's
cycle counter after all prior instructions have completed.

Cost of the measurement itself: ~18-20 cycles. This is included in every sample
but consistent, so it shifts all percentiles by a fixed offset without affecting
the distribution shape.

Non-x86-64 platforms fall back to `Instant::now()` (~25-50ns, less precise).

### Recording: HDR Histogram

All latency distributions are recorded in an HDR histogram with 3 significant
digits of precision. This provides accurate percentile computation across the
full range without fixed-width bucketing artifacts.

### Warmup

Each benchmark runs 10,000 warmup iterations before measurement. This ensures:
- CPU caches (L1/L2/L3) are hot for the access pattern
- Branch predictors have stabilized
- TLB entries are populated
- Allocator freelists are warm
- JIT-like effects in the log dispatch path have settled

### Samples

100,000 measured samples per benchmark. Enough for stable p999 estimates
(100 samples in the tail bucket).

### Isolation

- **NullWriter**: Consumer writes to a no-op writer (no syscalls, no I/O
  variance). This isolates pure producer cost from consumer I/O behavior.
- **Large capacity** (65,536): Channel never fills during measurement, so
  producer never hits backpressure.
- **Colors disabled**: No ANSI escape sequence formatting overhead.
- **`black_box()`**: Prevents the compiler from constant-folding values or
  eliminating dead code in the measured path.

---

## What's Measured

### Producer Latency

The wall-to-wall cost of a `log::info!(...)` call on the application thread.

**Includes:**
- `log` crate dispatch (max_level check, Logger::log call)
- Level filter check inside Logger
- `SystemTime::now()` timestamp snap
- String pool acquisition
- `format_args!` evaluation and string formatting
- Channel push (crossbeam ArrayQueue, single atomic CAS)
- String pool return (on consumer side, not measured here)

**Excludes:**
- Consumer-side I/O (NullWriter)
- Channel backpressure (large capacity, fast consumer)

### Filtered Message Cost

The cost of a `log::debug!()` or `log::trace!()` call when the global level
filter is set to `Info`. These are rejected at the `log` crate's `max_level()`
check before reaching InqJet's `Logger::log()`.

Expected cost: ~1-10ns (single atomic load + branch).

### Multi-Producer Contention

Measures the producer latency of one thread while N-1 other threads are
simultaneously logging. Shows the impact of CAS contention on the lock-free
channel under concurrent access.

### Sustained Throughput

Total messages per second for a sustained burst. Uses wall-clock timing
(`Instant`), not per-message rdtscp. Includes any backpressure effects that
emerge during sustained load.

---

## Benchmark Matrix

### Type Spectrum

Measures how formatting cost varies across types and format specifiers.
Single-threaded, no contention.

| Benchmark | Format | What it isolates |
|-----------|--------|-----------------|
| static &str (no args) | `"checkpoint reached"` | Baseline: no formatting |
| single u64 | `"value: {}", u64` | Integer Display |
| single f64 `{:.4}` | `"price: {:.4}", f64` | Float with precision |
| single &str arg | `"name: {}", &str` | String interpolation |
| single String arg | `"name: {}", String` | Owned string interpolation |
| 4x mixed primitives | `"id={} qty={} price={:.2} active={}"` | Multi-arg formatting |
| struct `{:?}` | `"order: {:?}", struct` | Debug trait formatting |
| u64 `{:#x}` | `"addr: {:#x}", u64` | Hex with prefix |
| `{:>20}` padded | `"status: {:>20}", &str` | Alignment/padding |

### Payload Size

Measures how output length affects producer cost. Uses pre-constructed strings
of target lengths to isolate the write-more-bytes cost from formatting
complexity.

| Size | Typical use |
|------|------------|
| ~32 bytes | Short status message |
| ~64 bytes | Typical one-liner |
| ~128 bytes | Standard log line with context |
| ~256 bytes | Verbose message |
| ~512 bytes | Detailed error with context |
| ~1024 bytes | Debug dump, stack trace excerpt |

### Level Filtering

| Benchmark | Expected cost |
|-----------|--------------|
| `info!` (accepted) | Full producer cost (~1-5us) |
| `debug!` (filtered) | ~1-10ns (macro-level filter) |
| `trace!` (filtered) | ~1-10ns (macro-level filter) |

### Multi-Producer Contention

| Producers | What it shows |
|-----------|--------------|
| 1 (baseline) | Uncontended producer latency |
| 2 | CAS retry impact with one competitor |
| 4 | CAS retry impact with three competitors |

---

## Understanding Results

### Units

Output is in **CPU cycles** (raw rdtscp values). To convert to nanoseconds:

```
nanoseconds ≈ cycles / CPU_base_frequency_GHz
```

Example: On a 3.0 GHz CPU, 300 cycles ≈ 100ns ≈ 0.1μs.

To find your base frequency:
```bash
lscpu | grep "Model name"
# or
cat /proc/cpuinfo | grep "model name" | head -1
```

### Percentiles

| Percentile | Meaning | Affected by |
|------------|---------|-------------|
| p50 | Median. Typical steady-state cost. | Algorithm, data layout |
| p75 | 75th. Most calls cost less than this. | Algorithm, cache behavior |
| p95 | 1 in 20. Start of tail. | L2/L3 cache misses |
| p99 | 1 in 100. Significant tail. | TLB misses, allocator |
| p999 | 1 in 1,000. Extreme tail. | Preemption, page faults |
| max | Worst observed. Usually an outlier. | Scheduler, interrupts, SMIs |

### Red Flags

- **p50 >> expected**: Something fundamental is wrong (wrong writer, level
  misconfigured, debug build).
- **p99 >> 10x p50**: Severe tail latency. Look for allocations, lock
  contention, or page faults on the producer path.
- **p999 >> 100x p50**: Likely OS interference (context switches, interrupts).
  Check core pinning and interrupt affinity.
- **max >> 1,000x p50**: Normal for long-running benchmarks. A scheduler
  preemption or SMI (System Management Interrupt) snuck in.

---

## Baseline Results: Current Implementation (crossbeam transport)

Measured on: (run `lscpu | grep "Model name"` to record your CPU)

### Type Spectrum

| Benchmark | p50 | p75 | p95 | p99 | p999 | max |
|-----------|-----|-----|-----|-----|------|-----|
| static &str (no args) | 868 | 992 | 1138 | 1292 | 2149 | 25215 |
| single u64 (Display) | 942 | 1076 | 1184 | 1354 | 2423 | 28303 |
| single f64 {:.4} | 1478 | 1604 | 1792 | 2547 | 6963 | 35071 |
| single &str arg | 952 | 1074 | 1204 | 1422 | 2953 | 19839 |
| single String arg | 964 | 1080 | 1204 | 1400 | 5991 | 26495 |
| 4x mixed primitives | 1602 | 1744 | 1904 | 2481 | 7535 | 24431 |
| struct {:?} (Debug) | 1378 | 1518 | 1680 | 2079 | 7223 | 31231 |
| u64 {:#x} (hex) | 1012 | 1118 | 1248 | 1464 | 2339 | 30015 |
| {:>20} (padded) | 1064 | 1204 | 1328 | 1536 | 6303 | 46271 |

**Observations:**
- Baseline floor (static message): ~868 cycles = timestamp + pool get + header format + channel push
- Integer formatting adds ~74 cycles over baseline
- f64 with precision is the most expensive single-type formatter (+610 cycles)
- &str and String are nearly identical (both just copy bytes via Display)
- 4x mixed args dominated by the f64 component

### Payload Size

| Size | p50 | p75 | p95 | p99 | p999 | max |
|------|-----|-----|-----|-----|------|-----|
| ~32B | 952 | 1068 | 1202 | 1780 | 4323 | 29263 |
| ~64B | 958 | 1076 | 1218 | 1572 | 3037 | 44767 |
| ~128B | 936 | 1066 | 1188 | 1368 | 4367 | 27167 |
| ~256B | 1016 | 1102 | 1224 | 1472 | 2815 | 50431 |
| ~512B | 1408 | 2004 | 2565 | 2979 | 9815 | 235775 |
| ~1024B | 1996 | 2195 | 2821 | 5243 | 13767 | 962047 |

**Observations:**
- Flat from 32-256 bytes: formatting overhead dominates, not payload copy
- Inflection at ~512 bytes: string operations start to matter
- 1024B is ~2x the cost of 256B — scales with memcpy/write volume
- Tail latency (p999, max) degrades sharply at large sizes

### Level Filtering

| Benchmark | p50 | p75 | p95 | p99 | p999 | max |
|-----------|-----|-----|-----|-----|------|-----|
| info! (accepted) | 932 | 1058 | 1174 | 1372 | 2461 | 43327 |
| debug! (filtered) | 26 | 26 | 28 | 28 | 42 | 2413 |
| trace! (filtered) | 24 | 26 | 26 | 28 | 38 | 622 |

**Observations:**
- Filtered messages cost ~26 cycles — effectively free (single atomic load + branch)
- Ratio: accepted/filtered = ~36x. Level filtering is extremely effective.

### Multi-Producer Contention

| Producers | p50 | p75 | p95 | p99 | p999 | max |
|-----------|-----|-----|-----|-----|------|-----|
| 1 (baseline) | 884 | 990 | 1288 | 1552 | 2185 | 65247 |
| 2 producers | 1354 | 1410 | 1760 | 3121 | 5979 | 43263 |
| 4 producers | 1424 | 1576 | 2181 | 15303 | 45055 | 7020543 |

**Observations:**
- 2 producers: p50 +53%, p999 +174%. Moderate CAS contention.
- 4 producers: p50 +61%, but **p99 explodes to 15K** and p999 to 45K.
  Tail latency is the real casualty of contention — the median holds up
  but the worst cases are 20-30x worse.
- Max at 4 producers (7M cycles) is likely a scheduler preemption while
  holding a CAS retry loop.

### Sustained Throughput

| Metric | Value |
|--------|-------|
| Messages | 1,000,000 |
| Duration | 0.36s |
| Throughput | 2,762,265 msg/s |
| Avg latency | 0.36 us/msg |

---

## After nexus-logbuf Migration

| Benchmark | p50 | p75 | p95 | p99 | p999 | max | vs baseline p50 |
|-----------|-----|-----|-----|-----|------|-----|-----------------|
| (to be measured after Phase 1) | | | | | | | |

## After POD Path (Phase 5)

| Benchmark | p50 | p75 | p95 | p99 | p999 | max | vs baseline p50 |
|-----------|-----|-----|-----|-----|------|-----|-----------------|
| (to be measured after Phase 5) | | | | | | | |

---

## Environment Checklist

For reproducible results:

- [ ] Release build (`--release`)
- [ ] Turbo boost disabled
- [ ] Pinned to physical cores (no HT siblings)
- [ ] Minimal background load
- [ ] Same kernel, same hardware for before/after comparisons
- [ ] Run multiple times, report median run
