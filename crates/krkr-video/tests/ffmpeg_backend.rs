//! Round-trips the FFmpeg fallback backend through the [`krkr_video::VideoPort`]
//! contract on the committed sample clip.
//!
//! The clip is generated with a system FFmpeg (so the repository carries no
//! encoder dependency):
//!
//! ```text
//! ffmpeg -y -f lavfi -i testsrc2=size=320x240:rate=10:duration=2 \
//!   -f lavfi -i sine=frequency=440:sample_rate=44100:duration=2 \
//!   -c:v libx264 -preset veryfast -crf 30 -pix_fmt yuv420p -g 10 \
//!   -c:a aac -ac 2 -b:a 64k -shortest sample_clip.mp4
//! ```
//!
//! The suite needs the `ffmpeg` feature and system FFmpeg on the host. It is
//! skipped where the macOS system decoder wins the selection, because then
//! `create_decoder` opens AVFoundation and this backend is not the one under
//! test.

#![cfg(all(
    feature = "ffmpeg",
    not(all(target_os = "macos", feature = "macos-avfoundation"))
))]

use std::path::PathBuf;

use krkr_video::{
    AudioSpec, VideoBackendKind, VideoDecoder, VideoFrame, VideoPort, VideoSource, create_decoder,
    platform_capabilities,
};

/// 2 s at 10 fps.
const SAMPLE_DURATION_MS: i64 = 2000;
const SAMPLE_FRAMES: usize = 20;

/// The clip also serves as the `krkr-debug` fixture project root
/// (`tests/fixtures/movie_project`), which is why it lives there.
fn sample_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/movie_project/sample_clip.mp4")
}

/// Opens the clip the way the engine does: host-owned bytes with a name hint.
fn open_from_bytes() -> Box<dyn VideoPort> {
    let data = std::fs::read(sample_path()).expect("the sample clip is committed");
    create_decoder(VideoSource::bytes(data, Some("sample_clip.mp4")))
        .expect("the FFmpeg backend opens the sample clip")
}

fn drain_frames(decoder: &mut (impl VideoDecoder + ?Sized)) -> Vec<VideoFrame> {
    let mut frames = Vec::new();
    loop {
        match decoder.next_frame() {
            Ok(Some(frame)) => frames.push(frame),
            Ok(None) => break,
            Err(error) => panic!("decoding failed: {error}"),
        }
    }
    frames
}

fn audio_spec(decoder: &(impl VideoDecoder + ?Sized)) -> AudioSpec {
    decoder.audio_spec().expect("the clip has a soundtrack")
}

#[test]
fn selection_picks_ffmpeg_where_no_system_decoder_is_compiled_in() {
    assert_eq!(
        platform_capabilities().backend,
        VideoBackendKind::Ffmpeg,
        "this build is outside the macOS AVFoundation branch"
    );
    let decoder = open_from_bytes();
    let capabilities = decoder.capabilities();
    assert_eq!(capabilities.backend, VideoBackendKind::Ffmpeg);
    assert!(capabilities.in_memory, "the FFmpeg backend reads bytes");
    assert!(capabilities.soundtrack_pcm);
}

#[test]
fn metadata_describes_the_clip() {
    let decoder = open_from_bytes();
    let metadata = decoder.metadata();
    assert_eq!(metadata.width, 320);
    assert_eq!(metadata.height, 240);
    assert!(
        (metadata.fps - 10.0).abs() < 0.01,
        "fps was {}",
        metadata.fps
    );
    assert!(metadata.has_audio);
    assert!(
        (metadata.duration_ms - SAMPLE_DURATION_MS).abs() <= 50,
        "duration was {} ms",
        metadata.duration_ms
    );
    assert!(
        (metadata.frame_count - SAMPLE_FRAMES as i64).abs() <= 1,
        "frame count was {}",
        metadata.frame_count
    );
}

#[test]
fn frames_are_rgba_with_monotone_pts_and_sticky_eof() {
    let mut decoder = open_from_bytes();
    let frames = drain_frames(decoder.as_mut());
    assert_eq!(frames.len(), SAMPLE_FRAMES);

    let mut previous_pts = i64::MIN;
    for frame in &frames {
        assert_eq!(frame.width, 320);
        assert_eq!(frame.height, 240);
        assert_eq!(frame.stride, 320 * 4);
        assert_eq!(frame.data.len(), 320 * 240 * 4);
        assert!(
            frame.pts_ms >= previous_pts,
            "pts went backwards: {} after {previous_pts}",
            frame.pts_ms
        );
        previous_pts = frame.pts_ms;
        assert!(frame.pts_ms >= 0 && frame.pts_ms <= SAMPLE_DURATION_MS);
        // swscale fills alpha for a stream without one.
        let (pixels, remainder) = frame.data.as_chunks::<4>();
        assert!(remainder.is_empty());
        assert!(
            pixels.iter().all(|pixel| pixel[3] == 255),
            "frame at {} ms has non-opaque pixels",
            frame.pts_ms
        );
        assert!(
            pixels
                .iter()
                .any(|pixel| pixel[0] | pixel[1] | pixel[2] != 0),
            "frame at {} ms is fully black",
            frame.pts_ms
        );
    }
    assert!(
        frames[0].pts_ms < 100,
        "first frame pts {}",
        frames[0].pts_ms
    );

    // End of stream stays sticky until a seek, like a parked decode session.
    assert!(matches!(decoder.next_frame(), Ok(None)));
    assert!(matches!(decoder.next_frame(), Ok(None)));
}

#[test]
fn soundtrack_matches_the_advertised_spec_until_eof() {
    let mut decoder = open_from_bytes();
    let spec = audio_spec(decoder.as_ref());
    assert_eq!(spec.sample_rate, 44_100);
    assert_eq!(spec.channels, 2);

    let mut chunks = 0usize;
    let mut frames_per_channel = 0u64;
    let mut previous_pts = i64::MIN;
    let mut peak = 0.0f32;
    loop {
        match decoder.next_audio_chunk() {
            Ok(Some(chunk)) => {
                chunks += 1;
                assert!(
                    chunk.samples.len() % spec.channels as usize == 0,
                    "chunk {} is not interleaved: {} samples for {} channels",
                    chunks,
                    chunk.samples.len(),
                    spec.channels
                );
                assert!(
                    chunk.pts_ms >= previous_pts,
                    "audio pts went backwards: {} after {previous_pts}",
                    chunk.pts_ms
                );
                previous_pts = chunk.pts_ms;
                frames_per_channel += chunk.samples.len() as u64 / u64::from(spec.channels);
                peak = chunk
                    .samples
                    .iter()
                    .fold(peak, |peak, sample| peak.max(sample.abs()));
            }
            Ok(None) => break,
            Err(error) => panic!("decoding the soundtrack failed: {error}"),
        }
    }
    assert!(chunks > 0, "no audio chunks were produced");
    let expected = SAMPLE_DURATION_MS as u64 * u64::from(spec.sample_rate) / 1000;
    assert!(
        frames_per_channel.abs_diff(expected) <= u64::from(spec.sample_rate) / 10,
        "{frames_per_channel} frames per channel, expected about {expected}"
    );
    assert!(peak > 0.01, "the 440 Hz tone decoded as silence");
    assert!(matches!(decoder.next_audio_chunk(), Ok(None)));
}

/// The engine's decode thread pulls one frame and one audio chunk per
/// iteration; a pull for one stream must not discard the other stream's
/// packets, so both must still decode the whole clip when interleaved.
#[test]
fn interleaved_video_and_audio_pulls_keep_every_stream() {
    let mut decoder = open_from_bytes();
    let spec = audio_spec(decoder.as_ref());
    let mut video_frames = 0usize;
    let mut audio_frames = 0u64;
    loop {
        let frame = decoder.next_frame().expect("video");
        let chunk = decoder.next_audio_chunk().expect("audio");
        if frame.is_none() && chunk.is_none() {
            break;
        }
        if frame.is_some() {
            video_frames += 1;
        }
        if let Some(chunk) = chunk {
            audio_frames += chunk.samples.len() as u64 / u64::from(spec.channels);
        }
    }
    assert_eq!(video_frames, SAMPLE_FRAMES);
    let expected = SAMPLE_DURATION_MS as u64 * u64::from(spec.sample_rate) / 1000;
    assert!(
        audio_frames.abs_diff(expected) <= u64::from(spec.sample_rate) / 10,
        "{audio_frames} frames per channel, expected about {expected}"
    );
}

#[test]
fn seek_lands_on_the_target_and_revives_a_finished_stream() {
    let mut decoder = open_from_bytes();
    // Prime the decoders, then seek forward mid-clip.
    for _ in 0..3 {
        assert!(matches!(decoder.next_frame(), Ok(Some(_))));
    }
    decoder.seek_ms(1200).expect("seek forward");

    let frame = decoder.next_frame().expect("seek decode").expect("a frame");
    assert!(
        (1200..1400).contains(&frame.pts_ms),
        "first frame after the seek is at {} ms",
        frame.pts_ms
    );
    let chunk = decoder
        .next_audio_chunk()
        .expect("seek audio decode")
        .expect("an audio chunk");
    assert!(
        (1200..1400).contains(&chunk.pts_ms),
        "first audio chunk after the seek is at {} ms",
        chunk.pts_ms
    );

    // Run to the end, then seek back: end of stream is not terminal.
    let tail = drain_frames(decoder.as_mut());
    assert!(!tail.is_empty());
    assert!(tail.last().expect("tail").pts_ms > 1200);
    decoder.seek_ms(0).expect("seek back to the start");
    let frame = decoder.next_frame().expect("re-decode").expect("a frame");
    assert!(
        frame.pts_ms < 100,
        "rewound frame is at {} ms",
        frame.pts_ms
    );
    let frames = drain_frames(decoder.as_mut());
    assert_eq!(frames.len() + 1, SAMPLE_FRAMES);
}

#[test]
fn path_and_memory_sources_decode_identically() {
    let mut from_path = create_decoder(VideoSource::path(sample_path()))
        .expect("the FFmpeg backend opens the sample clip by path");
    let mut from_bytes = open_from_bytes();
    for _ in 0..3 {
        let path_frame = from_path.next_frame().expect("decode").expect("a frame");
        let bytes_frame = from_bytes.next_frame().expect("decode").expect("a frame");
        assert_eq!(path_frame.pts_ms, bytes_frame.pts_ms);
        assert_eq!(path_frame.data, bytes_frame.data);
    }
}
