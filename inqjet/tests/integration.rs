//! Integration tests for inqjet standalone macros.
//!
//! Single test function because `LOGGER` is a `OnceLock` — only one
//! `build()` per process actually sets global state. Separate `#[test]`
//! functions would fight over the global and only the first init's
//! writer would receive records.

mod common;

use inqjet::{ColorMode, InqJetBuilder, LevelFilter};

// -- User POD type for derive(Pod) coverage ----------------------------------

#[derive(Clone, Debug, PartialEq, inqjet::Pod)]
struct OrderInfo {
    id: u64,
    price: f64,
}

#[test]
fn standalone_macros() {
    let writer = common::CaptureWriter::new();
    let guard = InqJetBuilder::default()
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
    common::drain();

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
    common::drain();

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
    common::drain();

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
    common::drain();

    let output = writer.contents();
    assert!(
        output.contains("static message with no args"),
        "missing static msg: {output}"
    );

    // -- User POD type with derive(Pod) + Debug format -----------------------

    writer.clear();

    let order = OrderInfo {
        id: 12345,
        price: 99.95,
    };
    inqjet::info!("order {:?}", order);
    inqjet::info!("hex {:x}", 255u64);
    common::drain();

    let output = writer.contents();
    assert!(
        output.contains("OrderInfo"),
        "missing OrderInfo in debug output: {output}"
    );
    assert!(
        output.contains("12345"),
        "missing order id in debug output: {output}"
    );
    assert!(output.contains("hex ff"), "missing hex format: {output}");

    // -- Level gate function -------------------------------------------------

    writer.clear();

    // MAX_LEVEL is Trace (5). Verify the gate function matches:
    // levels 1-5 are enabled, level 6+ is not.
    assert!(inqjet::__private::log_enabled(1)); // Error: enabled at Trace
    assert!(inqjet::__private::log_enabled(5)); // Trace: enabled at Trace
    assert!(!inqjet::__private::log_enabled(6)); // Above Trace: disabled

    // -- Level filtering: filtered messages absent ---------------------------

    inqjet::set_level(LevelFilter::Warn);
    inqjet::info!("should be filtered");
    common::drain();

    let output = writer.contents();
    assert!(
        !output.contains("should be filtered"),
        "info msg should be filtered at Warn level: {output}"
    );

    inqjet::set_level(LevelFilter::Trace); // restore

    // -- set_level() runtime change ------------------------------------------

    writer.clear();

    // At Trace: info should appear
    inqjet::info!("runtime info visible");
    common::drain();
    assert!(
        writer.contents().contains("runtime info visible"),
        "info should be visible at Trace: {}",
        writer.contents()
    );

    // Switch to Error: warn should be absent
    writer.clear();
    inqjet::set_level(LevelFilter::Error);
    inqjet::warn!("runtime warn filtered");
    common::drain();
    assert!(
        !writer.contents().contains("runtime warn filtered"),
        "warn should be filtered at Error: {}",
        writer.contents()
    );

    // Switch to Debug: debug should appear
    writer.clear();
    inqjet::set_level(LevelFilter::Debug);
    inqjet::debug!("runtime debug visible");
    common::drain();
    assert!(
        writer.contents().contains("runtime debug visible"),
        "debug should be visible at Debug: {}",
        writer.contents()
    );

    inqjet::set_level(LevelFilter::Trace); // restore

    // -- Multi-threaded producers --------------------------------------------

    writer.clear();

    let threads: Vec<_> = (0..4u32)
        .map(|tid| {
            std::thread::spawn(move || {
                for i in 0..50u32 {
                    inqjet::info!("thread-{} msg-{}", tid, i);
                }
            })
        })
        .collect();

    for t in threads {
        t.join().unwrap();
    }
    common::drain();

    let output = writer.contents();
    for tid in 0..4u32 {
        for i in 0..50u32 {
            let expected = format!("thread-{tid} msg-{i}");
            assert!(
                output.contains(&expected),
                "missing '{expected}' in multi-thread output"
            );
        }
    }

    // -- Non-ASCII UTF-8 ----------------------------------------------------

    writer.clear();

    inqjet::info!("cjk: 你好世界");
    inqjet::info!("accented: café résumé naïve");
    inqjet::info!("emoji: 🦀🔥");
    common::drain();

    let output = writer.contents();
    assert!(output.contains("cjk: 你好世界"), "missing CJK: {output}");
    assert!(
        output.contains("accented: café résumé naïve"),
        "missing accented: {output}"
    );
    assert!(output.contains("emoji: 🦀🔥"), "missing emoji: {output}");

    // -- Guard drop drains remaining records (MUST BE LAST) -----------------

    writer.clear();

    inqjet::info!("drain-on-drop 1");
    inqjet::info!("drain-on-drop 2");
    inqjet::info!("drain-on-drop 3");
    // Do NOT drain — rely on guard drop to flush
    drop(guard);

    let output = writer.contents();
    assert!(
        output.contains("drain-on-drop 1"),
        "guard drop should drain record 1: {output}"
    );
    assert!(
        output.contains("drain-on-drop 2"),
        "guard drop should drain record 2: {output}"
    );
    assert!(
        output.contains("drain-on-drop 3"),
        "guard drop should drain record 3: {output}"
    );
}
