const LEN_OFFSET: usize = 2;

pub struct LogWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
    truncated: bool,
}

impl<'a> LogWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            pos: LEN_OFFSET,
            truncated: false,
        }
    }
}

impl<'a> std::io::Write for LogWriter<'a> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let remaining = self.buf.len().saturating_sub(self.pos);
        let to_write = data.len().min(remaining);

        // Mark if we need to truncate the log.
        if to_write < data.len() {
            self.truncated = true;
        }

        if to_write > 0 {
            self.buf[self.pos..self.pos + to_write].copy_from_slice(&data[..to_write]);
            self.pos += to_write;
        }

        // Always return data.len() to satisfy write_all/write!
        // This prevents "failed to write whole buffer" errors.
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Add ellipses if we are truncating
        if self.truncated {
            let marker_pos = self.buf.len() - 3;
            self.buf[marker_pos..].copy_from_slice(b"...");
            self.pos = self.pos.min(marker_pos); // Adjust pos if needed
        }

        // Write length prefix
        let len = (self.pos - 2).min(self.buf.len() - 2) as u16;
        self.buf[0] = (len & 0xff) as u8;
        self.buf[1] = (len >> 8) as u8;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_basic_write() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        write!(&mut writer, "Hi").unwrap();
        writer.flush().unwrap();

        // First 2 bytes = length (2)
        assert_eq!(buf[0], 2);
        assert_eq!(buf[1], 0);
        // Next 2 bytes = "Hi"
        assert_eq!(&buf[2..4], b"Hi");
    }

    #[test]
    fn test_exact_fit() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        // 6 bytes of data fits exactly (8 - 2 for length)
        write!(&mut writer, "123456").unwrap();
        writer.flush().unwrap();

        assert_eq!(buf[0], 6);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..8], b"123456");
    }

    #[test]
    fn test_truncation() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        // Try to write 10 bytes - should truncate
        write!(&mut writer, "1234567890").unwrap();
        writer.flush().unwrap();

        // Length should be 3 (6 available - 3 for "...")
        assert_eq!(buf[0], 3);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..5], b"123");
        assert_eq!(&buf[5..8], b"...");
    }

    #[test]
    fn test_multiple_writes() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        write!(&mut writer, "AB").unwrap();
        write!(&mut writer, "CD").unwrap();
        write!(&mut writer, "EF").unwrap();
        writer.flush().unwrap();

        assert_eq!(buf[0], 6);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..8], b"ABCDEF");
    }

    #[test]
    fn test_multiple_writes_with_truncation() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        write!(&mut writer, "ABC").unwrap();
        write!(&mut writer, "DEF").unwrap();
        write!(&mut writer, "GHI").unwrap(); // This will cause truncation
        writer.flush().unwrap();

        // Should have "ABC..." (3 chars + ...)
        assert_eq!(buf[0], 3);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..5], b"ABC");
        assert_eq!(&buf[5..8], b"...");
    }

    #[test]
    fn test_empty_write() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        writer.flush().unwrap();

        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 0);
    }

    #[test]
    fn test_format_args() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        let x = 42;
        write!(&mut writer, "x={}", x).unwrap();
        writer.flush().unwrap();

        assert_eq!(buf[0], 4);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..6], b"x=42");
    }

    #[test]
    fn test_format_args_truncated() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        write!(&mut writer, "value={}", 123456789).unwrap();
        writer.flush().unwrap();

        // Should truncate to "val..."
        assert_eq!(buf[0], 3);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..5], b"val");
        assert_eq!(&buf[5..8], b"...");
    }
}
