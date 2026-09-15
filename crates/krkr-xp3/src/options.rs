use std::{fmt, sync::Arc};

use krkr_core::Xp3FilterRegistry;

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
    /// The filter slots every entry stream of the archives consults. It is
    /// *shared*, not copied: the archive reads the slots when it creates a
    /// stream and on each read, so a filter installed after the archive was
    /// opened still applies — the reference's ordering
    /// (`XP3Archive.cpp:585`, `:1047`).
    pub(crate) filter_registry: Option<Arc<Xp3FilterRegistry>>,
    /// The name the content filter is told the archive has
    /// (`tTVPXP3Archive::ArchiveName`, `XP3Archive.cpp:586`). `open_file*`
    /// fills it from the path; a reader-only archive has no name of its own.
    pub(crate) archive_name: Option<String>,
    pub(crate) segment_cache: SegmentCacheConfig,
}

impl Xp3OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// The registry the archives hold a handle to. Whoever keeps an [`Arc`] of
    /// it can install or clear filters after the archives are open, which is
    /// what the host storage hands its plugins.
    ///
    /// There is deliberately no "install this filter now" shortcut: a filter
    /// the archive captured at open time is the design the reference does not
    /// have (its callbacks are read per stream and per read,
    /// `XP3Archive.cpp:585`, `:1047`).
    pub fn with_filter_registry(mut self, registry: Arc<Xp3FilterRegistry>) -> Self {
        self.filter_registry = Some(registry);
        self
    }

    /// The name the content filter receives as `archivename`. `open_file*`
    /// defaults it to the file's name.
    pub fn with_archive_name(mut self, name: impl Into<String>) -> Self {
        self.archive_name = Some(name.into());
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
            .field(
                "filter_registry",
                &self
                    .filter_registry
                    .as_ref()
                    .map(|registry| registry.generation()),
            )
            .field("archive_name", &self.archive_name)
            .field("segment_cache", &self.segment_cache)
            .finish()
    }
}
