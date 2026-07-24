//! Video decoding backends for the KRKR VideoOverlay object.
//!
//! Like krkr2/krkrz (DirectShow / Media Foundation), this crate does not
//! bundle a codec; it decodes through the host platform's own media
//! framework. Backends are pluggable per platform behind the
//! [`VideoDecoder`] trait:
//!
//! - `macos-avfoundation` (macOS): AVFoundation `AVAssetReader`, zero-copy
//!   BGRA frames out of `CVPixelBuffer`.

use std::{error::Error, fmt, path::Path};

#[cfg(all(target_os = "macos", feature = "macos-avfoundation"))]
mod macos;

/// Static stream description for one opened video.
#[derive(Clone, Debug)]
pub struct VideoMetadata {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frame_count: i64,
    pub duration_ms: i64,
    pub has_audio: bool,
}

/// One decoded video frame, 32-bit RGBA, tightly packed rows.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub pts_ms: i64,
    pub width: u32,
    pub height: u32,
    /// Bytes per row in `data`; may exceed `width * 4`.
    pub stride: u32,
    pub data: Vec<u8>,
}

/// Format of the decoded audio stream of an opened video.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioSpec {
    pub sample_rate: u32,
    pub channels: u32,
}

/// One decoded movie-audio chunk: interleaved f32 samples in the layout
/// described by [`AudioSpec`].
#[derive(Clone, Debug)]
pub struct AudioChunk {
    pub pts_ms: i64,
    pub samples: Vec<f32>,
}

/// Sequential decoder interface. Implementations must be `Send` so the
/// engine can run them on a dedicated decode thread with back-pressure.
pub trait VideoDecoder: Send {
    fn metadata(&self) -> &VideoMetadata;
    /// Returns the next frame in stream order, or `None` at end of stream.
    fn next_frame(&mut self) -> Result<Option<VideoFrame>, VideoError>;
    /// Restarts decoding at (approximately) the given position.
    fn seek_ms(&mut self, ms: i64) -> Result<(), VideoError>;
    /// Audio layout of the movie's soundtrack, when it has one. Like
    /// krkr2/krkrz's system-decoder graphs, movie audio is decoded by the
    /// same backend as the video — the engine feeds this PCM to its audio
    /// system instead of routing the movie *file* through the audio stack.
    fn audio_spec(&self) -> Option<AudioSpec> {
        None
    }
    /// Returns the next decoded audio chunk in stream order, or `None` at
    /// end of stream. Only called when [`Self::audio_spec`] is `Some`.
    fn next_audio_chunk(&mut self) -> Result<Option<AudioChunk>, VideoError> {
        Ok(None)
    }
}

/// Errors a backend can report.
#[derive(Debug)]
pub enum VideoError {
    /// No backend on this platform can handle the stream/container.
    Unsupported(String),
    /// The file could not be read or parsed.
    Open(String),
    /// Decoding failed mid-stream.
    Decode(String),
}

impl fmt::Display for VideoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoError::Unsupported(msg) => write!(f, "unsupported video: {msg}"),
            VideoError::Open(msg) => write!(f, "cannot open video: {msg}"),
            VideoError::Decode(msg) => write!(f, "video decode failed: {msg}"),
        }
    }
}

impl Error for VideoError {}

/// Opens `path` with the best backend for the current platform.
pub fn create_decoder(path: &Path) -> Result<Box<dyn VideoDecoder>, VideoError> {
    #[cfg(all(target_os = "macos", feature = "macos-avfoundation"))]
    {
        return macos::AvfDecoder::open(path).map(|decoder| Box::new(decoder) as _);
    }
    #[allow(unreachable_code)]
    Err(VideoError::Unsupported(format!(
        "no video backend for this platform ({})",
        std::env::consts::OS
    )))
}
