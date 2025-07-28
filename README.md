# InqJet ⚡

**Ultra-fast, low-latency logging for Rust applications**

InqJet is a high-performance logging library designed for latency-critical applications where every microsecond counts. It achieves consistent single-digit μs producer-side overhead by decoupling log formatting from I/O operations using a lock-free architecture.

## ✨ Features

- 🚀 **Ultra-low latency**: Single-digit μs producer-side overhead (cold start optimized)
- 🔒 **Lock-free architecture**: Zero contention between logging threads
- 💾 **Minimal allocations**: String pooling eliminates malloc/free overhead
- 🎨 **Colored output**: ANSI color-coded log levels for better readability
- ⏱️ **Structured format**: ISO 8601 timestamps with microsecond precision
- 🎯 **Efficient filtering**: Level-based filtering with minimal overhead
- 🛡️ **Clean shutdown**: Guaranteed message delivery on application exit
- 🔧 **Configurable**: Tunable for different latency/throughput profiles

## 🚀 Quick Start

Initialize the logger:

```rust
use inqjet::InqJetBuilder;
use log::{info, error, LevelFilter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logger with stdout output
    let _guard = InqJetBuilder::default()
        .with_writer(std::io::stdout())
        .with_log_level(LevelFilter::Info)
        .build()?;

    // Use standard logging macros
    info!("Server started on port {}", 8080);
    error!("Connection failed: timeout");

    // Logger automatically shuts down when guard is dropped
    Ok(())
}
```

## 📊 Performance

InqJet is specifically optimized for **producer-side latency** - the time it takes for a log statement to be passed to the background appender thread.

## 🏗️ Architecture

```
[Producer Threads] → [Logger] → [Lock-free Channel] → [Background Thread] → [Output]
       ↓               ↓              ↓                     ↓               ↓
   log!() calls    Format msg     Queue message         Write to I/O      File/Stdout
   (1-5μs)         Pool strings    (lock-free)          (background)
```

### Key Design Decisions

1. **String Pool**: Reuses pre-allocated 256-byte strings, automatically growing as needed
2. **Lock-free Channel**: `crossbeam::ArrayQueue` for zero-contention message passing
3. **Background I/O**: Dedicated consumer thread handles all expensive I/O operations
4. **Early Filtering**: Level checks happen before any formatting overhead
5. **Guaranteed Delivery**: Backpressure blocks producers rather than dropping messages

## 📋 Log Format

InqJet produces structured, colored log output:

```
2024-01-15T14:30:45.123456Z [INFO] my_app::auth:127 - User alice logged in
2024-01-15T14:30:45.124001Z [ERROR] my_app::db - Connection failed: timeout
├─ Timestamp (gray)         ├─ Level (colored)  ├─ Target:line (gray)  ├─ Message
└─ ISO 8601 with μs         └─ Color coded      └─ Optional line number └─ User content
```

### Color Scheme

- 🔴 **ERROR**: Red
- 🟡 **WARN**: Yellow
- 🟢 **INFO**: Green
- 🔵 **DEBUG**: Cyan
- ⚪ **TRACE**: Gray

## ⚙️ Configuration

### Basic Configuration

```rust
let _guard = InqJetBuilder::default()
    .with_writer(std::io::stdout())
    .with_log_level(LevelFilter::Info)
    .build()?;
```

### File Logging

```rust
use std::fs::OpenOptions;

let file = OpenOptions::new()
    .create(true)
    .append(true)
    .open("app.log")?;

let _guard = InqJetBuilder::default()
    .with_writer(file)
    .with_log_level(LevelFilter::Debug)
    .build()?;
```

### Performance Tuning

#### Channel Capacity

Size the channel based on your burst patterns:

```rust
let _guard = InqJetBuilder::default()
    .with_capacity(4096)  // Large buffer for traffic bursts
    .build()?;
```

- **64-256**: Consistent applications, quick backpressure
- **512-1024**: Balanced throughput and memory usage (recommended)
- **2048+**: High-burst applications, absorb traffic spikes

#### Ultra-Low Latency Mode

For absolute minimum latency (lowest possible tails), use spinning appender (dedicates a CPU core):

```rust
let _guard = InqJetBuilder::default()
    .with_timeout(None)  // Spin forever, never park
    .build()?;
```

**Performance impact:**
- ✅ **Producer**: Never calls `unpark()` - no kernel overhead for wakeups
- ✅ **Consumer**: Sub-microsecond message processing
- ❌ **CPU**: Consumes 100% of one CPU core continuously

#### Balanced Mode (Default)

For responsive shutdown with minimal overhead:

```rust
use std::time::Duration;

let _guard = InqJetBuilder::default()
    .with_timeout(Some(Duration::from_millis(5)))  // Default
    .build()?;
```

## 🔧 Advanced Usage

### Custom Writers

InqJet works with any `Write + Send + 'static` type:

```rust
use std::net::TcpStream;

// Network logging
let stream = TcpStream::connect("log-server:514")?;
let _guard = InqJetBuilder::default()
    .with_writer(stream)
    .build()?;

// Multiple outputs (using a custom writer)
struct MultiWriter {
    stdout: std::io::Stdout,
    file: std::fs::File,
}

impl std::io::Write for MultiWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stdout.write_all(buf)?;
        self.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stdout.flush()?;
        self.file.flush()
    }
}
```

### Integration with Existing Code

InqJet is fully compatible with the standard `log` crate:

```rust
// Works with any crate using the log facade
use log::{info, debug, error};
use serde_json::json;

// Structured logging
let user_data = json!({
    "user_id": 12345,
    "action": "login",
    "ip": "192.168.1.100"
});

info!("User event: {}", user_data);

// Error logging with context
if let Err(e) = risky_operation() {
    error!("Operation failed: {} (retrying in 5s)", e);
}
```

## 🏆 When to Use InqJet

### ✅ Perfect For

- **Latency-critical applications**: Trading systems, real-time games, embedded systems
- **High-frequency logging**: Applications logging thousands of messages per second
- **Performance monitoring**: Where logging overhead itself affects metrics
- **Hot path logging**: When logs are in critical performance paths

### ❌ Consider Alternatives

- **Simple applications**: If logging latency isn't critical, `env_logger` is simpler
- **Structured logging**: If you need complex structured data, consider `tracing`
- **Memory-constrained environments**: InqJet uses more memory for performance
- **Single-threaded applications**: The background thread overhead may not be worth it

### Feature Flags

Currently InqJet has no optional features - it's designed to be minimal and fast by default.

## 🙋 FAQ

### Q: Why not use `tracing`?

**A:** `tracing` is excellent for structured logging and observability, but has higher latency overhead (10-50μs cold start vs 1-5μs for InqJet). If you need structured data and spans, use `tracing`. If you need raw speed, use InqJet.

### Q: Can I use InqJet with async runtimes?

**A:** Yes! InqJet works perfectly with tokio, async-std, and other runtimes. The background thread is independent of your async runtime.

### Q: Does InqJet support log rotation?

**A:** Not directly. Use external tools like `logrotate` or provide a custom writer that handles rotation.

### Q: What happens if the consumer can't keep up?

**A:** Producers will block with exponential backoff until the consumer catches up. No messages are dropped - InqJet guarantees delivery.

### Q: Can I have multiple InqJet instances?

**A:** No, InqJet sets itself as the global logger. Only one logger can be active per process.
