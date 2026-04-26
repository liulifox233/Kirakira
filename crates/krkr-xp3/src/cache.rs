use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::{Arc, Mutex},
};

use crate::SegmentCacheConfig;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SegmentCacheKey {
    pub(crate) entry_index: usize,
    pub(crate) segment_index: usize,
}

pub(crate) struct SegmentCache {
    config: SegmentCacheConfig,
    state: Mutex<SegmentCacheState>,
}

#[derive(Default)]
struct SegmentCacheState {
    segments: HashMap<SegmentCacheKey, CacheEntry>,
    lru: VecDeque<SegmentCacheKey>,
    total_bytes: usize,
}

struct CacheEntry {
    data: Arc<[u8]>,
    bytes: usize,
}

impl SegmentCache {
    pub(crate) fn new(config: SegmentCacheConfig) -> Self {
        Self {
            config,
            state: Mutex::new(SegmentCacheState::default()),
        }
    }

    pub(crate) fn get(&self, key: SegmentCacheKey) -> io::Result<Option<Arc<[u8]>>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("XP3 segment cache lock poisoned"))?;
        let data = state
            .segments
            .get(&key)
            .map(|entry| Arc::clone(&entry.data));
        if data.is_some() {
            state.touch(key);
        }
        Ok(data)
    }

    pub(crate) fn insert(&self, key: SegmentCacheKey, data: Arc<[u8]>) -> io::Result<()> {
        let bytes = data.len();
        if self.config.max_bytes == 0
            || self.config.max_segment_bytes == 0
            || bytes > self.config.max_segment_bytes
            || bytes > self.config.max_bytes
        {
            return Ok(());
        }

        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("XP3 segment cache lock poisoned"))?;
        state.remove(key);
        state.total_bytes += bytes;
        state.lru.push_back(key);
        state.segments.insert(key, CacheEntry { data, bytes });
        state.evict_to_limit(self.config.max_bytes);
        Ok(())
    }

    pub(crate) fn clear(&self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("XP3 segment cache lock poisoned"))?;
        state.segments.clear();
        state.lru.clear();
        state.total_bytes = 0;
        Ok(())
    }

    pub(crate) fn config(&self) -> SegmentCacheConfig {
        self.config
    }
}

impl SegmentCacheState {
    fn touch(&mut self, key: SegmentCacheKey) {
        self.remove_lru_key(key);
        self.lru.push_back(key);
    }

    fn remove(&mut self, key: SegmentCacheKey) {
        if let Some(entry) = self.segments.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
        }
        self.remove_lru_key(key);
    }

    fn remove_lru_key(&mut self, key: SegmentCacheKey) {
        if let Some(position) = self.lru.iter().position(|candidate| *candidate == key) {
            self.lru.remove(position);
        }
    }

    fn evict_to_limit(&mut self, max_bytes: usize) {
        while self.total_bytes > max_bytes {
            let Some(key) = self.lru.pop_front() else {
                self.total_bytes = 0;
                break;
            };
            if let Some(entry) = self.segments.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_evicts_least_recently_used_segments() {
        let cache = SegmentCache::new(SegmentCacheConfig::new(4, 4));
        let first = SegmentCacheKey {
            entry_index: 0,
            segment_index: 0,
        };
        let second = SegmentCacheKey {
            entry_index: 0,
            segment_index: 1,
        };
        let third = SegmentCacheKey {
            entry_index: 0,
            segment_index: 2,
        };

        cache.insert(first, Arc::from(&b"aa"[..])).expect("insert");
        cache.insert(second, Arc::from(&b"bb"[..])).expect("insert");
        assert!(cache.get(first).expect("get").is_some());
        cache.insert(third, Arc::from(&b"cc"[..])).expect("insert");

        assert!(cache.get(first).expect("get").is_some());
        assert!(cache.get(second).expect("get").is_none());
        assert!(cache.get(third).expect("get").is_some());
    }

    #[test]
    fn cache_skips_segments_over_single_segment_limit() {
        let cache = SegmentCache::new(SegmentCacheConfig::new(10, 2));
        let key = SegmentCacheKey {
            entry_index: 0,
            segment_index: 0,
        };

        cache.insert(key, Arc::from(&b"abc"[..])).expect("insert");

        assert!(cache.get(key).expect("get").is_none());
    }
}
