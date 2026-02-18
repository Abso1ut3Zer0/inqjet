//! End-to-end test for the `log` crate bridge path.
//!
//! Verifies that `log::info!()` etc. flow through InqJet's `Logger`
//! implementation and produce correctly formatted output.

#![cfg(feature = "log-compat")]

mod common;

use inqjet::{ColorMode, InqJetBuilder, LevelFilter};

#[test]
fn log_bridge() {
    let writer = common::CaptureWriter::new();
    let _guard = InqJetBuilder::default()
        .with_writer(writer.clone())
        .with_log_level(LevelFilter::Trace)
        .with_color_mode(ColorMode::Never)
        .build()
        .unwrap();

    log::error!("bridge error {}", 1);
    log::warn!("bridge warn");
    log::info!("bridge info {}", "val");
    log::debug!("bridge debug {:?}", vec![1, 2]);
    log::trace!("bridge trace");
    common::drain();

    let output = writer.contents();

    // All five levels present
    assert!(output.contains("[ERROR]"), "missing ERROR: {output}");
    assert!(
        output.contains("bridge error 1"),
        "missing error msg: {output}"
    );
    assert!(output.contains("[WARN]"), "missing WARN: {output}");
    assert!(output.contains("bridge warn"), "missing warn msg: {output}");
    assert!(output.contains("[INFO]"), "missing INFO: {output}");
    assert!(
        output.contains("bridge info val"),
        "missing info msg: {output}"
    );
    assert!(output.contains("[DEBUG]"), "missing DEBUG: {output}");
    assert!(
        output.contains("bridge debug [1, 2]"),
        "missing debug msg: {output}"
    );
    assert!(output.contains("[TRACE]"), "missing TRACE: {output}");
    assert!(
        output.contains("bridge trace"),
        "missing trace msg: {output}"
    );

    // Each output line has a timestamp and module target
    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Timestamp format: YYYY-MM-DDThh:mm:ss.nnnnnnnnn — at least 30 chars
        assert!(
            line.len() > 30,
            "line too short (missing timestamp?): {line}"
        );
        // Module target present (this test's module path)
        assert!(
            line.contains("log_bridge"),
            "missing module target in line: {line}"
        );
    }
}
