const LEN_OFFSET: usize = 2;

pub struct LogWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> LogWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            pos: LEN_OFFSET,
        }
    }

    pub fn finish(self) -> usize {
        let len = (self.pos - 2).min(self.buf.len() - 2) as u16;
        self.buf[0] = (len & 0xff) as u8;
        self.buf[1] = (len >> 8) as u8;
        self.pos.min(self.buf.len())
    }

    pub fn finish_with_truncation_marker(self) -> usize {
        let len = self.buf.len();
        if self.pos >= len && len >= 5 {
            // Write "..." at the end
            self.buf[len - 3..].copy_from_slice(b"...");
        }
        self.finish()
    }
}

impl<'a> std::io::Write for LogWriter<'a> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let remaining = self.buf.len().saturating_sub(self.pos);
        let to_write = data.len().min(remaining);

        if to_write > 0 {
            self.buf[self.pos..self.pos + to_write].copy_from_slice(&data[..to_write]);
            self.pos += to_write;
        }

        // Always return data.len() to satisfy write_all/write!
        // This prevents "failed to write whole buffer" errors
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
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
        let len = writer.finish();

        // First 2 bytes = length (2)
        assert_eq!(buf[0], 2);
        assert_eq!(buf[1], 0);
        // Next 2 bytes = "Hi"
        assert_eq!(&buf[2..4], b"Hi");
        assert_eq!(len, 4); // 2 + 2
    }

    #[test]
    fn test_exact_fit() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        // 6 bytes of data fits exactly (8 - 2 for length)
        write!(&mut writer, "123456").unwrap();
        let len = writer.finish();

        assert_eq!(buf[0], 6);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..8], b"123456");
        assert_eq!(len, 8);
    }

    #[test]
    fn test_truncation() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        // Try to write 10 bytes - should truncate to 6
        write!(&mut writer, "1234567890").unwrap();
        let len = writer.finish();

        assert_eq!(buf[0], 6);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..8], b"123456");
        assert_eq!(len, 8);
    }

    #[test]
    fn test_truncation_with_marker() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        write!(&mut writer, "1234567890").unwrap();
        let len = writer.finish_with_truncation_marker();

        // Should truncate and add "..."
        assert_eq!(buf[0], 6);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..5], b"123");
        assert_eq!(&buf[5..8], b"...");
        assert_eq!(len, 8);
    }

    #[test]
    fn test_multiple_writes() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        write!(&mut writer, "AB").unwrap();
        write!(&mut writer, "CD").unwrap();
        write!(&mut writer, "EF").unwrap();
        let len = writer.finish();

        assert_eq!(buf[0], 6);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..8], b"ABCDEF");
        assert_eq!(len, 8);
    }

    #[test]
    fn test_empty_write() {
        let mut buf = [0u8; 8];
        let writer = LogWriter::new(&mut buf);

        let len = writer.finish();

        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 0);
        assert_eq!(len, 2);
    }

    #[test]
    fn test_tiny_buffer() {
        let mut buf = [0u8; 4];
        let mut writer = LogWriter::new(&mut buf);

        write!(&mut writer, "Hello").unwrap();
        let len = writer.finish();

        // Can only fit 2 bytes after length prefix
        assert_eq!(buf[0], 2);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..4], b"He");
        assert_eq!(len, 4);
    }

    #[test]
    fn test_format_args() {
        let mut buf = [0u8; 8];
        let mut writer = LogWriter::new(&mut buf);

        let x = 42;
        write!(&mut writer, "x={}", x).unwrap();
        let len = writer.finish();

        assert_eq!(buf[0], 4);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..6], b"x=42");
        assert_eq!(len, 6);
    }
}
