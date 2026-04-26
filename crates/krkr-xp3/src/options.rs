use std::{fmt, sync::Arc};

use crate::Xp3ExtractionFilter;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentCacheConfig {
    pub max_bytes: usize,
    pub max_segment_bytes: usize,
}

impl SegmentCacheConfig {
    pub const fn new(max_bytes: usize, max_segment_bytes: usize) -> Self {
        Self {
            max_bytes,
            max_segment_bytes,
        }
    }

    pub const fn disabled() -> Self {
        Self {
            max_bytes: 0,
            max_segment_bytes: 0,
        }
    }
}

impl Default for SegmentCacheConfig {
    fn default() -> Self {
        Self {
            max_bytes: 32 * 1024 * 1024,
            max_segment_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Default)]
pub struct Xp3OpenOptions {
    pub(crate) extraction_filter: Option<Arc<dyn Xp3ExtractionFilter>>,
    pub(crate) segment_cache: SegmentCacheConfig,
}

impl Xp3OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_extraction_filter<F>(mut self, filter: F) -> Self
    where
        F: Xp3ExtractionFilter + 'static,
    {
        self.extraction_filter = Some(Arc::new(filter));
        self
    }

    pub fn with_segment_cache_config(mut self, config: SegmentCacheConfig) -> Self {
        self.segment_cache = config;
        self
    }

    pub fn with_segment_cache_limits(mut self, max_bytes: usize, max_segment_bytes: usize) -> Self {
        self.segment_cache = SegmentCacheConfig::new(max_bytes, max_segment_bytes);
        self
    }
}

impl fmt::Debug for Xp3OpenOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Xp3OpenOptions")
            .field("extraction_filter", &self.extraction_filter.is_some())
            .field("segment_cache", &self.segment_cache)
            .finish()
    }
}
