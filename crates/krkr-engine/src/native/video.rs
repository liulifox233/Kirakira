//! Native `VideoOverlay` object backed by platform video decoders.
//!
//! krkr2/krkrz play movies through the OS media framework (DirectShow /
//! Media Foundation); kirakira follows the same architecture through
//! krkr-video's pluggable `VideoPort` backends (AVFoundation on macOS).
//!
//! Decoding runs on a dedicated thread per playing overlay with a bounded
//! frame queue for back-pressure. The engine tick consumes frames against a
//! playback clock driven by the host clock (which the headless debugger can
//! scale virtually) and fires the TJS status events — `ready` / `play` /
//! `pause` / `stop` / `unload` plus `onPeriod` reasons — that KAG movie
//! conductors wait on. Event timing mirrors krkrz non-MFEVR mode: `open`
//! fires `ready` synchronously and `play` fires `play` synchronously.

use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
};

#[cfg(not(target_arch = "wasm32"))]
use krkr_core::AudioBus;
use krkr_core::{
    AudioCommand, AudioInstanceId, DrawCommand, ImageCommand, ImageUpload, PcmAudioChunk,
    PcmAudioSpec, Rect, Size,
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{Closure, ObjectHandle, Runtime, Variant},
};
#[cfg(not(target_arch = "wasm32"))]
use krkr_video::VideoSource;
use krkr_video::{VideoFrame, VideoMetadata, VideoPort};

use crate::host::KrkrHost;

use super::classes::video_overlay_property_names;

const VIDEO_STATUS_UNLOAD: &str = "unload";
const VIDEO_STATUS_READY: &str = "ready";
const VIDEO_STATUS_PLAY: &str = "play";
const VIDEO_STATUS_PAUSE: &str = "pause";
const VIDEO_STATUS_STOP: &str = "stop";

const PER_LOOP: i64 = 0;
const PER_PERIOD: i64 = 1;
const PER_SEG_LOOP: i64 = 3;

#[cfg(not(target_arch = "wasm32"))]
const VIDEO_MODE_LAYER: i64 = 1;

/// How many decoded frames may queue up before the decode thread blocks.
const DECODE_QUEUE_DEPTH: usize = 4;

/// How many decoded movie-audio chunks may queue up before the decode
/// thread blocks. AVFoundation hands out LPCM in large buffers, so a handful
/// of chunks already covers several seconds of audio.
#[cfg(not(target_arch = "wasm32"))]
const AUDIO_QUEUE_DEPTH: usize = 8;

/// Mutable per-object state for one `VideoOverlay` instance. Keyed by object
/// handle on the host and created lazily on first native interaction so the
/// exact script construction order does not matter.
pub(crate) struct VideoOverlayState {
    pub(crate) status: &'static str,
    pub(crate) storage: Option<String>,
    metadata: Option<VideoMetadata>,
    /// Opened but not yet playing; moved into the decode session on `play`.
    decoder: Option<Box<dyn VideoPort>>,
    session: Option<DecodeSession>,
    /// Soundtrack layout reported by the video backend; `play` feeds this
    /// PCM stream to the audio system instead of loading the movie file
    /// through the audio loaders.
    audio_spec: Option<PcmAudioSpec>,
    pub(crate) left: i64,
    pub(crate) top: i64,
    pub(crate) width: i64,
    pub(crate) height: i64,
    pub(crate) visible: bool,
    mode: i64,
    pub(crate) looping: bool,
    pub(crate) play_rate: f64,
    /// Playback position at the clock anchor (or the frozen position while
    /// paused / stopped).
    pub(crate) position_ms: i64,
    clock_anchor_ms: Option<i64>,
    audio_id: Option<AudioInstanceId>,
    pub(crate) audio_volume: i64,
    pub(crate) audio_balance: i64,
    enabled_audio_stream: i64,
    enabled_video_stream: i64,
    period_event_frame: i64,
    period_event_fired: bool,
    segment_loop: Option<(i64, i64)>,
    texture_id: Option<u64>,
    last_uploaded_pts: i64,
    current_frame: Option<VideoFrame>,
    stop_fired: bool,
    vom_layer_notice_logged: bool,
    logged_first_frame: bool,
    logged_present: bool,
    /// Whether the previous frame emitted a draw quad for this overlay. Used
    /// to log exactly once when a playing overlay stops emitting quads.
    quad_active: bool,
    layer1: Variant,
    layer2: Variant,
    /// Record-only properties kirakira does not act on (mixing Movie
    /// parameters, color adjustment ranges, ...). Stored so scripts read back
    /// what they wrote, like krkrz's pass-through members.
    extra: BTreeMap<String, Variant>,
}

impl Default for VideoOverlayState {
    fn default() -> Self {
        Self {
            status: VIDEO_STATUS_UNLOAD,
            storage: None,
            metadata: None,
            decoder: None,
            session: None,
            audio_spec: None,
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            visible: false,
            mode: 0,
            looping: false,
            play_rate: 1.0,
            position_ms: 0,
            clock_anchor_ms: None,
            audio_id: None,
            audio_volume: 100000,
            audio_balance: 0,
            enabled_audio_stream: 0,
            enabled_video_stream: 0,
            period_event_frame: -1,
            period_event_fired: false,
            segment_loop: None,
            texture_id: None,
            last_uploaded_pts: -1,
            current_frame: None,
            stop_fired: false,
            vom_layer_notice_logged: false,
            logged_first_frame: false,
            logged_present: false,
            quad_active: false,
            layer1: Variant::Void,
            layer2: Variant::Void,
            extra: BTreeMap::new(),
        }
    }
}

impl Clone for VideoOverlayState {
    fn clone(&self) -> Self {
        // Decode threads and open decoders cannot be duplicated; a cloned
        // host keeps the overlay's static configuration but drops playback.
        Self {
            status: VIDEO_STATUS_UNLOAD,
            storage: None,
            metadata: None,
            decoder: None,
            session: None,
            audio_spec: None,
            left: self.left,
            top: self.top,
            width: self.width,
            height: self.height,
            visible: self.visible,
            mode: self.mode,
            looping: self.looping,
            play_rate: self.play_rate,
            position_ms: 0,
            clock_anchor_ms: None,
            audio_id: None,
            audio_volume: self.audio_volume,
            audio_balance: self.audio_balance,
            enabled_audio_stream: self.enabled_audio_stream,
            enabled_video_stream: self.enabled_video_stream,
            period_event_frame: self.period_event_frame,
            period_event_fired: false,
            segment_loop: self.segment_loop,
            texture_id: None,
            last_uploaded_pts: -1,
            current_frame: None,
            stop_fired: false,
            vom_layer_notice_logged: self.vom_layer_notice_logged,
            logged_first_frame: false,
            logged_present: false,
            quad_active: false,
            layer1: self.layer1.clone(),
            layer2: self.layer2.clone(),
            extra: self.extra.clone(),
        }
    }
}

impl VideoOverlayState {
    pub(crate) fn elapsed_ms(&self, now_ms: i64) -> i64 {
        match self.clock_anchor_ms {
            Some(anchor) => {
                self.position_ms + ((now_ms - anchor) as f64 * self.play_rate.max(0.0)) as i64
            }
            None => self.position_ms,
        }
    }

    fn fps(&self) -> f64 {
        self.metadata.as_ref().map(|meta| meta.fps).unwrap_or(0.0)
    }

    fn frame_number(&self, now_ms: i64) -> i64 {
        let fps = self.fps();
        if fps <= 0.0 {
            return 0;
        }
        (self.elapsed_ms(now_ms) as f64 * fps / 1000.0) as i64
    }

    fn duration_ms(&self) -> i64 {
        self.metadata
            .as_ref()
            .map(|meta| meta.duration_ms)
            .unwrap_or(0)
    }
}

enum DecodeMessage {
    Frame(VideoFrame),
    Error(String),
}

enum DecodeCommand {
    Seek(i64),
}

/// Owns the decode thread. Frames flow back through a bounded channel; the
/// engine drains it every tick, which also unblocks the thread after seeks.
/// Movie soundtracks flow through a second channel (`audio_tx`) that the
/// audio system drains in real time; both channels are pumped from the same
/// loop so AVFoundation never stalls on an unread output.
struct DecodeSession {
    command_tx: Option<SyncSender<DecodeCommand>>,
    rx: Option<Receiver<DecodeMessage>>,
    join: Option<JoinHandle<()>>,
    latest_error: Option<String>,
    /// Decoded-ahead frames in ascending pts order. The decode thread usually
    /// runs ahead of the presentation clock; frames not yet due must be kept
    /// for later ticks instead of being dropped (otherwise playback freezes
    /// on the first frame whenever decoding outruns the tick loop).
    pending: VecDeque<VideoFrame>,
}

impl DecodeSession {
    fn start(
        mut decoder: Box<dyn VideoPort>,
        audio_tx: Option<SyncSender<PcmAudioChunk>>,
    ) -> std::io::Result<Self> {
        let (command_tx, command_rx) = mpsc::sync_channel::<DecodeCommand>(DECODE_QUEUE_DEPTH);
        let (frame_tx, frame_rx) = mpsc::sync_channel::<DecodeMessage>(DECODE_QUEUE_DEPTH);
        let join = std::thread::Builder::new()
            .name("krkr-video-decode".to_string())
            .spawn(move || {
                let mut held_frame: Option<VideoFrame> = None;
                let mut held_audio: Option<PcmAudioChunk> = None;
                let mut video_eof = false;
                let mut audio_eof = audio_tx.is_none();
                'decode: loop {
                    // Apply pending seeks between items; only the latest seek
                    // target matters. Buffered pre-seek media is dropped.
                    let mut seek = None;
                    while let Ok(command) = command_rx.try_recv() {
                        match command {
                            DecodeCommand::Seek(ms) => seek = Some(ms),
                        }
                    }
                    if let Some(ms) = seek {
                        held_frame = None;
                        held_audio = None;
                        video_eof = false;
                        audio_eof = audio_tx.is_none();
                        if decoder.seek_ms(ms).is_err() {
                            let _ = frame_tx.send(DecodeMessage::Error("seek failed".to_string()));
                            break;
                        }
                    }
                    if !video_eof && held_frame.is_none() {
                        match decoder.next_frame() {
                            Ok(Some(frame)) => held_frame = Some(frame),
                            Ok(None) => video_eof = true,
                            Err(error) => {
                                let _ = frame_tx.send(DecodeMessage::Error(error.to_string()));
                                break;
                            }
                        }
                    }
                    if !audio_eof && held_audio.is_none() {
                        match decoder.next_audio_chunk() {
                            Ok(Some(chunk)) => {
                                held_audio = Some(PcmAudioChunk {
                                    pts_ms: chunk.pts_ms,
                                    samples: Arc::from(chunk.samples.as_slice()),
                                });
                            }
                            Ok(None) => audio_eof = true,
                            Err(error) => {
                                let _ = frame_tx.send(DecodeMessage::Error(error.to_string()));
                                break;
                            }
                        }
                    }
                    if let Some(frame) = held_frame.take() {
                        match frame_tx.try_send(DecodeMessage::Frame(frame)) {
                            Ok(()) => {}
                            Err(mpsc::TrySendError::Full(message)) => {
                                let DecodeMessage::Frame(frame) = message else {
                                    break;
                                };
                                held_frame = Some(frame);
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => break,
                        }
                    }
                    if let Some(chunk) = held_audio.take() {
                        match &audio_tx {
                            Some(tx) => match tx.try_send(chunk) {
                                Ok(()) => {}
                                Err(mpsc::TrySendError::Full(chunk)) => held_audio = Some(chunk),
                                // The audio consumer went away (movie stopped
                                // or audio unavailable); keep video flowing.
                                Err(mpsc::TrySendError::Disconnected(_)) => audio_eof = true,
                            },
                            None => audio_eof = true,
                        }
                    }
                    if video_eof && audio_eof {
                        // End of stream: park until a seek arrives or the
                        // session goes away (channel disconnects).
                        match command_rx.recv() {
                            Ok(DecodeCommand::Seek(ms)) => {
                                if decoder.seek_ms(ms).is_ok() {
                                    video_eof = false;
                                    audio_eof = audio_tx.is_none();
                                    continue 'decode;
                                }
                                let _ =
                                    frame_tx.send(DecodeMessage::Error("seek failed".to_string()));
                                break 'decode;
                            }
                            Err(_) => break 'decode,
                        }
                    }
                    if held_frame.is_some() || held_audio.is_some() {
                        // Both consumer queues are full; wait for them to
                        // drain instead of busy-spinning.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            })?;
        Ok(Self {
            command_tx: Some(command_tx),
            rx: Some(frame_rx),
            join: Some(join),
            latest_error: None,
            pending: VecDeque::new(),
        })
    }

    /// Drains pending messages into the lookahead buffer, then pops and
    /// returns the newest frame due at `position_ms`. Frames ahead of the
    /// clock stay buffered for later ticks. Always drains (even while paused)
    /// so the decode thread never stays blocked on a full queue.
    fn take_frame_at(&mut self, position_ms: i64) -> Option<VideoFrame> {
        const MAX_PENDING: usize = DECODE_QUEUE_DEPTH * 2;
        while self.pending.len() < MAX_PENDING {
            let Some(rx) = self.rx.as_ref() else {
                break;
            };
            match rx.try_recv() {
                Ok(DecodeMessage::Frame(frame)) => self.pending.push_back(frame),
                Ok(DecodeMessage::Error(error)) => self.latest_error = Some(error),
                Err(_) => break,
            }
        }
        let mut best = None;
        while let Some(front) = self.pending.front() {
            if front.pts_ms > position_ms {
                break;
            }
            best = self.pending.pop_front();
        }
        best
    }

    fn seek(&mut self, ms: i64) {
        self.pending.clear();
        if let Some(tx) = &self.command_tx {
            let _ = tx.try_send(DecodeCommand::Seek(ms));
        }
    }
}

impl Drop for DecodeSession {
    fn drop(&mut self) {
        // Disconnecting both channels unblocks the thread whether it is
        // parked at end-of-stream or blocked on a full frame queue.
        self.command_tx.take();
        self.rx.take();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn video_this(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    method: &str,
) -> Result<ObjectHandle> {
    let this = this_obj.ok_or_else(|| TjsError::runtime(format!("{method} requires this")))?;
    Ok(runtime.bound_this(this).unwrap_or(this))
}

fn optional_integer(args: &[Variant], index: usize) -> Result<Option<i64>> {
    args.get(index)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_integer)
        .transpose()
}

/// krkr2 leaves extension probing to each subsystem, but our generic storage
/// lookup guesses extensions and can land on a same-named *audio* file
/// (GINKA: `OP` resolves to `bgm/op.ogg`, which short-circuits the game's
/// own `isExistentStorage` probing). Probe video extensions first, in the
/// order GINKA's sysmovie handler uses, before falling back to the name as
/// given.
fn resolve_video_storage_name(host: &KrkrHost, storage: &str) -> String {
    if Path::new(storage).extension().is_some() {
        return storage.to_string();
    }
    for extension in ["mpg", "mpeg", "wmv", "mp4", "webm", "avi", "m2ts", "mov"] {
        let candidate = format!("{storage}.{extension}");
        if host.storage_exists(&candidate) {
            return candidate;
        }
    }
    storage.to_string()
}

/// Tears down any open decode state without firing events. Returns the audio
/// instance to stop, if one is playing.
fn video_teardown(state: &mut VideoOverlayState) -> Option<AudioInstanceId> {
    state.session = None;
    state.decoder = None;
    state.metadata = None;
    state.audio_spec = None;
    state.storage = None;
    state.clock_anchor_ms = None;
    state.position_ms = 0;
    state.current_frame = None;
    state.last_uploaded_pts = -1;
    state.audio_id.take()
}

fn video_open_storage(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    storage: &str,
) -> Result<()> {
    {
        let state = runtime.host_mut().video_overlay_state_mut(this);
        if state.status != VIDEO_STATUS_UNLOAD {
            let audio = video_teardown(state);
            state.status = VIDEO_STATUS_UNLOAD;
            if let Some(id) = audio {
                runtime.host_mut().queue_audio_command(AudioCommand::Stop {
                    id,
                    fade_seconds: 0.0,
                });
            }
        }
    }
    let resolved_storage = resolve_video_storage_name(runtime.host(), storage);
    let storage = resolved_storage.as_str();
    #[cfg(not(target_arch = "wasm32"))]
    let bytes = runtime.host_mut().read_binary_storage_for_tjs(storage)?;
    #[cfg(target_arch = "wasm32")]
    {
        // Browser media elements own decoding and audio output. Keep the
        // engine-side conductor state alive and let the Web shell present the
        // storage as a native `<video>` overlay (no temp files or decode
        // threads exist on wasm).
        let state = runtime.host_mut().video_overlay_state_mut(this);
        state.storage = Some(storage.to_string());
        state.metadata = None;
        state.audio_spec = None;
        state.decoder = None;
        state.status = VIDEO_STATUS_READY;
        state.position_ms = 0;
        state.clock_anchor_ms = None;
        state.stop_fired = false;
        state.period_event_fired = false;
        runtime.host_mut().log(&format!(
            "VideoOverlay.open: {storage} (browser media element)"
        ));
        return call_video_status_changed(runtime, this, VIDEO_STATUS_READY);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let decoder = match krkr_video::create_decoder(VideoSource::bytes(bytes, Some(storage))) {
            Ok(decoder) => decoder,
            Err(error) => {
                // GINKA's Movie.open rethrows only plugin-style failures whose
                // message contains ".dll"; a plain decode error takes its
                // graceful fallback path instead of deadlocking the scenario.
                return Err(TjsError::runtime(format!(
                    "VideoOverlay.open: {storage}: {error}"
                )));
            }
        };
        let metadata = decoder.metadata().clone();
        let audio_spec = decoder.audio_spec().map(|spec| PcmAudioSpec {
            sample_rate: spec.sample_rate,
            channels: spec.channels,
        });
        runtime.host_mut().log(&format!(
            "VideoOverlay.open: {storage} ({}x{}, {:.2} fps, {} ms, audio={})",
            metadata.width, metadata.height, metadata.fps, metadata.duration_ms, metadata.has_audio
        ));
        {
            let state = runtime.host_mut().video_overlay_state_mut(this);
            state.storage = Some(storage.to_string());
            state.metadata = Some(metadata);
            state.audio_spec = audio_spec;
            state.decoder = Some(decoder);
            state.status = VIDEO_STATUS_READY;
            state.position_ms = 0;
            state.clock_anchor_ms = None;
            state.stop_fired = false;
            state.period_event_fired = false;
        }
        call_video_status_changed(runtime, this, VIDEO_STATUS_READY)
    }
}

/// Makes sure the overlay holds an open decoder, transparently (re-)opening
/// the storage like krkrz's `Play(storage)` does.
fn video_ensure_open(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    storage_arg: Option<String>,
) -> Result<()> {
    let (has_decoder, stored_storage) = {
        let state = runtime.host().video_overlay_state(this);
        (
            state.is_some_and(|state| {
                state.decoder.is_some() || (cfg!(target_arch = "wasm32") && state.storage.is_some())
            }),
            state.and_then(|state| state.storage.clone()),
        )
    };
    if has_decoder {
        return Ok(());
    }
    let storage = storage_arg
        .or(stored_storage)
        .ok_or_else(|| TjsError::runtime("VideoOverlay.play requires an opened storage"))?;
    video_open_storage(runtime, this, &storage)
}

fn video_seek(runtime: &mut Runtime<KrkrHost>, this: ObjectHandle, position_ms: i64) {
    let position_ms = position_ms.max(0);
    let now = runtime.host_mut().now_millis();
    let state = runtime.host_mut().video_overlay_state_mut(this);
    state.position_ms = position_ms;
    if state.clock_anchor_ms.is_some() {
        state.clock_anchor_ms = Some(now);
    }
    if let Some(session) = &mut state.session {
        session.seek(position_ms);
    }
    state.period_event_fired = false;
}

fn video_overlay_open(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.open")?;
    let storage = args
        .first()
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    if storage.is_empty() {
        return Err(TjsError::runtime("VideoOverlay.open requires storage"));
    }
    video_open_storage(runtime, this, &storage)?;
    Ok(Variant::Void)
}

fn video_overlay_play(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.play")?;
    let storage_arg = args
        .first()
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()?
        .filter(|storage| !storage.is_empty());
    video_ensure_open(runtime, this, storage_arg)?;

    #[cfg(target_arch = "wasm32")]
    {
        let now = runtime.host_mut().now_millis();
        let state = runtime.host_mut().video_overlay_state_mut(this);
        state.position_ms = 0;
        state.clock_anchor_ms = Some(now);
        state.status = VIDEO_STATUS_PLAY;
        state.stop_fired = false;
        state.period_event_fired = false;
        return call_video_status_changed(runtime, this, VIDEO_STATUS_PLAY).map(|_| Variant::Void);
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        enum AudioAction {
            Start,
            Resume,
        }
        struct PcmStreamStart {
            spec: PcmAudioSpec,
            total_frames: u64,
            rx: Receiver<PcmAudioChunk>,
        }
        let (audio_action, audio_id, pcm_stream, audio_storage, volume, mode_notice) = {
            let now = runtime.host_mut().now_millis();
            let state = runtime.host_mut().video_overlay_state_mut(this);
            let resume = state.status == VIDEO_STATUS_PAUSE && state.session.is_some();
            let mut pcm_stream = None;
            if state.session.is_none() {
                let decoder = state
                    .decoder
                    .take()
                    .ok_or_else(|| TjsError::runtime("VideoOverlay.play: no opened storage"))?;
                let audio_tx = state.audio_spec.map(|spec| {
                    let (tx, rx) = mpsc::sync_channel::<PcmAudioChunk>(AUDIO_QUEUE_DEPTH);
                    let total_frames = state
                        .metadata
                        .as_ref()
                        .map(|meta| {
                            (meta.duration_ms.max(0) as u64)
                                .saturating_mul(u64::from(spec.sample_rate))
                                / 1000
                        })
                        .unwrap_or(0);
                    pcm_stream = Some(PcmStreamStart {
                        spec,
                        total_frames,
                        rx,
                    });
                    tx
                });
                let session = DecodeSession::start(decoder, audio_tx)
                    .map_err(|error| TjsError::runtime(format!("VideoOverlay.play: {error}")))?;
                state.session = Some(session);
            } else if !resume {
                // Play while already playing restarts from the beginning (krkrz
                // reruns the graph); rewind the decode thread with the clock.
                if let Some(session) = &mut state.session {
                    session.seek(0);
                }
            }
            if !resume {
                state.position_ms = 0;
            }
            state.clock_anchor_ms = Some(now);
            state.status = VIDEO_STATUS_PLAY;
            state.stop_fired = false;
            let has_audio = state.audio_spec.is_some();
            let action = if resume && state.audio_id.is_some() {
                Some(AudioAction::Resume)
            } else if has_audio {
                Some(AudioAction::Start)
            } else {
                None
            };
            let notice = (state.mode == VIDEO_MODE_LAYER && !state.vom_layer_notice_logged).then(|| {
            state.vom_layer_notice_logged = true;
            "VideoOverlay mode=vomLayer currently renders through the overlay quad; layer mixing is not implemented"
        });
            (
                action,
                state.audio_id,
                pcm_stream,
                state.storage.clone(),
                state.audio_volume,
                notice,
            )
        };
        if let Some(notice) = mode_notice {
            runtime.host_mut().log(notice);
        }
        if let Some(storage) = &audio_storage {
            runtime
                .host_mut()
                .log(&format!("VideoOverlay.play: {storage}"));
        }
        match audio_action {
            Some(AudioAction::Start) => {
                if let Some(start) = pcm_stream {
                    let id = runtime.host_mut().queue_pcm_stream_play(
                        AudioBus::Bgm,
                        start.spec,
                        start.total_frames,
                        start.rx,
                        volume as f32 / 100000.0,
                    );
                    runtime.host_mut().video_overlay_state_mut(this).audio_id = Some(id);
                }
            }
            Some(AudioAction::Resume) => {
                if let Some(id) = audio_id {
                    runtime
                        .host_mut()
                        .queue_audio_command(AudioCommand::Resume {
                            id,
                            fade_seconds: 0.0,
                        });
                }
            }
            None => {}
        }
        call_video_status_changed(runtime, this, VIDEO_STATUS_PLAY)?;
        Ok(Variant::Void)
    }
}

/// Shared stop/EOF teardown: drops the decode session, stops the audio track
/// and updates status. Returns the audio instance to stop.
fn video_stop_playback(state: &mut VideoOverlayState) -> Option<AudioInstanceId> {
    state.session = None;
    state.decoder = None;
    state.clock_anchor_ms = None;
    state.position_ms = 0;
    state.status = VIDEO_STATUS_STOP;
    state.stop_fired = true;
    state.audio_id.take()
}

fn video_overlay_stop(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.stop")?;
    let audio = {
        let state = runtime.host_mut().video_overlay_state_mut(this);
        if state.status == VIDEO_STATUS_UNLOAD || state.status == VIDEO_STATUS_STOP {
            None
        } else {
            video_stop_playback(state)
        }
    };
    if let Some(id) = audio {
        runtime.host_mut().queue_audio_command(AudioCommand::Stop {
            id,
            fade_seconds: 0.0,
        });
        call_video_status_changed(runtime, this, VIDEO_STATUS_STOP)?;
    } else {
        let should_fire = {
            let state = runtime.host().video_overlay_state(this);
            state.is_some_and(|state| {
                state.status != VIDEO_STATUS_UNLOAD && state.status != VIDEO_STATUS_STOP
            })
        };
        if should_fire {
            call_video_status_changed(runtime, this, VIDEO_STATUS_STOP)?;
        }
    }
    Ok(Variant::Void)
}

fn video_overlay_close(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.close")?;
    let (audio, should_fire) = {
        let state = runtime.host_mut().video_overlay_state_mut(this);
        if state.status == VIDEO_STATUS_UNLOAD {
            (None, false)
        } else {
            let audio = video_teardown(state);
            state.status = VIDEO_STATUS_UNLOAD;
            (audio, true)
        }
    };
    if let Some(id) = audio {
        runtime.host_mut().queue_audio_command(AudioCommand::Stop {
            id,
            fade_seconds: 0.0,
        });
    }
    if should_fire {
        call_video_status_changed(runtime, this, VIDEO_STATUS_UNLOAD)?;
    }
    Ok(Variant::Void)
}

fn video_overlay_set_pos(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.setPos")?;
    let position = optional_integer(&args, 0)?.unwrap_or(0);
    video_seek(runtime, this, position);
    Ok(Variant::Void)
}

fn video_overlay_rewind(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.rewind")?;
    video_seek(runtime, this, 0);
    Ok(Variant::Void)
}

fn video_overlay_pause(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.pause")?;
    let audio = {
        let now = runtime.host_mut().now_millis();
        let state = runtime.host_mut().video_overlay_state_mut(this);
        if state.status != VIDEO_STATUS_PLAY {
            None
        } else {
            state.position_ms = state.elapsed_ms(now);
            state.clock_anchor_ms = None;
            state.status = VIDEO_STATUS_PAUSE;
            state.audio_id
        }
    };
    if let Some(id) = audio {
        runtime.host_mut().queue_audio_command(AudioCommand::Pause {
            id,
            fade_seconds: 0.0,
        });
        call_video_status_changed(runtime, this, VIDEO_STATUS_PAUSE)?;
    }
    Ok(Variant::Void)
}

fn video_overlay_set_size(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.setSize")?;
    let width = optional_integer(&args, 0)?.unwrap_or(0);
    let height = optional_integer(&args, 1)?.unwrap_or(0);
    let state = runtime.host_mut().video_overlay_state_mut(this);
    state.width = width;
    state.height = height;
    Ok(Variant::Void)
}

fn video_overlay_set_bounds(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.setBounds")?;
    let left = optional_integer(&args, 0)?.unwrap_or(0);
    let top = optional_integer(&args, 1)?.unwrap_or(0);
    let width = optional_integer(&args, 2)?.unwrap_or(0);
    let height = optional_integer(&args, 3)?.unwrap_or(0);
    let state = runtime.host_mut().video_overlay_state_mut(this);
    state.left = left;
    state.top = top;
    state.width = width;
    state.height = height;
    Ok(Variant::Void)
}

fn video_overlay_set_segment_loop(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.setSegmentLoop")?;
    let start = optional_integer(&args, 0)?.unwrap_or(-1);
    let end = optional_integer(&args, 1)?.unwrap_or(-1);
    let state = runtime.host_mut().video_overlay_state_mut(this);
    state.segment_loop = (end > start && start >= 0).then_some((start, end));
    Ok(Variant::Void)
}

fn video_overlay_cancel_segment_loop(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.cancelSegmentLoop")?;
    runtime
        .host_mut()
        .video_overlay_state_mut(this)
        .segment_loop = None;
    Ok(Variant::Void)
}

fn video_overlay_set_period_event(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.setPeriodEvent")?;
    let frame = optional_integer(&args, 0)?.unwrap_or(-1);
    let state = runtime.host_mut().video_overlay_state_mut(this);
    state.period_event_frame = frame;
    state.period_event_fired = false;
    Ok(Variant::Void)
}

fn video_overlay_cancel_period_event(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.cancelPeriodEvent")?;
    runtime
        .host_mut()
        .video_overlay_state_mut(this)
        .period_event_frame = -1;
    Ok(Variant::Void)
}

fn video_overlay_select_audio_stream(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.selectAudioStream")?;
    let stream = optional_integer(&args, 0)?.unwrap_or(0);
    runtime
        .host_mut()
        .video_overlay_state_mut(this)
        .enabled_audio_stream = stream;
    Ok(Variant::Void)
}

fn video_overlay_set_mixing_layer(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.setMixingLayer")?;
    let layer1 = args.first().cloned().unwrap_or_default();
    let layer2 = args.get(1).cloned().unwrap_or_default();
    let state = runtime.host_mut().video_overlay_state_mut(this);
    state.layer1 = layer1;
    state.layer2 = layer2;
    Ok(Variant::Void)
}

fn video_overlay_reset_mixing_layer(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let this = video_this(runtime, this_obj, "VideoOverlay.resetMixingLayer")?;
    let state = runtime.host_mut().video_overlay_state_mut(this);
    state.layer1 = Variant::Void;
    state.layer2 = Variant::Void;
    Ok(Variant::Void)
}

/// `prepare` is only meaningful for `vomLayer` in krkrz (it pre-renders the
/// mixing frame and later fires `onPeriod(perPrepare)`); for overlay/mixer
/// modes it is a no-op, which covers GINKA's usage.
fn video_overlay_prepare(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

pub(crate) fn install_video_overlay_methods(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "open", video_overlay_open);
    runtime.register_object_native(handle, "play", video_overlay_play);
    runtime.register_object_native(handle, "stop", video_overlay_stop);
    runtime.register_object_native(handle, "close", video_overlay_close);
    runtime.register_object_native(handle, "setPos", video_overlay_set_pos);
    runtime.register_object_native(handle, "setSize", video_overlay_set_size);
    runtime.register_object_native(handle, "setBounds", video_overlay_set_bounds);
    runtime.register_object_native(handle, "pause", video_overlay_pause);
    runtime.register_object_native(handle, "rewind", video_overlay_rewind);
    runtime.register_object_native(handle, "prepare", video_overlay_prepare);
    runtime.register_object_native(handle, "setSegmentLoop", video_overlay_set_segment_loop);
    runtime.register_object_native(
        handle,
        "cancelSegmentLoop",
        video_overlay_cancel_segment_loop,
    );
    runtime.register_object_native(handle, "setPeriodEvent", video_overlay_set_period_event);
    runtime.register_object_native(
        handle,
        "cancelPeriodEvent",
        video_overlay_cancel_period_event,
    );
    runtime.register_object_native(
        handle,
        "selectAudioStream",
        video_overlay_select_audio_stream,
    );
    runtime.register_object_native(handle, "setMixingLayer", video_overlay_set_mixing_layer);
    runtime.register_object_native(handle, "resetMixingLayer", video_overlay_reset_mixing_layer);
}

/// True when a class in the object's super chain declares a script (not
/// native) property with this name. Installing a native property of the same
/// name directly on the instance would shadow that script property — GINKA's
/// `Movie` class overrides `left`/`top`/`audioVolume`/`audioBalance`/
/// `layer1`/`layer2` this way and forwards explicitly through
/// `global.VideoOverlay.<name> = value`.
fn chain_has_script_property(
    runtime: &Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &str,
) -> bool {
    let mut current = runtime.object_super_class(handle);
    while let Some(class_handle) = current {
        if runtime.object_member_is_property(class_handle, name) {
            let member = runtime.object_member(class_handle, name);
            if !runtime.variant_is_native_property(&member) {
                return true;
            }
        }
        current = runtime.object_super_class(class_handle);
    }
    false
}

/// Instance-side counterpart of `install_properties` for VideoOverlay that
/// keeps script-overridden property names off the instance.
pub(crate) fn install_video_overlay_property_placeholders(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
) {
    for &property in video_overlay_property_names() {
        if runtime.has_object_member(handle, property) {
            continue;
        }
        if chain_has_script_property(runtime, handle, property) {
            continue;
        }
        runtime.set_object_member(handle, property, Variant::Void);
    }
}

pub(crate) fn install_video_native_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    preserve_script_properties: bool,
) {
    for &property in video_overlay_property_names() {
        if preserve_script_properties && chain_has_script_property(runtime, handle, property) {
            continue;
        }
        let property_handle = runtime.register_object_native_property(
            handle,
            property,
            move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                video_native_property_get(runtime, this_obj, property)
            },
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  value: Variant| {
                video_native_property_set(runtime, this_obj, property, value)
            },
        );
        if preserve_script_properties {
            runtime.set_object_member(
                handle,
                property,
                Variant::Closure(Closure::new(property_handle, Some(handle))),
            );
        }
    }
}

fn record_only_default(name: &str) -> Variant {
    if name == "mixingMovieAlpha" {
        return Variant::Integer(255);
    }
    if name.ends_with("RangeMin") {
        return Variant::Integer(0);
    }
    if name.ends_with("RangeMax") {
        return Variant::Integer(10000);
    }
    if name.ends_with("DefaultValue") {
        return Variant::Integer(5000);
    }
    if name.ends_with("StepSize") {
        return Variant::Integer(1);
    }
    // contrast / brightness / hue / saturation current values, mixingMovieBGColor
    if matches!(name, "contrast" | "brightness" | "hue" | "saturation") {
        return Variant::Integer(5000);
    }
    Variant::Integer(0)
}

fn video_native_property_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(Variant::Void);
    };
    let now = runtime.host_mut().now_millis();
    let state = runtime.host_mut().video_overlay_state_mut(this);
    let value = match name {
        "mode" => Variant::Integer(state.mode),
        "visible" => Variant::Integer(i64::from(state.visible)),
        "left" => Variant::Integer(state.left),
        "top" => Variant::Integer(state.top),
        "width" => Variant::Integer(state.width),
        "height" => Variant::Integer(state.height),
        "loop" => Variant::Integer(i64::from(state.looping)),
        "audioVolume" => Variant::Integer(state.audio_volume),
        "audioBalance" => Variant::Integer(state.audio_balance),
        "position" => Variant::Integer(state.elapsed_ms(now)),
        "frame" => Variant::Integer(state.frame_number(now)),
        "fps" => Variant::Real(state.fps()),
        "numberOfFrame" => Variant::Integer(
            state
                .metadata
                .as_ref()
                .map(|meta| meta.frame_count)
                .unwrap_or(0),
        ),
        "totalTime" => Variant::Integer(state.duration_ms()),
        "originalWidth" => Variant::Integer(
            state
                .metadata
                .as_ref()
                .map(|meta| i64::from(meta.width))
                .unwrap_or(0),
        ),
        "originalHeight" => Variant::Integer(
            state
                .metadata
                .as_ref()
                .map(|meta| i64::from(meta.height))
                .unwrap_or(0),
        ),
        "playRate" => Variant::Real(state.play_rate),
        "layer1" => state.layer1.clone(),
        "layer2" => state.layer2.clone(),
        "segmentLoopStartFrame" => {
            Variant::Integer(state.segment_loop.map(|(start, _)| start).unwrap_or(-1))
        }
        "segmentLoopEndFrame" => {
            Variant::Integer(state.segment_loop.map(|(_, end)| end).unwrap_or(-1))
        }
        "periodEventFrame" => Variant::Integer(state.period_event_frame),
        "numberOfAudioStream" => Variant::Integer(i64::from(
            state.metadata.as_ref().is_some_and(|meta| meta.has_audio),
        )),
        "enabledAudioStream" => Variant::Integer(state.enabled_audio_stream),
        "numberOfVideoStream" => Variant::Integer(i64::from(state.metadata.is_some())),
        "enabledVideoStream" => Variant::Integer(state.enabled_video_stream),
        record_only => state
            .extra
            .get(record_only)
            .cloned()
            .unwrap_or_else(|| record_only_default(record_only)),
    };
    Ok(value)
}

fn video_native_property_set(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
    value: Variant,
) -> Result<()> {
    let Some(this) = this_obj.map(|this| runtime.bound_this(this).unwrap_or(this)) else {
        return Ok(());
    };
    match name {
        // Read-only stream metadata.
        "fps"
        | "numberOfFrame"
        | "totalTime"
        | "originalWidth"
        | "originalHeight"
        | "numberOfAudioStream"
        | "numberOfVideoStream" => {}
        "mode" => {
            let mode = value.to_integer()?;
            runtime.host_mut().video_overlay_state_mut(this).mode = mode;
        }
        "visible" => {
            let visible = value.is_truthy();
            runtime.host_mut().video_overlay_state_mut(this).visible = visible;
        }
        "left" => {
            let left = value.to_integer()?;
            runtime.host_mut().video_overlay_state_mut(this).left = left;
        }
        "top" => {
            let top = value.to_integer()?;
            runtime.host_mut().video_overlay_state_mut(this).top = top;
        }
        "width" => {
            let width = value.to_integer()?;
            runtime.host_mut().video_overlay_state_mut(this).width = width;
        }
        "height" => {
            let height = value.to_integer()?;
            runtime.host_mut().video_overlay_state_mut(this).height = height;
        }
        "loop" => {
            let looping = value.is_truthy();
            runtime.host_mut().video_overlay_state_mut(this).looping = looping;
        }
        "audioVolume" => {
            let volume = value.to_integer()?.clamp(0, 100000);
            let audio_id = {
                let state = runtime.host_mut().video_overlay_state_mut(this);
                state.audio_volume = volume;
                state.audio_id
            };
            if let Some(id) = audio_id {
                runtime
                    .host_mut()
                    .queue_audio_command(AudioCommand::SetVolume {
                        id,
                        volume: volume as f32 / 100000.0,
                        fade_seconds: 0.0,
                    });
            }
        }
        "audioBalance" => {
            let balance = value.to_integer()?.clamp(-100000, 100000);
            runtime
                .host_mut()
                .video_overlay_state_mut(this)
                .audio_balance = balance;
        }
        "position" => {
            let position = value.to_integer()?;
            video_seek(runtime, this, position);
        }
        "frame" => {
            let frame = value.to_integer()?.max(0);
            let fps = runtime
                .host()
                .video_overlay_state(this)
                .map(|state| state.fps())
                .unwrap_or(0.0);
            if fps > 0.0 {
                video_seek(runtime, this, (frame as f64 * 1000.0 / fps) as i64);
            }
        }
        "playRate" => {
            let rate = value.to_real()?;
            if rate > 0.0 {
                // Re-anchor the clock so the position does not jump.
                let now = runtime.host_mut().now_millis();
                let state = runtime.host_mut().video_overlay_state_mut(this);
                state.position_ms = state.elapsed_ms(now);
                if state.clock_anchor_ms.is_some() {
                    state.clock_anchor_ms = Some(now);
                }
                state.play_rate = rate;
            }
        }
        "layer1" => {
            runtime.host_mut().video_overlay_state_mut(this).layer1 = value;
        }
        "layer2" => {
            runtime.host_mut().video_overlay_state_mut(this).layer2 = value;
        }
        "segmentLoopStartFrame" => {
            let start = value.to_integer()?;
            let state = runtime.host_mut().video_overlay_state_mut(this);
            let end = state.segment_loop.map(|(_, end)| end).unwrap_or(-1);
            state.segment_loop = (end > start && start >= 0).then_some((start, end));
        }
        "segmentLoopEndFrame" => {
            let end = value.to_integer()?;
            let state = runtime.host_mut().video_overlay_state_mut(this);
            let start = state.segment_loop.map(|(start, _)| start).unwrap_or(-1);
            state.segment_loop = (end > start && start >= 0).then_some((start, end));
        }
        "periodEventFrame" => {
            let frame = value.to_integer()?;
            let state = runtime.host_mut().video_overlay_state_mut(this);
            state.period_event_frame = frame;
            state.period_event_fired = false;
        }
        "enabledAudioStream" => {
            let stream = value.to_integer()?;
            runtime
                .host_mut()
                .video_overlay_state_mut(this)
                .enabled_audio_stream = stream;
        }
        "enabledVideoStream" => {
            let stream = value.to_integer()?;
            runtime
                .host_mut()
                .video_overlay_state_mut(this)
                .enabled_video_stream = stream;
        }
        record_only => {
            runtime
                .host_mut()
                .video_overlay_state_mut(this)
                .extra
                .insert(record_only.to_string(), value);
        }
    }
    Ok(())
}

/// Delivers a VideoOverlay event (`onStatusChanged` / `onPeriod` /
/// `onCallbackCommand`) to script code. Script overrides take priority: an
/// own member (assigned handler), then secondary class extenders, then the
/// first script implementation found along the super chain (script class
/// methods such as GINKA's `Movie.onStatusChanged` live on the class object,
/// not the instance). Native fallback stubs stay silent.
fn call_video_event(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    name: &str,
    args: Vec<Variant>,
) -> Result<()> {
    let callback = runtime.object_member(this, name);
    if !matches!(callback, Variant::Void) && !runtime.variant_is_native_function(&callback) {
        return runtime.call_object_method(this, name, args).map(|_| ());
    }
    if runtime.call_secondary_class_method(this, name, args.clone())? {
        return Ok(());
    }
    let mut current = runtime.object_super_class(this);
    while let Some(class_handle) = current {
        let member = runtime.object_member(class_handle, name);
        if !matches!(member, Variant::Void) {
            if runtime.variant_is_native_function(&member) {
                return Ok(());
            }
            return runtime.call_object_method(this, name, args).map(|_| ());
        }
        current = runtime.object_super_class(class_handle);
    }
    Ok(())
}

fn call_video_status_changed(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    status: &'static str,
) -> Result<()> {
    call_video_event(
        runtime,
        this,
        "onStatusChanged",
        vec![Variant::String(status.to_string())],
    )
}

fn call_video_period(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    reason: i64,
) -> Result<()> {
    call_video_event(runtime, this, "onPeriod", vec![Variant::Integer(reason)])
}

/// Per-frame video upkeep: feeds decoded frames to overlays against the
/// playback clock, handles segment loops and period events, and fires the
/// terminal `stop` event at end of stream (or on decode failure, so a broken
/// stream cannot soft-lock the scenario).
pub(crate) fn tick_video_overlays(runtime: &mut Runtime<KrkrHost>) -> Result<()> {
    let now = runtime.host_mut().now_millis();
    let handles = runtime.host().video_overlay_handles();
    let mut stale = Vec::new();
    for this in handles {
        if !runtime.object_valid(this) {
            stale.push(this);
            continue;
        }
        enum PostAction {
            Nothing,
            FireStop,
            FirePeriod(i64),
        }
        let mut audio_to_stop = None;
        let mut needs_texture = false;
        let mut logs = Vec::new();
        let action = {
            let state = runtime.host_mut().video_overlay_state_mut(this);
            if state.status != VIDEO_STATUS_PLAY && state.status != VIDEO_STATUS_PAUSE {
                PostAction::Nothing
            } else {
                let mut action = PostAction::Nothing;
                let elapsed = state.elapsed_ms(now);
                let mut decode_failed = false;
                if let Some(session) = &mut state.session {
                    if let Some(frame) = session.take_frame_at(elapsed) {
                        if state.texture_id.is_none() {
                            needs_texture = true;
                        }
                        if !state.logged_first_frame {
                            state.logged_first_frame = true;
                            logs.push(format!(
                                "VideoOverlay: first frame decoded ({}x{}, pts={} ms, visible={}, {}x{}@{},{})",
                                frame.width,
                                frame.height,
                                frame.pts_ms,
                                state.visible,
                                state.width,
                                state.height,
                                state.left,
                                state.top
                            ));
                        }
                        state.last_uploaded_pts = -1;
                        state.current_frame = Some(frame);
                    }
                    if let Some(error) = session.latest_error.take() {
                        logs.push(format!("VideoOverlay decode error: {error}"));
                        decode_failed = true;
                    }
                }
                let frame_number = state.frame_number(now);
                if let Some((start, end)) = state.segment_loop
                    && end > start
                    && frame_number >= end
                {
                    let fps = state.fps();
                    if fps > 0.0 {
                        let position = (start as f64 * 1000.0 / fps) as i64;
                        state.position_ms = position;
                        if state.clock_anchor_ms.is_some() {
                            state.clock_anchor_ms = Some(now);
                        }
                        if let Some(session) = &mut state.session {
                            session.seek(position);
                        }
                        state.period_event_fired = false;
                        action = PostAction::FirePeriod(PER_SEG_LOOP);
                    }
                }
                if state.period_event_frame >= 0
                    && !state.period_event_fired
                    && frame_number >= state.period_event_frame
                {
                    state.period_event_fired = true;
                    action = PostAction::FirePeriod(PER_PERIOD);
                }
                if state.status == VIDEO_STATUS_PLAY && !state.stop_fired {
                    let duration = state.duration_ms();
                    if decode_failed || (duration > 0 && state.elapsed_ms(now) >= duration) {
                        if state.looping && !decode_failed {
                            state.position_ms = 0;
                            state.clock_anchor_ms = Some(now);
                            if let Some(session) = &mut state.session {
                                session.seek(0);
                            }
                            state.period_event_fired = false;
                            action = PostAction::FirePeriod(PER_LOOP);
                        } else {
                            logs.push(format!(
                                "VideoOverlay: end of stream ({})",
                                state.storage.as_deref().unwrap_or("?")
                            ));
                            audio_to_stop = video_stop_playback(state);
                            action = PostAction::FireStop;
                        }
                    }
                }
                action
            }
        };
        for message in logs {
            runtime.host_mut().log(&message);
        }
        if needs_texture {
            let texture_id = runtime.host_mut().allocate_video_texture_id();
            runtime.host_mut().video_overlay_state_mut(this).texture_id = Some(texture_id);
        }
        if let Some(id) = audio_to_stop {
            runtime.host_mut().queue_audio_command(AudioCommand::Stop {
                id,
                fade_seconds: 0.0,
            });
        }
        match action {
            PostAction::Nothing => {}
            PostAction::FireStop => call_video_status_changed(runtime, this, VIDEO_STATUS_STOP)?,
            PostAction::FirePeriod(reason) => call_video_period(runtime, this, reason)?,
        }
    }
    for handle in stale {
        runtime.host_mut().remove_video_overlay(handle);
    }
    Ok(())
}

/// Builds the presentation quad for every visible playing overlay, plus the
/// texture upload for frames not yet on the GPU. The renderer caches textures
/// by id, so uploads are only emitted when a new frame arrived.
pub(crate) fn video_overlay_frame_quads(
    host: &mut KrkrHost,
) -> (Vec<ImageUpload>, Vec<DrawCommand>) {
    let mut uploads = Vec::new();
    let mut commands = Vec::new();
    let mut present_logs = Vec::new();
    for state in host.video_overlays_mut().values_mut() {
        let skip_reason = if state.status != VIDEO_STATUS_PLAY && state.status != VIDEO_STATUS_PAUSE
        {
            Some(format!("status={}", state.status))
        } else if !state.visible {
            Some("visible=false".to_string())
        } else if state.current_frame.is_none() || state.texture_id.is_none() {
            Some(format!(
                "current_frame={}, texture_id={}",
                state.current_frame.is_some(),
                state.texture_id.is_some()
            ))
        } else {
            let frame = state.current_frame.as_ref().expect("checked above");
            if frame.width == 0 || frame.height == 0 || frame.data.is_empty() {
                Some(format!(
                    "empty frame ({}x{}, {} bytes)",
                    frame.width,
                    frame.height,
                    frame.data.len()
                ))
            } else {
                None
            }
        };
        let Some(texture_id) = state.texture_id else {
            if state.quad_active {
                state.quad_active = false;
                present_logs.push(format!(
                    "VideoOverlay: quad stopped ({})",
                    skip_reason.unwrap_or_else(|| "no texture".to_string())
                ));
            }
            continue;
        };
        if let Some(reason) = skip_reason {
            if state.quad_active {
                state.quad_active = false;
                present_logs.push(format!("VideoOverlay: quad stopped ({reason})"));
            }
            continue;
        }
        state.quad_active = true;
        let frame = state.current_frame.as_ref().expect("skip_reason checked");
        if frame.pts_ms != state.last_uploaded_pts {
            state.last_uploaded_pts = frame.pts_ms;
            uploads.push(ImageUpload::new(
                texture_id,
                frame.width,
                frame.height,
                Arc::<[u8]>::from(frame.data.as_slice()),
            ));
        }
        let (width, height) = if state.width > 0 && state.height > 0 {
            (state.width as f32, state.height as f32)
        } else {
            (frame.width as f32, frame.height as f32)
        };
        if !state.logged_present {
            state.logged_present = true;
            present_logs.push(format!(
                "VideoOverlay: presenting (texture={texture_id}, rect=({},{} {}x{}), pts={} ms)",
                state.left, state.top, width as i64, height as i64, frame.pts_ms
            ));
        }
        commands.push(DrawCommand::Image(ImageCommand {
            texture_id,
            rect: Rect::new(state.left as f32, state.top as f32, width, height),
            source_rect: Rect::new(0.0, 0.0, frame.width as f32, frame.height as f32),
            texture_size: Size::new(frame.width as f32, frame.height as f32),
            opacity: 1.0,
        }));
    }
    for message in present_logs {
        host.log(&message);
    }
    (uploads, commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pts_ms: i64) -> VideoFrame {
        VideoFrame {
            pts_ms,
            width: 1,
            height: 1,
            stride: 4,
            data: vec![0; 4],
        }
    }

    fn session_with_frames(frames: &[i64]) -> DecodeSession {
        let (tx, rx) = mpsc::sync_channel(frames.len().max(1));
        for &pts in frames {
            tx.send(DecodeMessage::Frame(frame(pts))).unwrap();
        }
        DecodeSession {
            command_tx: None,
            rx: Some(rx),
            join: None,
            latest_error: None,
            pending: VecDeque::new(),
        }
    }

    /// Regression: when the decode thread outruns the presentation clock, the
    /// drained frames ahead of the clock must stay buffered for later ticks.
    /// Dropping them (the old behaviour) froze playback on the first frame
    /// whenever decoding sprinted ahead — the desktop "black OP" bug.
    #[test]
    fn take_frame_at_buffers_future_frames() {
        let mut session = session_with_frames(&[0, 33, 66, 99]);
        assert_eq!(session.take_frame_at(0).map(|f| f.pts_ms), Some(0));
        assert_eq!(session.take_frame_at(40).map(|f| f.pts_ms), Some(33));
        assert_eq!(session.take_frame_at(70).map(|f| f.pts_ms), Some(66));
        assert_eq!(session.take_frame_at(200).map(|f| f.pts_ms), Some(99));
        assert!(session.take_frame_at(300).is_none());
    }

    /// A tick with several due frames returns the newest of them.
    #[test]
    fn take_frame_at_returns_newest_due_frame() {
        let mut session = session_with_frames(&[0, 33, 66, 99]);
        assert_eq!(session.take_frame_at(70).map(|f| f.pts_ms), Some(66));
        // 99 ms is not due at t=70; it presents once the clock passes it.
        assert!(session.take_frame_at(70).is_none());
        assert_eq!(session.take_frame_at(100).map(|f| f.pts_ms), Some(99));
    }

    /// Seeking drops buffered pre-seek frames so stale pictures never present.
    #[test]
    fn seek_clears_pending_frames() {
        let (command_tx, _command_rx) = mpsc::sync_channel(1);
        let (tx, rx) = mpsc::sync_channel(2);
        for pts in [0, 33] {
            tx.send(DecodeMessage::Frame(frame(pts))).unwrap();
        }
        let mut session = DecodeSession {
            command_tx: Some(command_tx),
            rx: Some(rx),
            join: None,
            latest_error: None,
            pending: VecDeque::new(),
        };
        assert_eq!(session.take_frame_at(0).map(|f| f.pts_ms), Some(0));
        session.seek(5000);
        assert!(session.pending.is_empty());
    }
}
