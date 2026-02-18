//! Prototype test for the hot-path proc macro.
//!
//! Exercises the full encode → logbuf → decode → format path with
//! various type combinations.

use std::io::Write;
use std::sync::{Arc, Mutex};

use inqjet::{ColorMode, InqJetBuilder, LevelFilter};

// -- Capture writer for test output ------------------------------------------

#[derive(Clone)]
struct CaptureWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl Write for CaptureWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// -- Debug-only user POD type (no Display) -----------------------------------

#[derive(Clone, Debug, PartialEq, inqjet::Pod)]
struct OrderInfo {
    id: u64,
    price: f64,
}

// -- Test --------------------------------------------------------------------

#[test]
fn hot_macro_prototype() {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let _guard = InqJetBuilder::default()
        .with_writer(CaptureWriter { buf: buf.clone() })
        .with_log_level(LevelFilter::Trace)
        .with_color_mode(ColorMode::Never)
        .build()
        .unwrap();

    // 1. POD Display: u64 + f64 with precision
    let id: u64 = 42;
    let price: f64 = 99.95;
    inqjet::__hot_log!(
        3u8,
        module_path!(),
        line!(),
        "order {} price {:.2}",
        id,
        price
    );

    // 2. String arg
    let name = String::from("BTC-USD");
    inqjet::__hot_log!(3u8, module_path!(), line!(), "symbol {}", name);

    // 3. &str arg
    let status: &str = "active";
    inqjet::__hot_log!(3u8, module_path!(), line!(), "status {}", status);

    // 4. Mixed POD + String
    let seq: u32 = 7;
    let tag = String::from("fill");
    inqjet::__hot_log!(3u8, module_path!(), line!(), "seq {} tag {}", seq, tag);

    // 5. Static message (no args)
    inqjet::__hot_log!(3u8, module_path!(), line!(), "heartbeat");

    // 6. Debug format (POD)
    let debug_val: i32 = -42;
    inqjet::__hot_log!(4u8, module_path!(), line!(), "debug {:?}", debug_val);

    // 7. Debug-only user POD type (no Display impl)
    let order = OrderInfo {
        id: 12345,
        price: 99.95,
    };
    inqjet::__hot_log!(3u8, module_path!(), line!(), "order {:?}", order);

    // 8. Hex formatting
    let hex_val: u64 = 255;
    inqjet::__hot_log!(4u8, module_path!(), line!(), "hex {:x}", hex_val);

    // 9. Bool
    let flag = true;
    inqjet::__hot_log!(5u8, module_path!(), line!(), "flag {}", flag);

    // Wait for consumer to process all records
    std::thread::sleep(std::time::Duration::from_millis(200));

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();

    // Verify each message was formatted correctly
    assert!(
        output.contains("order 42 price 99.95"),
        "missing 'order 42 price 99.95', got:\n{output}"
    );
    assert!(
        output.contains("symbol BTC-USD"),
        "missing 'symbol BTC-USD', got:\n{output}"
    );
    assert!(
        output.contains("status active"),
        "missing 'status active', got:\n{output}"
    );
    assert!(
        output.contains("seq 7 tag fill"),
        "missing 'seq 7 tag fill', got:\n{output}"
    );
    assert!(
        output.contains("heartbeat"),
        "missing 'heartbeat', got:\n{output}"
    );
    assert!(
        output.contains("debug -42"),
        "missing 'debug -42', got:\n{output}"
    );
    assert!(
        output.contains("OrderInfo"),
        "missing 'OrderInfo' in debug output, got:\n{output}"
    );
    assert!(
        output.contains("hex ff"),
        "missing 'hex ff', got:\n{output}"
    );
    assert!(
        output.contains("flag true"),
        "missing 'flag true', got:\n{output}"
    );
}
