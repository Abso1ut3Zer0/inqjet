//! Verifies `BackpressureMode::Drop` behavior.
//!
//! With a tiny ring buffer and a tight producer loop, some messages
//! must be dropped. The test verifies the mode doesn't hang and
//! produces partial output.

mod common;

use inqjet::{BackpressureMode, ColorMode, InqJetBuilder, LevelFilter};

#[test]
fn backpressure_drop() {
    let writer = common::CaptureWriter::new();
    let _guard = InqJetBuilder::default()
        .with_writer(writer.clone())
        .with_log_level(LevelFilter::Info)
        .with_color_mode(ColorMode::Never)
        .with_buffer_size(512)
        .with_backpressure(BackpressureMode::Drop)
        .with_timeout(None) // busy-spin consumer for fast drain
        .build()
        .unwrap();

    // Tight loop: overwhelm the tiny buffer
    for i in 0..10_000u32 {
        inqjet::info!("bp {}", i);
    }

    common::drain();

    let output = writer.contents();
    let count = output.lines().count();

    assert!(count > 0, "consumer should have processed some messages");
    assert!(
        count < 10_000,
        "with 512-byte buffer and Drop mode, some messages should be dropped (got {count})"
    );
}
