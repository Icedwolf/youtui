use std::io::{Read, Result, Seek, SeekFrom};

use symphonia::core::io::MediaSource;

/// A read source that reports itself as seekable (or not) with an optional
/// known length. The seekable form is the default: it lets isomp4 seek to an
/// end-of-file moov atom (full-file M4A). The non-seekable form (constructed
/// via [`Self::nonseekable`]) drives isomp4 down its incremental streaming
/// code path — it must never seek, since a growing `SharedBuffer` cannot serve
/// seeks past the currently-available bytes. `MediaSource` requires `io::Seek`
/// on the trait bound regardless; the `is_seekable()` flag (not the bound)
/// drives symphonia's behavior.
pub struct ReadSeekSource<T: Read + Seek + Send + Sync> {
    inner: T,
    seekable: bool,
    length: Option<u64>,
}

impl<T: Read + Seek + Send + Sync> ReadSeekSource<T> {
    pub fn new(inner: T, length: Option<u64>) -> Self {
        ReadSeekSource { inner, seekable: true, length }
    }

    pub fn nonseekable(inner: T) -> Self {
        ReadSeekSource { inner, seekable: false, length: None }
    }
}

impl<T: Read + Seek + Send + Sync> MediaSource for ReadSeekSource<T> {
    fn is_seekable(&self) -> bool {
        self.seekable
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
