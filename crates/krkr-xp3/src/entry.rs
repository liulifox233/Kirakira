#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Xp3SegmentEncoding {
    Raw,
    Zlib,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Xp3Segment {
    pub encoding: Xp3SegmentEncoding,
    pub archive_offset: u64,
    pub uncompressed_offset: u64,
    pub uncompressed_size: u64,
    pub archived_size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Xp3Entry {
    pub name: String,
    pub flags: u32,
    pub original_size: u64,
    pub archived_size: u64,
    pub file_hash: u32,
    pub modified_time: Option<u64>,
    pub segments: Vec<Xp3Segment>,
}
