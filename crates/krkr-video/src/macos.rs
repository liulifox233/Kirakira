//! macOS video backend on top of AVFoundation (`AVAssetReader`).
//!
//! Decoded frames come out of `CVPixelBuffer` in 32-bit BGRA and are copied
//! row-wise into engine-owned buffers, honouring the pixel buffer stride.

use std::ptr::NonNull;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use objc2::{AllocAnyThread as _, rc::Retained, runtime::AnyObject};
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderTrackOutput, AVMediaTypeAudio, AVMediaTypeVideo, AVURLAsset,
};
use objc2_core_audio_types::kAudioFormatLinearPCM;
use objc2_core_foundation::CFString;
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMFormatDescription, CMTime, CMTimeRange,
};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use crate::{
    AudioChunk, AudioSpec, VideoDecoder, VideoError, VideoFrame, VideoMetadata, VideoPort,
    VideoSource,
};

pub struct AvfDecoder {
    /// Bytes-backed sources are staged by this backend only because the
    /// current AVFoundation bindings expose URL assets. Keeping ownership
    /// here (rather than in VideoOverlay) leaves mobile/browser backends free
    /// to use in-memory/native media APIs without temporary files.
    owned_path: Option<PathBuf>,
    asset: Retained<AVURLAsset>,
    track: Retained<objc2_av_foundation::AVAssetTrack>,
    audio_track: Option<Retained<objc2_av_foundation::AVAssetTrack>>,
    output_settings: Retained<NSDictionary<NSString, AnyObject>>,
    reader: Option<Retained<AVAssetReader>>,
    output: Option<Retained<AVAssetReaderTrackOutput>>,
    audio_output: Option<Retained<AVAssetReaderTrackOutput>>,
    metadata: VideoMetadata,
    audio_spec: Option<AudioSpec>,
}

impl AvfDecoder {
    pub fn open(source: VideoSource) -> Result<Self, VideoError> {
        let (path, owned_path) = match source {
            VideoSource::Path(path) => (path, None),
            VideoSource::Bytes { data, name } => {
                let extension = sniff_video_extension(&data, name.as_deref());
                let path = stage_bytes(&data, &extension)?;
                (path.clone(), Some(path))
            }
        };
        let result = Self::open_path(&path);
        match result {
            Ok(mut decoder) => {
                decoder.owned_path = owned_path;
                Ok(decoder)
            }
            Err(error) => {
                if let Some(path) = owned_path {
                    let _ = fs::remove_file(path);
                }
                Err(error)
            }
        }
    }

    fn open_path(path: &Path) -> Result<Self, VideoError> {
        let url = NSURL::from_file_path(path)
            .ok_or_else(|| VideoError::Open(format!("invalid path {}", path.display())))?;
        let asset = unsafe { AVURLAsset::assetWithURL(&url) };
        let video_type = unsafe { AVMediaTypeVideo }.ok_or(VideoError::Unsupported(
            "AVMediaTypeVideo is unavailable".to_string(),
        ))?;
        let audio_type = unsafe { AVMediaTypeAudio }.ok_or(VideoError::Unsupported(
            "AVMediaTypeAudio is unavailable".to_string(),
        ))?;
        let video_tracks = unsafe { asset.tracksWithMediaType(video_type) };
        if video_tracks.is_empty() {
            return Err(VideoError::Unsupported(format!(
                "{} has no video track readable by AVFoundation",
                path.display()
            )));
        }
        let track = video_tracks
            .firstObject()
            .ok_or_else(|| VideoError::Open(format!("{}: empty video track", path.display())))?;
        let has_audio = !unsafe { asset.tracksWithMediaType(audio_type) }.is_empty();
        let audio_track = unsafe { asset.tracksWithMediaType(audio_type) }.firstObject();
        let audio_spec = audio_track
            .as_ref()
            .and_then(|track| audio_track_spec(track));
        let size = unsafe { track.naturalSize() };
        let fps = f64::from(unsafe { track.nominalFrameRate() });
        let duration_seconds = unsafe { asset.duration().seconds() };
        if size.width <= 0.0 || size.height <= 0.0 || fps <= 0.0 || duration_seconds <= 0.0 {
            return Err(VideoError::Open(format!(
                "{}: unreadable stream properties ({}x{}, {} fps, {} s)",
                path.display(),
                size.width,
                size.height,
                fps,
                duration_seconds
            )));
        }
        let duration_ms = (duration_seconds * 1000.0).round() as i64;
        let metadata = VideoMetadata {
            width: size.width as u32,
            height: size.height as u32,
            fps,
            frame_count: (duration_seconds * fps).round() as i64,
            duration_ms,
            has_audio,
        };
        let output_settings = bgra_output_settings();
        let mut decoder = Self {
            owned_path: None,
            asset,
            track,
            audio_track,
            output_settings,
            reader: None,
            output: None,
            audio_output: None,
            metadata,
            audio_spec,
        };
        decoder.rebuild_reader(0)?;
        Ok(decoder)
    }

    /// Creates a fresh reader decoding from `start_ms` onwards.
    fn rebuild_reader(&mut self, start_ms: i64) -> Result<(), VideoError> {
        let reader = unsafe {
            AVAssetReader::initWithAsset_error(AVAssetReader::alloc(), &self.asset)
                .map_err(|error| VideoError::Open(error.localizedDescription().to_string()))?
        };
        let output = unsafe {
            AVAssetReaderTrackOutput::initWithTrack_outputSettings(
                AVAssetReaderTrackOutput::alloc(),
                &self.track,
                Some(self.output_settings.as_ref()),
            )
        };
        if !unsafe { reader.canAddOutput(&output) } {
            return Err(VideoError::Open(
                "AVAssetReader rejected the BGRA track output".to_string(),
            ));
        }
        unsafe { reader.addOutput(&output) };
        let audio_output = if let Some(audio_track) = &self.audio_track {
            let audio_output = unsafe {
                AVAssetReaderTrackOutput::initWithTrack_outputSettings(
                    AVAssetReaderTrackOutput::alloc(),
                    audio_track,
                    Some(lpcm_output_settings().as_ref()),
                )
            };
            if !unsafe { reader.canAddOutput(&audio_output) } {
                return Err(VideoError::Open(
                    "AVAssetReader rejected the LPCM audio track output".to_string(),
                ));
            }
            unsafe { reader.addOutput(&audio_output) };
            Some(audio_output)
        } else {
            None
        };
        if start_ms > 0 {
            let start = unsafe { CMTime::new(start_ms.max(0), 1000) };
            // An explicit infinite duration makes AVAssetReader fail to
            // start; bound the range to the asset end instead.
            let range = unsafe { CMTimeRange::from_time_to_time(start, self.asset.duration()) };
            unsafe { reader.setTimeRange(range) };
        }
        if !unsafe { reader.startReading() } {
            let message = unsafe { reader.error() }
                .map(|error| {
                    let description = error.localizedDescription().to_string();
                    match error.localizedFailureReason() {
                        Some(reason) => format!("{description} ({reason})"),
                        None => description,
                    }
                })
                .unwrap_or_else(|| "unknown AVAssetReader error".to_string());
            return Err(VideoError::Open(format!(
                "AVAssetReader failed to start: {message}"
            )));
        }
        self.reader = Some(reader);
        self.output = Some(output);
        self.audio_output = audio_output;
        Ok(())
    }
}

impl Drop for AvfDecoder {
    fn drop(&mut self) {
        if let Some(path) = self.owned_path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn sniff_video_extension(bytes: &[u8], name: Option<&str>) -> String {
    const ASF_GUID: &[u8] = &[
        0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE,
        0x6C,
    ];
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return ".mp4".to_string();
    }
    if bytes.starts_with(ASF_GUID) {
        return ".wmv".to_string();
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"AVI " {
        return ".avi".to_string();
    }
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0xBA]) {
        return ".mpg".to_string();
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return ".webm".to_string();
    }
    name.and_then(|name| Path::new(name).extension().and_then(|ext| ext.to_str()))
        .map(|ext| format!(".{ext}"))
        .unwrap_or_else(|| ".mp4".to_string())
}

fn stage_bytes(bytes: &[u8], extension: &str) -> Result<PathBuf, VideoError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "krkr-video-{}-{}{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        extension
    ));
    fs::write(&path, bytes).map_err(|error| {
        VideoError::Open(format!("cannot stage video for AVFoundation: {error}"))
    })?;
    Ok(path)
}

fn bgra_output_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    // kCVPixelBufferPixelFormatTypeKey is a CFString; NSString is toll-free
    // bridged with it, which is what AVFoundation expects here.
    let key: &NSString =
        unsafe { &*(kCVPixelBufferPixelFormatTypeKey as *const CFString).cast::<NSString>() };
    let format = NSNumber::numberWithUnsignedInt(kCVPixelFormatType_32BGRA);
    let value: &AnyObject = format.as_ref();
    NSDictionary::from_slices(&[key], &[value])
}

/// Requests interleaved 32-bit float LPCM from the audio track output. The
/// AVAudioSettings.h keys are not yet bound in objc2-av-foundation, but their
/// string values are identical to their names. (`AVLinearPCMIsNonInterleavedKey`
/// is deliberately absent: AVAssetReaderTrackOutput rejects it, and packed
/// interleaved output is the default.)
fn lpcm_output_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys = [
        NSString::from_str("AVFormatIDKey"),
        NSString::from_str("AVLinearPCMBitDepthKey"),
        NSString::from_str("AVLinearPCMIsFloatKey"),
        NSString::from_str("AVLinearPCMIsBigEndianKey"),
    ];
    let values = [
        NSNumber::numberWithUnsignedInt(kAudioFormatLinearPCM),
        NSNumber::numberWithUnsignedInt(32),
        NSNumber::numberWithBool(true),
        NSNumber::numberWithBool(false),
    ];
    let key_refs: Vec<&NSString> = keys.iter().map(|key| key.as_ref()).collect();
    let value_refs: Vec<&AnyObject> = values.iter().map(|value| value.as_ref()).collect();
    NSDictionary::from_slices(&key_refs, &value_refs)
}

/// Reads the audio track's source format (sample rate / channel count) from
/// its first format description. LPCM output keeps the source layout when no
/// rate/channel keys are specified.
fn audio_track_spec(track: &objc2_av_foundation::AVAssetTrack) -> Option<AudioSpec> {
    let descriptions = unsafe { track.formatDescriptions() };
    let first = descriptions.firstObject()?;
    let description: &CMFormatDescription =
        unsafe { &*(Retained::as_ptr(&first).cast::<CMFormatDescription>()) };
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(description) };
    let asbd = unsafe { asbd.as_ref()? };
    if asbd.mSampleRate <= 0.0 || asbd.mChannelsPerFrame == 0 {
        return None;
    }
    Some(AudioSpec {
        sample_rate: asbd.mSampleRate.round() as u32,
        channels: asbd.mChannelsPerFrame,
    })
}

// AVAssetReader/AVAssetTrack are safe to use from any thread as long as
// access is externally synchronized; the engine moves the decoder onto its
// dedicated decode thread and only touches it there.
unsafe impl Send for AvfDecoder {}

impl VideoDecoder for AvfDecoder {
    fn metadata(&self) -> &VideoMetadata {
        &self.metadata
    }

    fn next_frame(&mut self) -> Result<Option<VideoFrame>, VideoError> {
        let output = self
            .output
            .as_ref()
            .ok_or_else(|| VideoError::Decode("decoder is not started".to_string()))?;
        let sample = match unsafe { output.copyNextSampleBuffer() } {
            Some(sample) => sample,
            None => {
                if let Some(reader) = &self.reader {
                    let status = unsafe { reader.status() };
                    if status.0 == objc2_av_foundation::AVAssetReaderStatus::Failed.0 {
                        let message = unsafe { reader.error() }
                            .map(|error| error.localizedDescription().to_string())
                            .unwrap_or_else(|| "unknown error".to_string());
                        return Err(VideoError::Decode(format!(
                            "AVAssetReader failed mid-stream: {message}"
                        )));
                    }
                }
                return Ok(None);
            }
        };
        let pts_seconds = unsafe { sample.presentation_time_stamp().seconds() };
        let image = unsafe { sample.image_buffer() }
            .ok_or_else(|| VideoError::Decode("sample buffer carries no image".to_string()))?;
        let width = CVPixelBufferGetWidth(&image);
        let height = CVPixelBufferGetHeight(&image);
        let stride = CVPixelBufferGetBytesPerRow(&image);
        let flags = CVPixelBufferLockFlags(0);
        let lock_result = unsafe { CVPixelBufferLockBaseAddress(&image, flags) };
        if lock_result != 0 {
            return Err(VideoError::Decode(format!(
                "CVPixelBufferLockBaseAddress failed ({lock_result})"
            )));
        }
        let data = unsafe {
            let base = CVPixelBufferGetBaseAddress(&image);
            if base.is_null() {
                CVPixelBufferUnlockBaseAddress(&image, flags);
                return Err(VideoError::Decode(
                    "pixel buffer has no base address".to_string(),
                ));
            }
            let row_bytes = width * 4;
            let mut data = Vec::with_capacity(row_bytes * height);
            let source = base.cast::<u8>();
            for row in 0..height {
                let row_ptr = source.add(row * stride);
                data.extend_from_slice(std::slice::from_raw_parts(row_ptr, row_bytes));
            }
            // The renderer expects RGBA; AVFoundation only hands out BGRA.
            for pixel in data.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            CVPixelBufferUnlockBaseAddress(&image, flags);
            data
        };
        Ok(Some(VideoFrame {
            pts_ms: (pts_seconds * 1000.0).round() as i64,
            width: width as u32,
            height: height as u32,
            stride: (width * 4) as u32,
            data,
        }))
    }

    fn seek_ms(&mut self, ms: i64) -> Result<(), VideoError> {
        self.rebuild_reader(ms)
    }

    fn audio_spec(&self) -> Option<AudioSpec> {
        self.audio_spec
    }

    fn next_audio_chunk(&mut self) -> Result<Option<AudioChunk>, VideoError> {
        let Some(output) = self.audio_output.as_ref() else {
            return Ok(None);
        };
        let sample = match unsafe { output.copyNextSampleBuffer() } {
            Some(sample) => sample,
            None => {
                if let Some(reader) = &self.reader {
                    let status = unsafe { reader.status() };
                    if status.0 == objc2_av_foundation::AVAssetReaderStatus::Failed.0 {
                        let message = unsafe { reader.error() }
                            .map(|error| error.localizedDescription().to_string())
                            .unwrap_or_else(|| "unknown error".to_string());
                        return Err(VideoError::Decode(format!(
                            "AVAssetReader failed mid-stream: {message}"
                        )));
                    }
                }
                return Ok(None);
            }
        };
        let pts_seconds = unsafe { sample.presentation_time_stamp().seconds() };
        let block = unsafe { sample.data_buffer() }
            .ok_or_else(|| VideoError::Decode("audio sample buffer carries no data".to_string()))?;
        let byte_len = unsafe { block.data_length() };
        let mut bytes = vec![0u8; byte_len];
        let destination = NonNull::new(bytes.as_mut_ptr().cast::<std::ffi::c_void>())
            .expect("audio buffer pointer is never null");
        let status = unsafe { block.copy_data_bytes(0, byte_len, destination) };
        if status != 0 {
            return Err(VideoError::Decode(format!(
                "CMBlockBufferCopyDataBytes failed ({status})"
            )));
        }
        let samples = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        Ok(Some(AudioChunk {
            pts_ms: (pts_seconds * 1000.0).round() as i64,
            samples,
        }))
    }
}

impl VideoPort for AvfDecoder {}
