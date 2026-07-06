use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex, RwLock};

struct SharedBufferMeta {
    finished: bool,
    failed: bool,
    total_len: Option<u64>,
}

pub struct SharedBuffer {
    meta: Mutex<SharedBufferMeta>,
    data: RwLock<Vec<u8>>,
    cvar: Condvar,
}

impl SharedBuffer {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            meta: Mutex::new(SharedBufferMeta {
                finished: false,
                failed: false,
                total_len: None,
            }),
            data: RwLock::new(Vec::new()),
            cvar: Condvar::new(),
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.data.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[must_use]
    pub fn writer(self: &Arc<Self>) -> SharedBufferWriter {
        SharedBufferWriter {
            buffer: self.clone(),
            finished: false,
        }
    }

    pub fn set_total_len(&self, len: u64) {
        let mut meta = self.meta.lock().unwrap_or_else(|e| e.into_inner());
        meta.total_len = Some(len);
        self.cvar.notify_all();
    }

    #[must_use]
    pub fn total_len(&self) -> Option<u64> {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).total_len
    }

    #[must_use]
    pub fn data(&self) -> Vec<u8> {
        self.data.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).failed
    }

    pub fn fail(&self) {
        let mut meta = self.meta.lock().unwrap_or_else(|e| e.into_inner());
        meta.failed = true;
        meta.finished = true;
        if meta.total_len.is_none() {
            meta.total_len = Some(self.data.read().unwrap_or_else(|e| e.into_inner()).len() as u64);
        }
        self.cvar.notify_all();
    }

    #[must_use]
    pub fn reader(self: &Arc<Self>) -> SharedBufferReader {
        SharedBufferReader {
            buffer: self.clone(),
            pos: 0,
        }
    }
}

pub struct SharedBufferWriter {
    buffer: Arc<SharedBuffer>,
    finished: bool,
}

impl SharedBufferWriter {
    pub fn write(&mut self, data: &[u8]) {
        self.buffer.data.write().unwrap_or_else(|e| e.into_inner()).extend_from_slice(data);
        self.buffer.cvar.notify_all();
    }

    pub fn finish(&mut self) {
        let mut meta = self.buffer.meta.lock().unwrap_or_else(|e| e.into_inner());
        meta.finished = true;
        if meta.total_len.is_none() {
            meta.total_len = Some(self.buffer.data.read().unwrap_or_else(|e| e.into_inner()).len() as u64);
        }
        self.finished = true;
        self.buffer.cvar.notify_all();
    }

    pub fn fail(&mut self) {
        let mut meta = self.buffer.meta.lock().unwrap_or_else(|e| e.into_inner());
        meta.failed = true;
        meta.finished = true;
        if meta.total_len.is_none() {
            meta.total_len = Some(self.buffer.data.read().unwrap_or_else(|e| e.into_inner()).len() as u64);
        }
        self.finished = true;
        self.buffer.cvar.notify_all();
    }

}

impl Drop for SharedBufferWriter {
    fn drop(&mut self) {
        if !self.finished {
            self.fail();
        }
    }
}

pub struct SharedBufferReader {
    buffer: Arc<SharedBuffer>,
    pos: usize,
}

impl Read for SharedBufferReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut meta = self.buffer.meta.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let data_len = self.buffer.data.read().unwrap_or_else(|e| e.into_inner()).len();
            if self.pos < data_len || meta.finished || meta.failed {
                break;
            }
            meta = self.buffer.cvar.wait(meta).unwrap_or_else(|e| e.into_inner());
        }
        // Clamp position in case the buffer finished while we were waiting
        // and total_len was slightly overestimated.
        let data_len = self.buffer.data.read().unwrap_or_else(|e| e.into_inner()).len();
        if meta.finished && self.pos >= data_len {
            self.pos = data_len;
        }
        if self.pos >= data_len || meta.failed {
            return Ok(0);
        }
        let data = self.buffer.data.read().unwrap_or_else(|e| e.into_inner());
        let available = data.len() - self.pos;
        let to_read = buf.len().min(available);
        buf[..to_read].copy_from_slice(&data[self.pos..self.pos + to_read]);
        self.pos += to_read;
        Ok(to_read)
    }
}

impl Seek for SharedBufferReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let mut meta = self.buffer.meta.lock().unwrap_or_else(|e| e.into_inner());
        let new_pos = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(delta) => self.pos as i64 + delta,
            SeekFrom::End(offset) => {
                if meta.finished {
                    self.buffer.data.read().unwrap_or_else(|e| e.into_inner()).len() as i64 + offset
                } else if let Some(total_len) = meta.total_len {
                    total_len as i64 + offset
                } else {
                    while !meta.finished && !meta.failed {
                        meta = self.buffer.cvar.wait(meta).unwrap_or_else(|e| e.into_inner());
                    }
                    self.buffer.data.read().unwrap_or_else(|e| e.into_inner()).len() as i64 + offset
                }
            }
        };
        let data_len = self.buffer.data.read().unwrap_or_else(|e| e.into_inner()).len() as u64;
        let upper = meta
            .total_len
            .filter(|_| !meta.finished)
            .unwrap_or(data_len) as usize;
        self.pos = (new_pos.max(0) as usize).min(upper);
        Ok(self.pos as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn seek_from_end_returns_total_len_not_clamped_to_available() {
        let buf = SharedBuffer::new();
        let total: u64 = 4_000_000;
        buf.set_total_len(total);

        let mut w = buf.writer();
        w.write(b"\x00\x01\x02\x03\x04\x05\x06\x07");
        // Don't drop w — keep writer alive to simulate ongoing download.

        let mut r = buf.reader();
        let pos = r.seek(SeekFrom::End(0)).unwrap();
        // SeekFrom::End(0) MUST return the total_len estimate, NOT clamped
        // to the 8 bytes currently available.  Symphonia's isomp4 reader
        // uses this value to calculate moov-atom offsets and will panic
        // with SeekError → unreachable! if it gets a wrong (clamped) value.
        assert_eq!(pos, total, "SeekFrom::End(0) must return total_len");
        // Position must be at total_len, not clamped to data.len().
        assert_eq!(r.pos, total as usize, "reader position must be total_len");
    }

    #[test]
    fn seek_from_end_negative_offset_uses_total_len() {
        let buf = SharedBuffer::new();
        let total: u64 = 1_000_000;
        buf.set_total_len(total);

        let mut r = buf.reader();
        // Seek to 100 bytes before end.
        let pos = r.seek(SeekFrom::End(-100)).unwrap();
        assert_eq!(pos, total - 100);
        assert_eq!(r.pos, (total - 100) as usize);
    }

    #[test]
    fn read_blocks_until_data_arrives_at_seeked_position() {
        let buf = SharedBuffer::new();
        let total: u64 = 1_000_000;
        buf.set_total_len(total);

        let mut initial_writer = buf.writer();
        initial_writer.write(b"AAAA");
        // Keep initial_writer alive so the buffer stays in "downloading" state.

        let mut r = buf.reader();
        // Seek near the end — data not yet available.
        let pos = r.seek(SeekFrom::End(-4)).unwrap();
        assert_eq!(pos, 999_996);

        // Spawn a thread that writes the final 4 bytes after a delay.
        let buf2 = buf.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let mut w = buf2.writer();
            // Fill up to position 999_996..1_000_000
            let needed = (total - 4) as usize;
            // Extend the buffer — just write padding to reach the needed size
            // then write the target bytes.
            {
                let current = w.buffer.data.read().unwrap_or_else(|e| e.into_inner()).len();
                let pad = vec![0u8; needed - current];
                w.write(&pad);
            }
            w.write(b"BBBB");
            w.finish();
        });

        // Now read — should block until the writer provides data.
        let mut out = [0u8; 4];
        let n = r.read(&mut out).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&out, b"BBBB");
    }

    #[test]
    fn seek_from_end_blocks_when_total_len_unknown_and_not_finished() {
        let buf = SharedBuffer::new();
        // No total_len set, not finished.

        let mut r = buf.reader();
        // Write some data in a thread, then finish.
        let buf2 = buf.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let mut w = buf2.writer();
            w.write(b"hello world");
            w.finish();
        });

        let pos = r.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(pos, 11);
        assert_eq!(r.pos, 11);
    }

    #[test]
    fn read_returns_zero_after_fail() {
        let buf = SharedBuffer::new();
        {
            let mut w = buf.writer();
            w.write(b"hello");
            w.fail();
        }
        let mut r = buf.reader();
        let mut out = [0u8; 10];
        let n = r.read(&mut out).unwrap();
        assert_eq!(n, 0, "Read after fail should return 0");
    }

    #[test]
    fn write_after_finish_does_not_panic() {
        let buf = SharedBuffer::new();
        let mut w = buf.writer();
        w.write(b"hello");
        w.finish();
        // Writing after finish should not panic. The buffer allows writes
        // even after finish (no guard), but readers won't block since
        // finished=true.
        w.write(b"world"); // no panic
    }

    #[test]
    fn seek_from_start_and_current_position() {
        let buf = SharedBuffer::new();
        buf.set_total_len(1000);
        {
            let mut w = buf.writer();
            w.write(&[0u8; 500]);
        }

        let mut r = buf.reader();

        // SeekFrom::Start
        let pos = r.seek(SeekFrom::Start(250)).unwrap();
        assert_eq!(pos, 250);
        assert_eq!(r.pos, 250);

        // SeekFrom::Current forward
        let pos = r.seek(SeekFrom::Current(100)).unwrap();
        assert_eq!(pos, 350);

        // SeekFrom::Current backward
        let pos = r.seek(SeekFrom::Current(-50)).unwrap();
        assert_eq!(pos, 300);
    }

}
