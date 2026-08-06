use std::io::{Read, Result, Seek, SeekFrom};

use symphonia::core::io::MediaSource;

pub struct ReadSeekSource<T: Read + Seek + Send + Sync> {
    inner: T,
    length: Option<u64>,
}

impl<T: Read + Seek + Send + Sync> ReadSeekSource<T> {
    pub fn new(inner: T, length: Option<u64>) -> Self {
        ReadSeekSource { inner, length }
    }
}

impl<T: Read + Seek + Send + Sync> MediaSource for ReadSeekSource<T> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.length
    }
}

impl<T: Read + Seek + Send + Sync> Read for ReadSeekSource<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.read(buf)
    }
}

impl<T: Read + Seek + Send + Sync> Seek for ReadSeekSource<T> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.inner.seek(pos)
    }
}

/// Read-only media source that reports itself as non-seekable with unknown
/// length. isomp4 uses this to take its incremental (streaming) code path —
/// it must never seek, since a growing `SharedBuffer` cannot serve seeks
/// past the currently-available bytes. `MediaSource` requires `io::Seek` on
/// the trait; the `is_seekable()` flag (not the bound) drives symphonia's
/// behavior, and this type reports `false`.
pub struct NonSeekableReadSource<T: Read + Seek + Send + Sync> {
    inner: T,
}

impl<T: Read + Seek + Send + Sync> NonSeekableReadSource<T> {
    pub fn new(inner: T) -> Self {
        NonSeekableReadSource { inner }
    }
}

impl<T: Read + Seek + Send + Sync> MediaSource for NonSeekableReadSource<T> {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

impl<T: Read + Seek + Send + Sync> Read for NonSeekableReadSource<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.read(buf)
    }
}

impl<T: Read + Seek + Send + Sync> Seek for NonSeekableReadSource<T> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.inner.seek(pos)
    }
}
