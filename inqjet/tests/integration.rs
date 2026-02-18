//! Integration tests for inqjet standalone macros.
//!
//! Single test function because `LOGGER` is a `OnceLock` — only one
//! `build()` per process actually sets global state. Separate `#[test]`
//! functions would fight over the global and only the first init's
//! writer would receive records.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use inqjet::{ColorMode, InqJetBuilder, LevelFilter};

/// Shared writer that captures output for assertions.
#[derive(Clone)]
struct CaptureWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl CaptureWriter {
    fn new() -> Self {
        Self {
            buf: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn contents(&self) -> String {
        let lock = self.buf.lock().unwrap();
        String::from_utf8(lock.clone()).unwrap()
    }

    fn clear(&self) {
        self.buf.lock().unwrap().clear();
    }
}

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Wait for archiver to process records.
fn drain() {
    std::thread::sleep(std::time::Duration::from_millis(50));
}

#[test]
fn standalone_macros() {
    let writer = CaptureWriter::new();
    let _guard = InqJetBuilder::default()
        .with_writer(writer.clone())
        .with_log_level(LevelFilter::Trace)
        .with_color_mode(ColorMode::Never)
        .build()
        .unwrap();

    // -- Basic output: all five levels produce records -----------------------

    inqjet::error!("error msg");
    inqjet::warn!("warn msg");
    inqjet::info!("info msg");
    inqjet::debug!("debug msg");
    inqjet::trace!("trace msg");
    drain();

    let output = writer.contents();
    assert!(output.contains("[ERROR]"), "missing ERROR: {output}");
    assert!(output.contains("error msg"), "missing error msg: {output}");
    assert!(output.contains("[WARN]"), "missing WARN: {output}");
    assert!(output.contains("warn msg"), "missing warn msg: {output}");
    assert!(output.contains("[INFO]"), "missing INFO: {output}");
    assert!(output.contains("info msg"), "missing info msg: {output}");
    assert!(output.contains("[DEBUG]"), "missing DEBUG: {output}");
    assert!(output.contains("debug msg"), "missing debug msg: {output}");
    assert!(output.contains("[TRACE]"), "missing TRACE: {output}");
    assert!(output.contains("trace msg"), "missing trace msg: {output}");

    // -- Format args: various specifiers -------------------------------------

    writer.clear();

    inqjet::info!("count: {}", 42);
    inqjet::info!("pi: {:.2}", 3.14159);
    inqjet::info!("debug: {:?}", vec![1, 2, 3]);
    inqjet::info!("multi: {} {} {}", "a", "b", "c");
    drain();

    let output = writer.contents();
    assert!(
        output.contains("count: 42"),
        "missing formatted int: {output}"
    );
    assert!(
        output.contains("pi: 3.14"),
        "missing formatted float: {output}"
    );
    assert!(output.contains("[1, 2, 3]"), "missing debug fmt: {output}");
    assert!(
        output.contains("multi: a b c"),
        "missing multi args: {output}"
    );

    // -- Target override -----------------------------------------------------

    writer.clear();

    inqjet::info!(target: "custom::target", "targeted msg");
    drain();

    let output = writer.contents();
    assert!(
        output.contains("custom::target"),
        "missing custom target: {output}"
    );
    assert!(
        output.contains("targeted msg"),
        "missing targeted msg: {output}"
    );

    // -- Static message (no format args) -------------------------------------

    writer.clear();

    inqjet::info!("static message with no args");
    drain();

    let output = writer.contents();
    assert!(
        output.contains("static message with no args"),
        "missing static msg: {output}"
    );

    // -- Level filtering: messages below threshold not produced ---------------

    writer.clear();

    // MAX_LEVEL is Trace (5). Verify the gate function matches:
    // levels 1-5 are enabled, level 6+ is not.
    assert!(inqjet::__log_enabled(1)); // Error: enabled at Trace
    assert!(inqjet::__log_enabled(5)); // Trace: enabled at Trace
    assert!(!inqjet::__log_enabled(6)); // Above Trace: disabled
}
