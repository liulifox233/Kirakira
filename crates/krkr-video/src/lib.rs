//! Video decoding backends for the KRKR VideoOverlay object.
//!
//! Like krkr2/krkrz (DirectShow / Media Foundation), this crate does not
//! bundle a codec; it decodes through the host platform's own media
//! framework. Backends are pluggable per platform behind the
//! [`VideoPort`] protocol and its [`VideoDecoder`] decode stream:
//!
//! - `macos-avfoundation` (macOS): AVFoundation `AVAssetReader`, zero-copy
//!   BGRA frames out of `CVPixelBuffer`.

use std::{error::Error, fmt, path::PathBuf, sync::Arc};

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

/// Backend selected by a host shell. The enum is deliberately independent of
/// a concrete decoder so capability negotiation can happen before opening a
/// resource (for example, a Web shell can choose an HTML media element).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoBackendKind {
    MacosAvFoundation,
    AndroidMediaCodec,
    IosAvFoundation,
    WebMediaElement,
    Unavailable,
}

/// Container/transport capabilities exposed by a video backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoCapabilities {
    pub backend: VideoBackendKind,
    pub mp4: bool,
    pub webm: bool,
    pub mpeg: bool,
    pub wmv: bool,
    pub avi: bool,
    pub in_memory: bool,
    pub soundtrack_pcm: bool,
}

impl VideoCapabilities {
    pub const fn unavailable() -> Self {
        Self {
            backend: VideoBackendKind::Unavailable,
            mp4: false,
            webm: false,
            mpeg: false,
            wmv: false,
            avi: false,
            in_memory: false,
            soundtrack_pcm: false,
        }
    }
}

/// Returns the capability profile for the current target. Android/iOS and
/// Web profiles are protocol declarations today; their shells can provide a
/// native/media-element `VideoPort` without linking the macOS decoder.
pub const fn platform_capabilities() -> VideoCapabilities {
    #[cfg(all(target_os = "macos", feature = "macos-avfoundation"))]
    {
        return VideoCapabilities {
            backend: VideoBackendKind::MacosAvFoundation,
            mp4: true,
            webm: true,
            mpeg: true,
            wmv: true,
            avi: true,
            in_memory: true,
            soundtrack_pcm: true,
        };
    }
    #[cfg(target_os = "android")]
    {
        return VideoCapabilities {
            backend: VideoBackendKind::AndroidMediaCodec,
            mp4: true,
            webm: true,
            mpeg: true,
            wmv: false,
            avi: false,
            in_memory: true,
            soundtrack_pcm: true,
        };
    }
    #[cfg(any(target_os = "ios", target_os = "tvos"))]
    {
        return VideoCapabilities {
            backend: VideoBackendKind::IosAvFoundation,
            mp4: true,
            webm: false,
            mpeg: true,
            wmv: false,
            avi: false,
            in_memory: true,
            soundtrack_pcm: true,
        };
    }
    #[cfg(target_arch = "wasm32")]
    {
        return VideoCapabilities {
            backend: VideoBackendKind::WebMediaElement,
            mp4: true,
            webm: true,
            mpeg: false,
            wmv: false,
            avi: false,
            in_memory: false,
            soundtrack_pcm: false,
        };
    }
    #[cfg(not(any(
        all(target_os = "macos", feature = "macos-avfoundation"),
        target_os = "android",
        any(target_os = "ios", target_os = "tvos"),
        target_arch = "wasm32"
    )))]
    {
        VideoCapabilities::unavailable()
    }
}

/// Input owned by the host when opening a movie.
///
/// Engine code normally uses [`VideoSource::Bytes`], keeping storage and
/// decoder lifetimes independent from filesystem paths. `Path` is retained
/// for desktop diagnostics and tools that intentionally open a local file.
#[derive(Clone, Debug)]
pub enum VideoSource {
    Path(PathBuf),
    Bytes {
        data: Arc<[u8]>,
        /// Optional filename/extension hint for system frameworks whose
        /// container sniffing relies on a URL suffix.
        name: Option<String>,
    },
}

impl VideoSource {
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self::Path(path.into())
    }

    pub fn bytes(data: impl Into<Arc<[u8]>>, name: Option<impl Into<String>>) -> Self {
        Self::Bytes {
            data: data.into(),
            name: name.map(Into::into),
        }
    }
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

/// Host-facing media port used by `VideoOverlay`.
///
/// Keeping this as a separate protocol lets mobile and browser shells provide
/// native media-element/framework implementations without changing the
/// engine's decode-session code. The blanket implementation makes existing
/// decoders source-compatible while the factory returns the protocol type.
pub trait VideoPort: VideoDecoder {
    fn capabilities(&self) -> VideoCapabilities {
        platform_capabilities()
    }
}

impl<T: VideoDecoder + ?Sized> VideoPort for T {}

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

/// Opens a host-owned source with the best backend for the current platform.
pub fn create_decoder(source: VideoSource) -> Result<Box<dyn VideoPort>, VideoError> {
    #[cfg(all(target_os = "macos", feature = "macos-avfoundation"))]
    {
        return macos::AvfDecoder::open(source).map(|decoder| Box::new(decoder) as _);
    }
    #[cfg(not(all(target_os = "macos", feature = "macos-avfoundation")))]
    let _ = source;
    #[allow(unreachable_code)]
    Err(VideoError::Unsupported(format!(
        "no video backend for this platform ({})",
        std::env::consts::OS
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_source_keeps_host_owned_bytes_and_name() {
        let source = VideoSource::bytes(vec![1, 2, 3], Some("movie.op"));
        let VideoSource::Bytes { data, name } = source else {
            panic!("expected bytes source");
        };
        assert_eq!(data.as_ref(), &[1, 2, 3]);
        assert_eq!(name.as_deref(), Some("movie.op"));
    }

    #[test]
    fn unavailable_capabilities_never_claim_a_codec() {
        let capabilities = VideoCapabilities::unavailable();
        assert_eq!(capabilities.backend, VideoBackendKind::Unavailable);
        assert!(!capabilities.mp4);
        assert!(!capabilities.in_memory);
    }
}
