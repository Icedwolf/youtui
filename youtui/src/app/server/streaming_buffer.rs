use crate::core::PoisonRecovery;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};

struct SharedBufferInner {
    finished: bool,
    failed: bool,
    total_len: Option<u64>,
    state: BufferState,
}

enum BufferState {
    Partial(Vec<u8>),
    Complete(Arc<[u8]>),
}

/// Single-Mutex design avoids nested lock ordering issues between the old
/// separate `meta: Mutex` + `state: RwLock` — all state transitions are
/// atomic within one lock acquisition.
pub struct SharedBuffer {
    inner: Mutex<SharedBufferInner>,
    cvar: Condvar,
}

fn available_len(state: &BufferState) -> usize {
    match state {
        BufferState::Partial(v) => v.len(),
        BufferState::Complete(a) => a.len(),
    }
}

impl SharedBuffer {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with_capacity(0)
    }

    #[must_use]
    pub fn with_capacity(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(SharedBufferInner {
                finished: false,
                failed: false,
                total_len: None,
                state: BufferState::Partial(Vec::with_capacity(cap)),
            }),
            cvar: Condvar::new(),
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let guard = self.inner.lock().unwrap_or_warn();
        available_len(&guard.state)
    }

    #[must_use]
    pub fn writer(self: &Arc<Self>) -> SharedBufferWriter {
        SharedBufferWriter {
            buffer: self.clone(),
            finished: false,
        }
    }

    pub fn set_total_len(&self, len: u64) {
        let mut guard = self.inner.lock().unwrap_or_warn();
        guard.total_len = Some(len);
        if let BufferState::Partial(ref mut v) = guard.state {
            v.reserve(len as usize);
        }
        self.cvar.notify_all();
    }

    #[must_use]
    pub fn total_len(&self) -> Option<u64> {
        self.inner.lock().unwrap_or_warn().total_len
    }

    #[must_use]
    pub fn finalize(&self) -> Arc<[u8]> {
        let mut guard = self.inner.lock().unwrap_or_warn();
        guard.finished = true;
        let arc = match &mut guard.state {
            BufferState::Complete(arc) => Arc::clone(arc),
            BufferState::Partial(vec) => {
                let data = std::mem::take(vec);
                let arc: Arc<[u8]> = Arc::from(data);
                guard.state = BufferState::Complete(Arc::clone(&arc));
                arc
            }
        };
        self.cvar.notify_all();
        arc
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.inner.lock().unwrap_or_warn().failed
    }

    pub fn fail(&self) {
        let mut guard = self.inner.lock().unwrap_or_warn();
        guard.failed = true;
        guard.finished = true;
        if let BufferState::Partial(ref v) = guard.state
            && guard.total_len.is_none()
        {
            guard.total_len = Some(v.len() as u64);
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
        let mut guard = self.buffer.inner.lock().unwrap_or_warn();
        if let BufferState::Partial(ref mut v) = guard.state {
            v.extend_from_slice(data);
        }
        self.buffer.cvar.notify_all();
    }

    pub fn finish(&mut self) {
        let mut guard = self.buffer.inner.lock().unwrap_or_warn();
        guard.finished = true;
        if let BufferState::Partial(ref v) = guard.state
            && guard.total_len.is_none()
        {
            guard.total_len = Some(v.len() as u64);
        }
        if let BufferState::Partial(ref mut v) = guard.state {
            v.shrink_to_fit();
        }
        self.finished = true;
        self.buffer.cvar.notify_all();
    }

    pub fn fail(&mut self) {
        let mut guard = self.buffer.inner.lock().unwrap_or_warn();
        guard.failed = true;
        guard.finished = true;
        if let BufferState::Partial(ref v) = guard.state
            && guard.total_len.is_none()
        {
            guard.total_len = Some(v.len() as u64);
        }
        if let BufferState::Partial(ref mut v) = guard.state {
            v.shrink_to_fit();
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
        let mut guard = self.buffer.inner.lock().unwrap_or_warn();
        loop {
            let avail = available_len(&guard.state);
            if self.pos < avail || guard.finished || guard.failed {
                break;
            }
            guard = self.buffer.cvar.wait(guard).unwrap_or_warn();
        }
        let avail = available_len(&guard.state);
        if guard.finished && self.pos >= avail {
            self.pos = avail;
        }
        if self.pos >= avail {
            return Ok(0);
        }

        match guard.state {
            BufferState::Complete(ref arc) => {
                let to_read = buf.len().min(arc.len() - self.pos);
                buf[..to_read].copy_from_slice(&arc[self.pos..self.pos + to_read]);
                self.pos += to_read;
                Ok(to_read)
            }
            BufferState::Partial(ref v) => {
                let to_read = buf.len().min(v.len() - self.pos);
                buf[..to_read].copy_from_slice(&v[self.pos..self.pos + to_read]);
                self.pos += to_read;
                Ok(to_read)
            }
        }
    }
}

impl Seek for SharedBufferReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let mut guard = self.buffer.inner.lock().unwrap_or_warn();
        let new_pos = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(delta) => self.pos as i64 + delta,
            SeekFrom::End(offset) => {
                if guard.finished {
                    available_len(&guard.state) as i64 + offset
                } else if let Some(total_len) = guard.total_len {
                    total_len as i64 + offset
                } else {
                    while !guard.finished && !guard.failed {
                        guard = self.buffer.cvar.wait(guard).unwrap_or_warn();
                    }
                    available_len(&guard.state) as i64 + offset
                }
            }
        };
        let data_len = available_len(&guard.state) as u64;
        let upper = guard
            .total_len
            .filter(|_| !guard.finished)
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
                let current = w.buffer.len();
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
        assert_eq!(n, 5, "Read after fail should drain remaining data first");
        let n2 = r.read(&mut out).unwrap();
        assert_eq!(n2, 0, "Second read after drain should return 0");
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
    fn writer_continues_after_handle_dropped() {
        // Regression: the prefetch mechanism must NOT kill the previous
        // song's data pipeline.  Dropping a JoinHandle / thread handle
        // does NOT cancel the underlying work — the writer completes
        // independently and fills the SharedBuffer.
        let buf = Arc::new(SharedBuffer::new());
        let total: u64 = 1_000_000;
        buf.set_total_len(total);

        // Spawn a writer that fills the buffer from a background thread.
        let bg_buf = buf.clone();
        let handle = thread::spawn(move || {
            let mut w = bg_buf.writer();
            let data = vec![0xCDu8; total as usize];
            // Write in chunks to simulate streaming.
            for chunk in data.chunks(100_000) {
                w.write(chunk);
                thread::sleep(Duration::from_millis(1));
            }
            w.finish();
        });

        // Drop the handle immediately — simulating what happens when
        // store_bg_handle overwrites the previous handle without
        // cancelling the task.  The writer MUST continue.
        drop(handle);

        // Reader must be able to read ALL data even after the handle
        // was dropped (writer is still running independently).
        let mut r = buf.reader();
        let mut total_read = 0usize;
        let mut out = vec![0u8; 4096];
        loop {
            let n = r.read(&mut out).unwrap();
            if n == 0 {
                break;
            }
            total_read += n;
        }
        assert_eq!(total_read, total as usize,
            "Reader must read all data after writer handle is dropped");
    }

    #[test]
    fn available_len_sees_complete_after_finalize() {
        let buf = SharedBuffer::new();
        let mut w = buf.writer();
        w.write(b"hello world");
        w.finish();
        let arc = buf.finalize();
        assert_eq!(arc.len(), 11);
        assert_eq!(buf.len(), 11, "available_len must reflect finalize");
        // Reader should also see it.
        let mut r = buf.reader();
        let mut out = [0u8; 11];
        let n = r.read(&mut out).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&out, b"hello world");
    }

    #[test]
    fn reader_never_sees_empty_vec_after_concurrent_finalize() {
        // Stress test: finalize is racing with reader.  The single-Mutex
        // approach should make it impossible for the reader to observe the
        // empty-Vec intermediate state.
        let buf = Arc::new(SharedBuffer::new());
        let total: u64 = 1_000_000;
        buf.set_total_len(total);

        // Fill the buffer completely, then finalize while reading.
        {
            let mut w = buf.writer();
            let data = vec![0xABu8; total as usize];
            w.write(&data);
            w.finish();
        }

        let buf2 = buf.clone();
        let jh = thread::spawn(move || {
            let mut r = buf2.reader();
            let mut out = vec![0u8; 4096];
            let mut total_read = 0usize;
            loop {
                let n = r.read(&mut out).unwrap();
                if n == 0 {
                    break;
                }
                total_read += n;
                // All bytes must be 0xAB — if reader ever sees the empty
                // intermediate state, it would read stale/zero bytes.
                assert!(out[..n].iter().all(|&b| b == 0xAB),
                    "reader got corrupted data at offset {total_read}");
            }
            assert_eq!(total_read, total as usize);
        });

        // Finalize while reader is running.
        let arc = buf.finalize();
        assert_eq!(arc.len(), total as usize);

        jh.join().unwrap();
    }
}
