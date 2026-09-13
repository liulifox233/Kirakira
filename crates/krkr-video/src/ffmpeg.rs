//! FFmpeg fallback backend on the host's system FFmpeg libraries.
//!
//! The `ffmpeg-next` bindings find libavformat/libavcodec/libswscale/
//! libswresample through pkg-config, so this backend decodes with whatever
//! FFmpeg the host provides — never a bundled decoder and never the `ffmpeg`
//! CLI, the same philosophy as the AVFoundation backend using the OS
//! framework. Sources are opened through a custom AVIO over the host's own
//! bytes, so in-memory storage never stages a temporary file (unlike the
//! AVFoundation bindings, which only take URL assets).
//!
//! Contract notes that the trait does not spell out:
//!
//! - The engine's `DecodeSession` moves the boxed decoder onto its own decode
//!   thread and only ever touches it there, so the type is `Send` but need not
//!   be `Sync` (libav* objects are thread-affine to their user).
//! - Video and audio are pulled in lockstep by that one thread (one
//!   `next_frame` and one `next_audio_chunk` per iteration), so demuxed
//!   packets are routed into per-stream queues instead of being dropped when
//!   the other stream is being pulled.
//! - `seek_ms` seeks the demuxer to the keyframe at or before the target and
//!   flushes both decoders; frames and audio chunks that still precede the
//!   target are dropped inside `next_frame`/`next_audio_chunk`, so callers see
//!   media from (approximately) the requested position, like AVAssetReader's
//!   time range does.
//! - EOF is sticky until the next `seek_ms`; the engine parks a session at end
//!   of stream and rewinds it with a seek.
//! - The soundtrack is delivered as interleaved f32 at the stream's own sample
//!   rate with the sample count the [`AudioSpec`] advertises (swresample maps
//!   the decoder's native layout onto it).

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

use ffmpeg_next::codec;
use ffmpeg_next::format::{Pixel, Sample, sample::Type as SampleType};
use ffmpeg_next::frame;
use ffmpeg_next::software;
use ffmpeg_next::{ChannelLayout, Error as FfError, Packet, Rational, ffi, format, media};

use crate::{
    AudioChunk, AudioSpec, VideoDecoder, VideoError, VideoFrame, VideoMetadata, VideoPort,
    VideoSource,
};

pub struct FfmpegDecoder {
    demuxer: Demuxer,
    video: VideoStream,
    audio: Option<AudioStream>,
    metadata: VideoMetadata,
    audio_spec: Option<AudioSpec>,
}

impl FfmpegDecoder {
    pub fn open(source: VideoSource) -> Result<Self, VideoError> {
        let (input, label) = open_input(source)?;
        let mut video_selection = None;
        let mut audio_selection = None;
        for stream in input.streams() {
            let parameters = stream.parameters();
            match parameters.medium() {
                media::Type::Video if video_selection.is_none() => {
                    video_selection = Some(StreamSelection {
                        index: stream.index(),
                        time_base: stream.time_base(),
                        parameters,
                        duration: stream.duration(),
                        frames: stream.frames(),
                        avg_frame_rate: stream.avg_frame_rate(),
                        rate: stream.rate(),
                    });
                }
                media::Type::Audio if audio_selection.is_none() => {
                    audio_selection = Some(StreamSelection {
                        index: stream.index(),
                        time_base: stream.time_base(),
                        parameters,
                        duration: stream.duration(),
                        frames: stream.frames(),
                        avg_frame_rate: stream.avg_frame_rate(),
                        rate: stream.rate(),
                    });
                }
                _ => {}
            }
        }
        let Some(video) = video_selection else {
            return Err(VideoError::Unsupported(format!(
                "{label} has no video stream readable by FFmpeg"
            )));
        };
        let video_decoder = open_video_decoder(&video, &label)?;
        let width = video_decoder.width();
        let height = video_decoder.height();
        let fps = fps_of(&video);
        let duration_ms = duration_ms_of(&input, &video);
        let frame_count = if video.frames > 0 {
            video.frames
        } else {
            (duration_ms as f64 * fps / 1000.0).round() as i64
        };

        let has_audio = audio_selection.is_some();
        let mut audio = None;
        let mut audio_spec = None;
        if let Some(selection) = audio_selection {
            // A movie whose soundtrack FFmpeg cannot decode still plays its
            // video: the missing audio is reported by `audio_spec` staying
            // `None`, exactly like an unreadable format description on macOS.
            if let Ok((decoder, spec)) = open_audio_decoder(&selection, &label) {
                audio_spec = Some(spec);
                audio = Some(AudioStream {
                    index: selection.index,
                    time_base: selection.time_base,
                    decoder,
                    spec,
                    resampler: None,
                    draining: false,
                    finished: false,
                    skip_to_ms: None,
                    last_pts_ms: 0,
                });
            }
        }

        Ok(Self {
            demuxer: Demuxer::new(input, video.index),
            video: VideoStream {
                index: video.index,
                time_base: video.time_base,
                decoder: video_decoder,
                scaler: None,
                scaled: frame::Video::empty(),
                draining: false,
                finished: false,
                skip_to_ms: None,
                last_pts_ms: 0,
            },
            audio,
            metadata: VideoMetadata {
                width,
                height,
                fps,
                frame_count,
                duration_ms,
                has_audio,
            },
            audio_spec,
        })
    }
}

/// One opened stream, cloned out of the demuxer so the decoder can outlive the
/// borrow of `input.streams()`.
struct StreamSelection {
    index: usize,
    time_base: Rational,
    parameters: codec::Parameters,
    duration: i64,
    frames: i64,
    avg_frame_rate: Rational,
    rate: Rational,
}

/// Demuxer with one packet queue per stream.
///
/// The engine pulls video and audio from the same decode thread, one item at a
/// time; a pull for one stream must not discard the other stream's packets, so
/// every demuxed packet is routed to its queue and consumed in stream order.
struct Demuxer {
    input: format::context::Input,
    /// Set once `av_read_frame` reports end of stream; cleared by a seek.
    eof: bool,
    /// Stream index of the video stream, used to route packets to their queue.
    video_index: usize,
    video_packets: VecDeque<Packet>,
    audio_packets: VecDeque<Packet>,
}

impl Demuxer {
    fn new(input: format::context::Input, video_index: usize) -> Self {
        Self {
            input,
            eof: false,
            video_index,
            video_packets: VecDeque::new(),
            audio_packets: VecDeque::new(),
        }
    }

    /// Next packet of `stream_index`, buffering the packets of the other
    /// stream for its own pull. `Ok(None)` means this stream has no packet
    /// left in the demuxer or in its queue.
    fn take_packet(&mut self, stream_index: usize) -> Result<Option<Packet>, VideoError> {
        let buffered = if stream_index == self.video_index {
            self.video_packets.pop_front()
        } else {
            self.audio_packets.pop_front()
        };
        if let Some(packet) = buffered {
            return Ok(Some(packet));
        }
        if self.eof {
            // Queued packets of this stream were consumed already; the other
            // stream may still be draining its own queue.
            return Ok(None);
        }
        loop {
            let mut packet = Packet::empty();
            match packet.read(&mut self.input) {
                Ok(()) => {
                    if packet.stream() == stream_index {
                        return Ok(Some(packet));
                    }
                    if packet.stream() == self.video_index {
                        self.video_packets.push_back(packet);
                    } else {
                        self.audio_packets.push_back(packet);
                    }
                }
                Err(FfError::Eof) => {
                    self.eof = true;
                    return Ok(None);
                }
                // A demuxer can resync past a single corrupt packet.
                Err(FfError::InvalidData) => continue,
                Err(error) => {
                    return Err(VideoError::Decode(format!("demuxer read failed: {error}")));
                }
            }
        }
    }

    /// Restarts demuxing at the keyframe at or before `ms`.
    fn seek(&mut self, ms: i64) -> Result<(), VideoError> {
        let target_us = i128::from(ms)
            .saturating_mul(1000)
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
        self.input
            .seek(target_us, ..)
            .map_err(|error| VideoError::Decode(format!("ffmpeg seek failed: {error}")))?;
        self.eof = false;
        self.video_packets.clear();
        self.audio_packets.clear();
        Ok(())
    }
}

fn open_input(source: VideoSource) -> Result<(format::context::Input, String), VideoError> {
    match source {
        VideoSource::Path(path) => {
            let label = path.display().to_string();
            let input = format::input(&path)
                .map_err(|error| VideoError::Open(format!("{label}: {error}")))?;
            Ok((input, label))
        }
        VideoSource::Bytes { data, name } => {
            let label = name.clone().unwrap_or_else(|| "<memory video>".to_string());
            let reader = MemoryReader::new(data);
            let io = format::context::StreamIo::from_read_seek(reader)
                .map_err(|error| VideoError::Open(format!("{label}: {error}")))?;
            let input = format::input_from_stream(io, name.as_deref(), None)
                .map_err(|error| VideoError::Open(format!("{label}: {error}")))?;
            Ok((input, label))
        }
    }
}

fn open_video_decoder(
    selection: &StreamSelection,
    label: &str,
) -> Result<codec::decoder::Video, VideoError> {
    let mut decoder = codec::context::Context::from_parameters(selection.parameters.clone())
        .map_err(|error| VideoError::Open(format!("{label}: {error}")))?
        .decoder();
    decoder.set_packet_time_base(selection.time_base);
    let opened = decoder
        .open_as(selection.parameters.id())
        .map_err(|error| match error {
            FfError::DecoderNotFound => VideoError::Unsupported(format!(
                "{label} has no FFmpeg decoder for its video stream"
            )),
            error => VideoError::Open(format!("{label}: cannot open the video decoder: {error}")),
        })?;
    opened
        .video()
        .map_err(|error| VideoError::Open(format!("{label}: not a video stream: {error}")))
}

fn open_audio_decoder(
    selection: &StreamSelection,
    label: &str,
) -> Result<(codec::decoder::Audio, AudioSpec), VideoError> {
    let mut decoder = codec::context::Context::from_parameters(selection.parameters.clone())
        .map_err(|error| VideoError::Open(format!("{label}: {error}")))?
        .decoder();
    decoder.set_packet_time_base(selection.time_base);
    let opened = decoder
        .open_as(selection.parameters.id())
        .map_err(|error| {
            VideoError::Open(format!("{label}: cannot open the audio decoder: {error}"))
        })?;
    let audio = opened
        .audio()
        .map_err(|error| VideoError::Open(format!("{label}: not an audio stream: {error}")))?;
    let spec = AudioSpec {
        sample_rate: audio.rate(),
        channels: u32::from(audio.channels()),
    };
    if spec.sample_rate == 0 || spec.channels == 0 {
        return Err(VideoError::Unsupported(format!(
            "{label}: FFmpeg reports no audio format for the soundtrack"
        )));
    }
    Ok((audio, spec))
}

fn fps_of(selection: &StreamSelection) -> f64 {
    for rate in [selection.avg_frame_rate, selection.rate] {
        if rate.numerator() > 0 && rate.denominator() > 0 {
            return f64::from(rate.numerator()) / f64::from(rate.denominator());
        }
    }
    0.0
}

fn duration_ms_of(input: &format::context::Input, video: &StreamSelection) -> i64 {
    // `AVFormatContext::duration` is in AV_TIME_BASE units (microseconds).
    let container_us = input.duration();
    if container_us > 0 {
        return (container_us + 500) / 1000;
    }
    if video.duration > 0 {
        return rescale_ms(video.duration, video.time_base).max(0);
    }
    0
}

/// Converts `ticks` in `time_base` to milliseconds, rounding to nearest.
fn rescale_ms(ticks: i64, time_base: Rational) -> i64 {
    let numerator = i128::from(time_base.numerator());
    let denominator = i128::from(time_base.denominator()).max(1);
    let scaled = i128::from(ticks) * numerator * 1000;
    let rounded = if scaled >= 0 {
        (scaled + denominator / 2) / denominator
    } else {
        (scaled - denominator / 2) / denominator
    };
    rounded.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

struct VideoStream {
    index: usize,
    time_base: Rational,
    decoder: codec::decoder::Video,
    scaler: Option<software::scaling::Context>,
    scaled: frame::Video,
    draining: bool,
    finished: bool,
    skip_to_ms: Option<i64>,
    last_pts_ms: i64,
}

impl VideoStream {
    fn next_frame(&mut self, demuxer: &mut Demuxer) -> Result<Option<VideoFrame>, VideoError> {
        if self.finished {
            return Ok(None);
        }
        let mut raw = frame::Video::empty();
        loop {
            match self.decoder.receive_frame(&mut raw) {
                Ok(()) => {
                    let pts_ms = self.frame_pts_ms(&raw);
                    if let Some(minimum) = self.skip_to_ms {
                        if pts_ms < minimum {
                            continue;
                        }
                        self.skip_to_ms = None;
                    }
                    return self.convert(&raw, pts_ms).map(Some);
                }
                Err(FfError::Eof) => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(FfError::Other { errno }) if errno == ffi::EAGAIN => {}
                Err(error) => {
                    return Err(VideoError::Decode(format!("video decoder failed: {error}")));
                }
            }
            match demuxer.take_packet(self.index)? {
                Some(packet) => {
                    self.decoder.send_packet(&packet).map_err(|error| {
                        VideoError::Decode(format!("video decoder rejected a packet: {error}"))
                    })?;
                }
                None if !self.draining => {
                    self.draining = true;
                    self.decoder.send_eof().map_err(|error| {
                        VideoError::Decode(format!("video decoder drain failed: {error}"))
                    })?;
                }
                None => {
                    // The decoder asked for more input after being drained.
                    self.finished = true;
                    return Ok(None);
                }
            }
        }
    }

    fn frame_pts_ms(&mut self, raw: &frame::Video) -> i64 {
        let timestamp = raw.timestamp().or_else(|| raw.pts());
        match timestamp {
            Some(ticks) => {
                let ms = rescale_ms(ticks, self.time_base);
                self.last_pts_ms = ms;
                ms
            }
            // Some codecs leave both fields unset; keep the stream monotone.
            None => self.last_pts_ms,
        }
    }

    fn convert(&mut self, raw: &frame::Video, pts_ms: i64) -> Result<VideoFrame, VideoError> {
        let width = raw.width();
        let height = raw.height();
        if width == 0 || height == 0 {
            return Err(VideoError::Decode(
                "decoded video frame has no size".to_string(),
            ));
        }
        let rebuild = match &self.scaler {
            Some(scaler) => {
                *scaler.input() != scaling_definition(raw.format(), width, height)
                    || scaler.output().format != Pixel::RGBA
                    || scaler.output().width != width
                    || scaler.output().height != height
            }
            None => true,
        };
        if rebuild {
            self.scaler = Some(
                software::scaling::Context::get(
                    raw.format(),
                    width,
                    height,
                    Pixel::RGBA,
                    width,
                    height,
                    software::scaling::Flags::BILINEAR,
                )
                .map_err(|error| VideoError::Decode(format!("swscale setup failed: {error}")))?,
            );
        }
        let scaler = self
            .scaler
            .as_mut()
            .expect("scaler created when it is missing");
        scaler
            .run(raw, &mut self.scaled)
            .map_err(|error| VideoError::Decode(format!("swscale failed: {error}")))?;

        let stride = self.scaled.stride(0);
        let row_bytes = width as usize * 4;
        let rows = height as usize;
        if stride < row_bytes {
            return Err(VideoError::Decode(format!(
                "scaled frame stride {stride} is narrower than {row_bytes}"
            )));
        }
        let plane = self.scaled.data(0);
        if plane.len() < stride * rows {
            return Err(VideoError::Decode(format!(
                "scaled frame carries {} bytes for {rows} rows of {stride}",
                plane.len()
            )));
        }
        // The renderer expects tightly packed RGBA rows.
        let mut data = Vec::with_capacity(row_bytes * rows);
        for row in 0..rows {
            let start = row * stride;
            data.extend_from_slice(&plane[start..start + row_bytes]);
        }
        Ok(VideoFrame {
            pts_ms,
            width,
            height,
            stride: row_bytes as u32,
            data,
        })
    }

    fn restart_after_seek(&mut self, ms: i64) {
        self.decoder.flush();
        self.draining = false;
        self.finished = false;
        self.skip_to_ms = Some(ms);
    }
}

struct AudioStream {
    index: usize,
    time_base: Rational,
    decoder: codec::decoder::Audio,
    spec: AudioSpec,
    resampler: Option<Resampler>,
    draining: bool,
    finished: bool,
    skip_to_ms: Option<i64>,
    last_pts_ms: i64,
}

struct Resampler {
    context: software::resampling::Context,
    input_format: Sample,
    input_layout: ChannelLayout,
    input_rate: u32,
}

impl AudioStream {
    fn next_audio_chunk(
        &mut self,
        demuxer: &mut Demuxer,
    ) -> Result<Option<AudioChunk>, VideoError> {
        if self.finished {
            return Ok(None);
        }
        let mut raw = frame::Audio::empty();
        loop {
            match self.decoder.receive_frame(&mut raw) {
                Ok(()) => {
                    let pts_ms = self.frame_pts_ms(&raw);
                    if let Some(minimum) = self.skip_to_ms {
                        if pts_ms < minimum {
                            continue;
                        }
                        self.skip_to_ms = None;
                    }
                    let samples = self.convert(&raw)?;
                    if samples.is_empty() {
                        continue;
                    }
                    return Ok(Some(AudioChunk { pts_ms, samples }));
                }
                Err(FfError::Eof) => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(FfError::Other { errno }) if errno == ffi::EAGAIN => {}
                Err(error) => {
                    return Err(VideoError::Decode(format!("audio decoder failed: {error}")));
                }
            }
            match demuxer.take_packet(self.index)? {
                Some(packet) => {
                    self.decoder.send_packet(&packet).map_err(|error| {
                        VideoError::Decode(format!("audio decoder rejected a packet: {error}"))
                    })?;
                }
                None if !self.draining => {
                    self.draining = true;
                    self.decoder.send_eof().map_err(|error| {
                        VideoError::Decode(format!("audio decoder drain failed: {error}"))
                    })?;
                }
                None => {
                    self.finished = true;
                    return Ok(None);
                }
            }
        }
    }

    fn frame_pts_ms(&mut self, raw: &frame::Audio) -> i64 {
        let timestamp = raw.timestamp().or_else(|| raw.pts());
        match timestamp {
            Some(ticks) => {
                let ms = rescale_ms(ticks, self.time_base);
                self.last_pts_ms = ms;
                ms
            }
            None => self.last_pts_ms,
        }
    }

    /// Converts one decoded frame to the interleaved f32 layout the engine
    /// streams, at the sample rate and channel count `AudioSpec` advertises.
    fn convert(&mut self, raw: &frame::Audio) -> Result<Vec<f32>, VideoError> {
        if raw.samples() == 0 {
            return Ok(Vec::new());
        }
        let format = raw.format();
        let layout = raw.channel_layout();
        let rate = raw.rate();
        let rebuild = match &self.resampler {
            Some(resampler) => {
                resampler.input_format != format
                    || resampler.input_layout != layout
                    || resampler.input_rate != rate
            }
            None => true,
        };
        if rebuild {
            let channels = self.spec.channels;
            // An unspecified layout carries no channel order; swresample wants
            // a concrete one, and the engine expects `spec.channels` voices.
            let concrete = concrete_layout(layout, channels);
            let context = software::resampling::Context::get(
                format,
                concrete,
                rate,
                Sample::F32(SampleType::Packed),
                concrete,
                self.spec.sample_rate,
            )
            .map_err(|error| VideoError::Decode(format!("swresample setup failed: {error}")))?;
            self.resampler = Some(Resampler {
                context,
                input_format: format,
                // The decoded layout, not the concrete substitute: the rebuild
                // check compares against the frame, so recording anything else
                // would rebuild the resampler on every frame of a stream whose
                // layout stays unspecified.
                input_layout: layout,
                input_rate: rate,
            });
        }
        let resampler = self
            .resampler
            .as_mut()
            .expect("resampler created when it is missing");
        let mut converted = frame::Audio::empty();
        resampler
            .context
            .run(raw, &mut converted)
            .map_err(|error| VideoError::Decode(format!("swresample failed: {error}")))?;
        Ok(converted.plane::<f32>(0).to_vec())
    }

    fn restart_after_seek(&mut self, ms: i64) {
        self.decoder.flush();
        self.draining = false;
        self.finished = false;
        self.skip_to_ms = Some(ms);
    }
}

fn concrete_layout(layout: ChannelLayout, channels: u32) -> ChannelLayout {
    let channels = channels.max(1) as i32;
    if layout.channels() == channels && layout.bits() != 0 {
        layout
    } else {
        ChannelLayout::default(channels)
    }
}

fn scaling_definition(
    format: Pixel,
    width: u32,
    height: u32,
) -> software::scaling::context::Definition {
    software::scaling::context::Definition {
        format,
        width,
        height,
    }
}

/// `Read + Seek` over the host's own bytes, used as FFmpeg's AVIO source so
/// in-memory storage never hits the filesystem.
struct MemoryReader {
    data: Arc<[u8]>,
    position: u64,
}

impl MemoryReader {
    fn new(data: Arc<[u8]>) -> Self {
        Self { data, position: 0 }
    }
}

impl Read for MemoryReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(self.position)
            .unwrap_or(usize::MAX)
            .min(self.data.len());
        let available = &self.data[start..];
        let length = available.len().min(buffer.len());
        buffer[..length].copy_from_slice(&available[..length]);
        self.position += length as u64;
        Ok(length)
    }
}

impl Seek for MemoryReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let length = self.data.len() as i128;
        let current = i128::from(self.position);
        let target = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => length + i128::from(offset),
            SeekFrom::Current(offset) => current + i128::from(offset),
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the video",
            ));
        }
        // Seeking past the end is legal: the next read reports end of stream.
        self.position = u64::try_from(target).unwrap_or(u64::MAX);
        Ok(self.position)
    }
}

// The engine's DecodeSession moves the boxed decoder onto its dedicated decode
// thread and only touches it there. `ffmpeg-next` already marks the demuxer,
// decoder contexts and resampler `Send`, but `software::scaling::Context`
// holds a raw `SwsContext`, so the wrapper asserts the same thread-affine use
// the AVFoundation backend does.
unsafe impl Send for FfmpegDecoder {}

impl VideoDecoder for FfmpegDecoder {
    fn metadata(&self) -> &VideoMetadata {
        &self.metadata
    }

    fn next_frame(&mut self) -> Result<Option<VideoFrame>, VideoError> {
        self.video.next_frame(&mut self.demuxer)
    }

    fn seek_ms(&mut self, ms: i64) -> Result<(), VideoError> {
        let ms = ms.max(0);
        self.demuxer.seek(ms)?;
        self.video.restart_after_seek(ms);
        if let Some(audio) = &mut self.audio {
            audio.restart_after_seek(ms);
        }
        Ok(())
    }

    fn audio_spec(&self) -> Option<AudioSpec> {
        self.audio_spec
    }

    fn next_audio_chunk(&mut self) -> Result<Option<AudioChunk>, VideoError> {
        match &mut self.audio {
            Some(audio) => audio.next_audio_chunk(&mut self.demuxer),
            None => Ok(None),
        }
    }
}

impl VideoPort for FfmpegDecoder {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescale_ms_rounds_to_nearest_millisecond() {
        let time_base = Rational::new(1, 30);
        assert_eq!(rescale_ms(0, time_base), 0);
        assert_eq!(rescale_ms(30, time_base), 1000);
        // 1/30 s = 33.33 ms
        assert_eq!(rescale_ms(1, time_base), 33);
        // 2/30 s = 66.67 ms
        assert_eq!(rescale_ms(2, time_base), 67);
        assert_eq!(rescale_ms(-1, Rational::new(1, 10)), -100);
    }

    #[test]
    fn memory_reader_reads_and_seeks_like_a_file() {
        let mut reader = MemoryReader::new(Arc::from([1u8, 2, 3, 4, 5].as_slice()));
        let mut buffer = [0u8; 2];
        assert_eq!(reader.read(&mut buffer).unwrap(), 2);
        assert_eq!(buffer, [1, 2]);
        reader.seek(SeekFrom::Current(1)).unwrap();
        assert_eq!(reader.read(&mut buffer).unwrap(), 2);
        assert_eq!(buffer, [4, 5]);
        assert_eq!(reader.read(&mut buffer).unwrap(), 0);
        reader.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(reader.read(&mut buffer).unwrap(), 2);
        assert_eq!(buffer, [1, 2]);
        reader.seek(SeekFrom::End(-1)).unwrap();
        assert_eq!(reader.read(&mut buffer).unwrap(), 1);
        assert_eq!(buffer[0], 5);
        assert!(reader.seek(SeekFrom::Start(u64::MAX)).is_ok());
        assert_eq!(reader.read(&mut buffer).unwrap(), 0);
        reader.seek(SeekFrom::Start(0)).unwrap();
        assert!(reader.seek(SeekFrom::Current(-10)).is_err());
        reader.seek(SeekFrom::End(0)).unwrap();
        assert!(reader.seek(SeekFrom::Current(10)).is_ok());
        assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    }

    #[test]
    fn concrete_layout_keeps_known_layouts_and_falls_back_otherwise() {
        let stereo = ChannelLayout::default(2);
        assert_eq!(concrete_layout(stereo, 2), stereo);
        // A two-channel request cannot use a mono layout.
        assert_eq!(concrete_layout(ChannelLayout::default(1), 2), stereo);
    }
}
