#![allow(dead_code)]

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

/// Shared writer that captures output for assertions.
#[derive(Clone)]
pub struct CaptureWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl CaptureWriter {
    pub fn new() -> Self {
        Self {
            buf: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn contents(&self) -> String {
        let lock = self.buf.lock().unwrap();
        String::from_utf8(lock.clone()).unwrap()
    }

    pub fn clear(&self) {
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
pub fn drain() {
    std::thread::sleep(std::time::Duration::from_millis(50));
}
