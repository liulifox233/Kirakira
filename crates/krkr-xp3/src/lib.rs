//! The XP3 archive reader, and the filter seam the reference's
//! `TVPSetXP3ArchiveExtractionFilter`/`TVPSetXP3ArchiveContentFilter` installs
//! into (`Kirikiroid2/src/core/base/XP3Archive.cpp:30-41`).
//!
//! The filter vocabulary itself lives in `krkr-core` (next to
//! `StorageMediaProvider`) so a plugin crate can name it without depending on
//! this crate; the types are re-exported here for callers that already read
//! archives through it. Archives consult the filters *live* — the content
//! filter when they create an entry stream, the extraction filter on every
//! read chunk — which is what lets a host install a filter after the project's
//! archives were opened.

mod archive;
mod cache;
mod entry;
mod options;
mod parse;
mod provider;
mod source;
mod stream;
mod util;

pub use archive::Xp3Archive;
pub use entry::{Xp3Entry, Xp3Segment, Xp3SegmentEncoding};
pub use krkr_core::{
    Xp3ContentFilter, Xp3ContentFilterAction, Xp3ExtractionFilter, Xp3ExtractionFilterInfo,
    Xp3FilterContext, Xp3FilterRegistry,
};
pub use options::{SegmentCacheConfig, Xp3OpenOptions};
pub use provider::{Xp3ResourceProvider, archive_qualifier_is_absolute};
pub use stream::Xp3EntryStream;
pub use util::{Result, Xp3Error, normalize_entry_name};

pub const XP3_MAGIC: [u8; 11] = [
    0x58, 0x50, 0x33, 0x0d, 0x0a, 0x20, 0x0a, 0x1a, 0x8b, 0x67, 0x01,
];

#[cfg(test)]
mod tests;
