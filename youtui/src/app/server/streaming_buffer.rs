use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};

struct SharedBufferInner {
    data: Vec<u8>,
    finished: bool,
    failed: bool,
    total_len: Option<u64>,
}

pub struct SharedBuffer {
    inner: Mutex<SharedBufferInner>,
    cvar: Condvar,
}

impl SharedBuffer {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(SharedBufferInner {
                data: Vec::new(),
                finished: false,
                failed: false,
                total_len: None,
            }),
            cvar: Condvar::new(),
        })
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().data.len()
    }

    pub fn writer(self: &Arc<Self>) -> SharedBufferWriter {
        SharedBufferWriter {
            buffer: self.clone(),
            finished: false,
        }
    }

    pub fn set_total_len(&self, len: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.total_len = Some(len);
        self.cvar.notify_all();
    }

    pub fn total_len(&self) -> Option<u64> {
        self.inner.lock().unwrap().total_len
    }

    /// Block until `total_len` is set or `timeout` elapses.
    /// Returns `None` if the timeout fires before total_len is set,
    /// or if the buffer transitions to finished/failed before total_len.
    /// WARNING: this blocks the calling thread (std::sync::Condvar).
    /// Do NOT call from async contexts — use the async polling loop
    /// in `download_and_decode` instead.
    #[allow(dead_code)]
    pub fn wait_for_total_len(&self, timeout: std::time::Duration) -> Option<u64> {
        let start = std::time::Instant::now();
        let mut inner = self.inner.lock().unwrap();
        while inner.total_len.is_none() && !inner.finished && !inner.failed {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return None;
            }
            let (guard, _) = self
                .cvar
                .wait_timeout(inner, timeout - elapsed)
                .unwrap();
            inner = guard;
        }
        inner.total_len
    }

    pub fn data(&self) -> Vec<u8> {
        self.inner.lock().unwrap().data.clone()
    }

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
        let mut inner = self.buffer.inner.lock().unwrap();
        inner.data.extend_from_slice(data);
        self.buffer.cvar.notify_all();
    }

    pub fn finish(&mut self) {
        let mut inner = self.buffer.inner.lock().unwrap();
        inner.finished = true;
        if inner.total_len.is_none() {
            inner.total_len = Some(inner.data.len() as u64);
        }
        self.finished = true;
        self.buffer.cvar.notify_all();
    }

    pub fn fail(&mut self) {
        let mut inner = self.buffer.inner.lock().unwrap();
        inner.failed = true;
        inner.finished = true;
        if inner.total_len.is_none() {
            inner.total_len = Some(inner.data.len() as u64);
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
        let mut inner = self.buffer.inner.lock().unwrap();
        // If buffer is finished, clamp position to the actual data length
        // to handle SeekFrom::End positions computed from an overestimated
        // total_len (yt-dlp progress line may slightly overestimate).
        if inner.finished && self.pos >= inner.data.len() {
            self.pos = inner.data.len();
        }
        while self.pos >= inner.data.len() && !inner.finished && !inner.failed {
            inner = self.buffer.cvar.wait(inner).unwrap();
        }
        if self.pos >= inner.data.len() || inner.failed {
            return Ok(0);
        }
        let available = inner.data.len() - self.pos;
        let to_read = buf.len().min(available);
        buf[..to_read].copy_from_slice(&inner.data[self.pos..self.pos + to_read]);
        self.pos += to_read;
        Ok(to_read)
    }
}

impl Seek for SharedBufferReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let mut inner = self.buffer.inner.lock().unwrap();
        let new_pos = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(delta) => self.pos as i64 + delta,
            SeekFrom::End(offset) => {
                // Use total_len estimate (set from yt-dlp progress) for
                // immediate non-blocking return when download is in progress.
                // When finished, use the actual data length (total_len may
                // slightly overestimate, and SeekFrom::End positions computed
                // from an overestimate would exceed available data — the
                // subsequent read() would return EOF, causing symphonia's
                // isomp4 reader to fail with "end of stream").
                if inner.finished {
                    inner.data.len() as i64 + offset
                } else if inner.total_len.is_some() {
                    inner.total_len.unwrap() as i64 + offset
                } else {
                    while !inner.finished && !inner.failed {
                        inner = self.buffer.cvar.wait(inner).unwrap();
                    }
                    inner.data.len() as i64 + offset
                }
            }
        };
        // Clamp to total_len when known and download is in progress so
        // SeekFrom::End(0) returns the actual file size (used by
        // Symphonia's isomp4 reader).  Clamp to available data when
        // total_len is unknown (legacy path).  Either way, subsequent
        // read() blocks on Condvar if data isn't available yet, so the
        // streaming behaviour works naturally.
        let upper = inner
            .total_len
            .filter(|_| !inner.finished)
            .unwrap_or(inner.data.len() as u64) as usize;
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
                let inner = w.buffer.inner.lock().unwrap();
                let current = inner.data.len();
                drop(inner);
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

    #[test]
    fn wait_for_total_len_returns_when_set_from_another_thread() {
        let buf = SharedBuffer::new();

        let buf2 = buf.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            buf2.set_total_len(42);
        });

        let got = buf.wait_for_total_len(Duration::from_secs(5));
        assert_eq!(got, Some(42));
    }

    #[test]
    fn wait_for_total_len_timeout_returns_none() {
        let buf = SharedBuffer::new();
        // No one sets total_len — should time out.
        let got = buf.wait_for_total_len(Duration::from_millis(10));
        assert_eq!(got, None);
    }

    #[test]
    fn wait_for_total_len_returns_some_when_already_set() {
        let buf = SharedBuffer::new();
        buf.set_total_len(100);
        let got = buf.wait_for_total_len(Duration::from_secs(5));
        assert_eq!(got, Some(100));
    }
}
