use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use symphonia::core::io::MediaSourceStream;

use crate::core::PoisonRecovery;
use crate::decoder::SymphoniaDecoder;
use crate::decoder::read_seek_source::ReadSeekSource;

pub(crate) static CACHE_MAX_ENTRIES: AtomicUsize = AtomicUsize::new(1);

pub fn set_cache_max_entries(val: usize) {
    CACHE_MAX_ENTRIES.store(val.clamp(0, 100), Ordering::Release);
}

struct AudioCache {
    data: HashMap<String, Arc<[u8]>>,
    order: VecDeque<String>,
}

impl AudioCache {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn put(&mut self, key: String, data: Arc<[u8]>) {
        let max = CACHE_MAX_ENTRIES.load(Ordering::Acquire);
        if max > 0
            && self.data.len() >= max
            && let Some(old) = self.order.pop_front()
        {
            self.data.remove(&old);
        }
        if max > 0 {
            self.data.insert(key.clone(), data);
            self.order.push_back(key);
        }
    }

    fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        let hit = self.data.get(key).cloned()?;
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
            self.order.push_back(key.to_string());
        }
        Some(hit)
    }
}

static BYTE_CACHE: LazyLock<Mutex<AudioCache>> = LazyLock::new(|| Mutex::new(AudioCache::new()));

pub(crate) fn cache_put(key: String, data: Arc<[u8]>) {
    BYTE_CACHE.lock().unwrap_or_warn().put(key, data);
}

pub(crate) fn cache_get(key: &str) -> Option<Arc<[u8]>> {
    BYTE_CACHE.lock().unwrap_or_warn().get(key)
}

pub fn cache_clear() {
    let mut cache = BYTE_CACHE.lock().unwrap_or_warn();
    cache.data.clear();
    cache.order.clear();
}

pub fn create_decoder_from_cache(video_id: &str) -> Option<SymphoniaDecoder> {
    let cached = cache_get(video_id)?;
    let len = cached.len() as u64;
    let cursor = std::io::Cursor::new(cached);
    let source = ReadSeekSource::new(cursor, Some(len));
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    SymphoniaDecoder::new(mss).ok()
}
