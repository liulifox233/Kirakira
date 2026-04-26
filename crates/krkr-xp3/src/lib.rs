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
pub use options::{SegmentCacheConfig, Xp3OpenOptions};
pub use provider::Xp3ResourceProvider;
pub use stream::Xp3EntryStream;
pub use util::{Result, Xp3Error, Xp3ExtractionFilter, normalize_entry_name};

pub const XP3_MAGIC: [u8; 11] = [
    0x58, 0x50, 0x33, 0x0d, 0x0a, 0x20, 0x0a, 0x1a, 0x8b, 0x67, 0x01,
];

#[cfg(test)]
mod tests;
