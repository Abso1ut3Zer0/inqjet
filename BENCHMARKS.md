# InqJet Benchmarks

## Running

```bash
# InqJet scenario benchmarks
cargo test -p inqjet --release bench_inqjet -- --ignored --nocapture

# Tracing comparison
cargo test -p inqjet --release bench_tracing -- --ignored --nocapture
```

For more stable results:

```bash
# Disable turbo boost (Intel / AMD)
echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo
echo 0 | sudo tee /sys/devices/system/cpu/cpufreq/boost

# Pin to physical cores — avoids hyperthread sibling contention
taskset -c 0,2 cargo test -p inqjet --release bench_inqjet -- --ignored --nocapture
```

---

## Results

### InqJet (inqjet::info! hot-path)

100,000 iterations per scenario | 1MB ring buffer | busy-spin consumer

| Scenario | p50 | p90 | p99 | p99.9 | max |
|---|---:|---:|---:|---:|---:|
| static_msg | 0.16us | 0.21us | 0.59us | 4.88us | 26.37us |
| single_int | 0.11us | 0.19us | 0.43us | 0.69us | 113.41us |
| single_str | 0.17us | 0.24us | 0.56us | 1.62us | 112.13us |
| single_string | 0.17us | 0.23us | 0.54us | 1.48us | 150.78us |
| realistic_4arg | 0.18us | 0.29us | 0.54us | 1.98us | 124.16us |
| pod_triple | 0.17us | 0.24us | 0.51us | 1.67us | 110.27us |
| float_precision | 0.08us | 0.18us | 0.36us | 0.79us | 103.74us |
| debug_vec | 0.46us | 0.54us | 1.12us | 5.29us | 1840.13us |
| debug_display_mix | 0.48us | 0.63us | 1.66us | 8.72us | 134.66us |
| verbose_6arg | 0.21us | 0.25us | 0.69us | 2.23us | 142.72us |
| log_bridge | 0.24us | 0.27us | 0.61us | 3.70us | 127.04us |

### Tracing (tracing::info! native)

100,000 iterations per scenario | non_blocking(262K) | file sink

| Scenario | p50 | p90 | p99 | p99.9 | max |
|---|---:|---:|---:|---:|---:|
| static_msg | 0.99us | 1.69us | 2.71us | 16.06us | 148.22us |
| single_int | 1.03us | 1.79us | 2.84us | 18.98us | 1861.63us |
| single_str | 1.07us | 1.92us | 3.10us | 20.78us | 207.74us |
| single_string | 1.03us | 1.72us | 2.71us | 18.83us | 175.36us |
| realistic_4arg | 1.04us | 1.72us | 2.92us | 19.14us | 1859.58us |
| pod_triple | 1.20us | 1.92us | 2.83us | 19.73us | 180.61us |
| float_precision | 1.62us | 2.37us | 3.97us | 20.25us | 536.58us |
| debug_vec | 1.20us | 1.84us | 3.19us | 17.97us | 126.78us |
| debug_display_mix | 1.24us | 1.84us | 3.02us | 14.62us | 77.38us |
| verbose_6arg | 1.31us | 2.04us | 3.56us | 16.70us | 146.81us |

### Head-to-Head (p50)

| Scenario | inqjet | tracing | Speedup |
|---|---:|---:|---:|
| static_msg | 0.16us | 0.99us | **6.2x** |
| single_int | 0.11us | 1.03us | **9.4x** |
| single_str | 0.17us | 1.07us | **6.3x** |
| single_string | 0.17us | 1.03us | **6.1x** |
| realistic_4arg | 0.18us | 1.04us | **5.8x** |
| pod_triple | 0.17us | 1.20us | **7.1x** |
| float_precision | 0.08us | 1.62us | **20.3x** |
| debug_vec | 0.46us | 1.20us | **2.6x** |
| debug_display_mix | 0.48us | 1.24us | **2.6x** |
| verbose_6arg | 0.21us | 1.31us | **6.2x** |

---

## Scenario Descriptions

| Scenario | Format string | Encoding path |
|---|---|---|
| static_msg | `"health check passed"` | No args |
| single_int | `"processed request in {}ms"` + u64 | Tier 1: Pod memcpy (8B) |
| single_str | `"user {} connected"` + &str | Tier 1.5: direct byte copy |
| single_string | `"session {}"` + String | Tier 1.5: direct byte copy |
| realistic_4arg | `"{} {} from {} status {}"` + 3x &str + u32 | Tier 1 + 1.5 mixed |
| pod_triple | `"order id={} price={} qty={}"` + u64 + f64 + i64 | Tier 1: 24 bytes memcpy |
| float_precision | `"latency: {:.4}ms"` + f64 | Tier 1: 8B memcpy, `{:.4}` applied on consumer |
| debug_vec | `"state: {:?}"` + Vec\<u32\> | Tier 2: eager format to TLS stash |
| debug_display_mix | `"request from user {:?} on instance {}: {:?}"` | Tier 1 + 1.5 + 2 (all three) |
| verbose_6arg | `"audit: user={} action={} ..."` + 4x &str + u32 + u64 | Tier 1 + 1.5 mixed |
| log_bridge | `log::info!("processed request in {}ms", i)` | log crate bridge path |

---

## Analysis

### Where the speedup comes from

**Pod and String paths (Tiers 1 & 1.5): 6-9x faster.** The producer memcpy's
raw bytes instead of formatting. A u64 is 8 bytes copied; an f64 with `{:.4}`
is also 8 bytes copied — the precision formatting happens on the consumer thread.
Tracing formats everything eagerly on the producer.

**float_precision: 20x faster.** This is the deferred-formatting advantage in
its purest form. Both loggers produce the same output (`"0.5004"`), but inqjet's
producer pays an 8-byte memcpy while tracing's producer pays `Display::fmt`
with precision.

**Debug fallback (Tier 2): still 2.6x faster.** When inqjet can't defer
formatting (e.g. `{:?}` on a Vec), it eagerly formats to a thread-local
buffer — same work as tracing. The remaining advantage is ring buffer transport
vs channel overhead.

**Scaling with arg count.** verbose_6arg (0.21us) is only ~30% more than
single_int (0.11us). Adding args is cheap when most are Pod/string memcpy's.
Tracing's cost scales more steeply because each additional arg adds formatting.

### Tail latency

InqJet's p99 is typically 2-3x its p50 — tight tail under low contention.
The p99.9 gap widens further: inqjet 1-5us vs tracing 15-20us, likely due
to allocator and channel contention effects in tracing's path.

Max values in the hundreds of microseconds are scheduler preemptions — expected
in any benchmark without full CPU isolation.

---

## Methodology

- **Timing**: `Instant::now()` per iteration (~25ns overhead, consistent)
- **Histogram**: HDR histogram with 3 significant digits
- **Warmup**: 200 iterations before measurement
- **Samples**: 100,000 per scenario
- **Drain**: 25ms sleep every 1,000 iterations to let consumers keep up
- **Isolation**: File sink (no terminal I/O variance), ANSI colors disabled

### InqJet setup

- 1MB ring buffer (`with_buffer_size(1 << 20)`)
- Busy-spin consumer (`with_timeout(None)`)
- `ColorMode::Never`
- File writer with `BufWriter`

### Tracing setup

- `tracing_subscriber::fmt` with `tracing_appender::non_blocking`
- 262K buffered lines limit
- `with_ansi(false)`
- File writer

---

## Environment

Results will vary by hardware. The relative speedups should be consistent
across x86-64 platforms. Record your environment for reproducible comparisons:

```bash
lscpu | grep "Model name"
uname -r
rustc --version
```

### Checklist for stable results

- [ ] Release build (`--release`)
- [ ] Turbo boost disabled
- [ ] Pinned to physical cores (no HT siblings)
- [ ] Minimal background load
- [ ] Same kernel and hardware for before/after comparisons
- [ ] Run multiple times, report median run
