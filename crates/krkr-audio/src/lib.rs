#[cfg(feature = "opus")]
use std::convert::TryFrom;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread,
    time::Duration,
};

mod pcm_tap;
mod wave_filter;

#[cfg(feature = "opus")]
use audiopus::{
    Channels as OpusChannels, MutSignals, SampleRate as OpusSampleRate,
    coder::Decoder as OpusDecoder, packet::Packet as OpusPacket,
};
#[cfg(not(target_arch = "wasm32"))]
use kira::sound::streaming::{StreamingSoundData, StreamingSoundHandle};
use kira::{
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Tween,
    sound::{
        PlaybackState,
        static_sound::{StaticSoundData, StaticSoundHandle},
    },
    track::{TrackBuilder, TrackHandle},
};
#[cfg(not(target_arch = "wasm32"))]
use kira::{Frame, sound::FromFileError};
use krkr_core::{
    AudioBus, AudioCommand, AudioLoadPolicy, AudioSourceRef, PcmStreamSource, ResourceStream,
    StoragePort,
};
pub use krkr_core::{
    AudioError, AudioEvent, AudioInstanceId, AudioSink, AudioState, AudioStatusEvent,
    AudioStatusLevel, PcmAudioSpec,
};
pub use pcm_tap::{
    DEFAULT_CAPACITY_FRAMES, MAX_READ_FRAMES, PcmTap, PcmTapFeed, PcmTapSnapshot, PcmTapState,
    PcmTapWindow,
};
use symphonia::core::io::MediaSource;
#[cfg(feature = "opus")]
use symphonia::core::{
    codecs::CODEC_TYPE_OPUS, errors::Error as SymphoniaError, io::MediaSourceStream,
};
pub use wave_filter::{
    WaveFilter, WaveFilterChain, WaveFilterId, WaveFilterSkip, register_wave_filter,
    resolve_wave_filter, unregister_wave_filter,
};

const STATIC_CACHE_CAPACITY_BYTES: usize = 64 * 1024 * 1024;
const STATIC_CACHE_MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;
const PRELOAD_MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

/// Frames of silence a [`ChannelPcmDecoder`] emits while its producer stalls
/// (end of stream or a waiting movie decoder), matching kira's own silence
/// block size so the decode loop never spins on empty chunks.
#[cfg(not(target_arch = "wasm32"))]
const STALL_SILENCE_FRAMES: usize = 4096;

pub struct AudioSystem {
    state: AudioState,
    control_tx: Option<mpsc::Sender<ControlMessage>>,
    event_rx: Option<mpsc::Receiver<AudioEvent>>,
    pcm_tap: PcmTap,
}

struct KiraBackend {
    manager: AudioManager<DefaultBackend>,
    bgm_track: TrackHandle,
    se_track: TrackHandle,
    handles: BTreeMap<AudioInstanceId, PlayingSound>,
}

struct PlayRequest {
    id: AudioInstanceId,
    bus: AudioBus,
    storage: String,
    looping: bool,
    volume: f32,
    paused: bool,
    /// The `WaveSoundBuffer.filters` chain this playback runs through, when
    /// the instance has one.
    chain: Option<WaveFilterChain>,
    /// The instance's PCM tap, for a chain that publishes what it renders.
    tap: Option<PcmTapFeed>,
}

enum PlayingSound {
    Static {
        bus: AudioBus,
        handle: StaticSoundHandle,
    },
    #[cfg(not(target_arch = "wasm32"))]
    Streaming {
        bus: AudioBus,
        handle: StreamingSoundHandle<FromFileError>,
    },
}

enum PreparedSound {
    Static(StaticSoundData),
    #[cfg(not(target_arch = "wasm32"))]
    Streaming(StreamingSoundData<FromFileError>),
}

enum ControlMessage {
    Command(AudioCommand),
    SetResourceProvider(Option<Arc<dyn StoragePort>>),
    Prepared(Box<PreparedAudio>),
    /// Replaces the filter chain of one instance (`AudioCommand::SetFilters`,
    /// or [`AudioSystem::set_wave_filters`]).
    WaveFilters {
        id: AudioInstanceId,
        filters: Vec<i64>,
    },
    Shutdown,
}

enum LoaderMessage {
    Load(LoadRequest),
    Shutdown,
}

struct LoadRequest {
    source: AudioSourceRef,
    load_policy: AudioLoadPolicy,
    provider: Arc<dyn StoragePort>,
    provider_epoch: u64,
    provider_revision: u64,
    kind: LoadRequestKind,
}

enum LoadRequestKind {
    Play {
        id: AudioInstanceId,
        generation: u64,
    },
    Preload,
}

struct PreparedAudio {
    source: AudioSourceRef,
    provider_epoch: u64,
    kind: PreparedAudioKind,
}

enum PreparedAudioKind {
    Play {
        id: AudioInstanceId,
        generation: u64,
        result: Box<Result<PreparedSound, AudioLoadFailure>>,
    },
    Preload {
        result: Result<(), AudioLoadFailure>,
    },
}

#[derive(Debug)]
struct AudioLoadFailure {
    storage: String,
    message: String,
}

struct SoundSlot {
    generation: u64,
    bus: AudioBus,
    looping: bool,
    volume: f32,
    paused: bool,
    tap: Option<PcmTapFeed>,
    /// The `interface` values of the instance's `filters` array at the moment
    /// its playback was requested.  Resolved to a [`WaveFilterChain`] when the
    /// sound reaches the backend.
    filters: Vec<i64>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct StaticCacheKey {
    provider_epoch: u64,
    provider_revision: u64,
    storage: String,
}

struct StaticCacheEntry {
    data: StaticSoundData,
    bytes: usize,
}

struct StaticSoundCache {
    entries: HashMap<StaticCacheKey, StaticCacheEntry>,
    lru: VecDeque<StaticCacheKey>,
    bytes: usize,
    capacity_bytes: usize,
    max_entry_bytes: usize,
}

struct ResourceMediaSource {
    stream: Mutex<Box<dyn ResourceStream>>,
    byte_len: Option<u64>,
}

impl fmt::Debug for AudioSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AudioSystem")
            .field("state", &self.state)
            .field("ready", &self.control_tx.is_some())
            .finish()
    }
}

impl Default for AudioSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AudioSystem {
    fn drop(&mut self) {
        if let Some(tx) = self.control_tx.take() {
            let _ = tx.send(ControlMessage::Shutdown);
        }
    }
}

impl AudioSystem {
    pub fn new() -> Self {
        let pcm_tap = PcmTap::default();
        // Publish the process-wide handle: a plugin cannot be handed the
        // `AudioSystem` this engine shell owns, but the sample readback
        // (`getVisBuffer`, `getSample.dll`, `fftgraph.dll`) needs the live tap.
        // The slot holds a weak handle, so the registry — and the ring buffers
        // of the instances in it — is freed with the system that owns it; a
        // process that builds a second system (tests) sees the newest one.
        *active_tap_slot()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = pcm_tap.downgrade();
        Self {
            state: AudioState::Stopped,
            control_tx: None,
            event_rx: None,
            pcm_tap,
        }
    }

    /// Shared decoded-PCM tap of every playing instance.
    ///
    /// The handle stays valid for the lifetime of the system: registering,
    /// reading and retiring instances all go through it, so the engine can hold
    /// a clone next to the `AudioSink` instead of talking to the audio worker.
    /// [`PcmTap`] documents the cursor and window semantics.
    pub fn pcm_tap(&self) -> PcmTap {
        self.pcm_tap.clone()
    }

    pub fn prepare(&mut self) -> Result<(), AudioError> {
        if self.control_tx.is_some() {
            self.state = AudioState::Ready;
            return Ok(());
        }

        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker_control_tx = control_tx.clone();
        let worker_tap = self.pcm_tap.clone();
        thread::Builder::new()
            .name("krkr-audio-control".to_string())
            .spawn(move || {
                audio_control_worker(control_rx, worker_control_tx, event_tx, worker_tap)
            })
            .map_err(|error| AudioError::WorkerUnavailable(error.to_string()))?;

        self.control_tx = Some(control_tx);
        self.event_rx = Some(event_rx);
        self.state = AudioState::Ready;
        Ok(())
    }

    /// Attaches the `WaveSoundBuffer.filters` chain of `id`, replacing any
    /// chain that instance had.
    ///
    /// `filters` carries the `interface` value of each element of the
    /// instance's script-side `filters` array, in array order; the worker
    /// resolves them through [`resolve_wave_filter`] when the sound starts
    /// playing and reports elements it cannot resolve as a status warning.
    /// This is the sink-side door onto the same state
    /// `AudioCommand::SetFilters` writes (see [`crate::WaveFilterChain`]);
    /// the chain of an instance that never plays stays pending until it does.
    pub fn set_wave_filters(
        &mut self,
        id: AudioInstanceId,
        filters: Vec<i64>,
    ) -> Result<(), AudioError> {
        self.send_control(ControlMessage::WaveFilters { id, filters })
    }

    pub fn set_resource_provider(
        &mut self,
        provider: Option<Arc<dyn StoragePort>>,
    ) -> Result<(), AudioError> {
        self.send_control(ControlMessage::SetResourceProvider(provider))
    }

    pub fn clear_resource_provider(&mut self) -> Result<(), AudioError> {
        self.set_resource_provider(None)
    }

    pub fn submit_commands(
        &mut self,
        commands: impl IntoIterator<Item = AudioCommand>,
    ) -> Result<(), AudioError> {
        self.prepare()?;
        for command in commands {
            self.send_control(ControlMessage::Command(command))?;
        }
        Ok(())
    }

    pub fn drain_events(&mut self) -> Vec<AudioEvent> {
        let Some(rx) = &self.event_rx else {
            return Vec::new();
        };
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    pub const fn state(&self) -> AudioState {
        self.state
    }

    fn send_control(&mut self, message: ControlMessage) -> Result<(), AudioError> {
        self.prepare()?;
        let tx = self.control_tx.as_ref().ok_or_else(|| {
            AudioError::WorkerUnavailable("control channel is not initialized".to_string())
        })?;
        tx.send(message).map_err(|error| {
            self.state = AudioState::Stopped;
            AudioError::CommandFailed(error.to_string())
        })
    }
}

/// The PCM tap of the process's most recently created [`AudioSystem`], when one
/// is still alive.
///
/// This is the readback a plugin reaches the decoded samples through: the
/// engine names neither this crate nor the `AudioSystem` its shell owns, so the
/// system publishes a weak handle here at construction (see
/// [`AudioSystem::new`]). `None` means no audio system exists — the reference's
/// "nothing to visualize" answer, not an error.
pub fn active_pcm_tap() -> Option<PcmTap> {
    let slot = active_tap_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    PcmTap::upgrade(&slot)
}

fn active_tap_slot() -> &'static Mutex<std::sync::Weak<pcm_tap::TapShared>> {
    static SLOT: OnceLock<Mutex<std::sync::Weak<pcm_tap::TapShared>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(std::sync::Weak::new()))
}

impl AudioSink for AudioSystem {
    fn prepare(&mut self) -> Result<(), AudioError> {
        AudioSystem::prepare(self)
    }

    fn submit(&mut self, commands: &[AudioCommand]) -> Result<(), AudioError> {
        self.submit_commands(commands.iter().cloned())
    }

    fn poll_events(&mut self) -> Vec<AudioEvent> {
        self.drain_events()
    }
}

/// Deterministic sink used by headless tools and tests. It intentionally does
/// not decode or play anything; callers can inspect submitted commands and
/// inject completion events explicitly.
#[derive(Clone, Debug, Default)]
pub struct VirtualAudioSink {
    commands: Vec<AudioCommand>,
    events: Vec<AudioEvent>,
}

impl VirtualAudioSink {
    pub fn commands(&self) -> &[AudioCommand] {
        &self.commands
    }

    pub fn take_commands(&mut self) -> Vec<AudioCommand> {
        std::mem::take(&mut self.commands)
    }

    pub fn push_event(&mut self, event: AudioEvent) {
        self.events.push(event);
    }
}

impl AudioSink for VirtualAudioSink {
    fn prepare(&mut self) -> Result<(), AudioError> {
        Ok(())
    }

    fn submit(&mut self, commands: &[AudioCommand]) -> Result<(), AudioError> {
        self.commands.extend_from_slice(commands);
        Ok(())
    }

    fn poll_events(&mut self) -> Vec<AudioEvent> {
        std::mem::take(&mut self.events)
    }
}

fn audio_control_worker(
    rx: mpsc::Receiver<ControlMessage>,
    control_tx: mpsc::Sender<ControlMessage>,
    event_tx: mpsc::Sender<AudioEvent>,
    pcm_tap: PcmTap,
) {
    let mut backend = match KiraBackend::new() {
        Ok(backend) => backend,
        Err(error) => {
            report_event(&event_tx, AudioStatusLevel::Error, error.to_string());
            return;
        }
    };
    let static_tx = match spawn_static_loader(control_tx.clone()) {
        Ok(tx) => tx,
        Err(error) => {
            report_event(
                &event_tx,
                AudioStatusLevel::Error,
                format!("failed to start static audio loader: {error}"),
            );
            return;
        }
    };
    let streaming_tx = match spawn_streaming_loader(control_tx) {
        Ok(tx) => tx,
        Err(error) => {
            report_event(
                &event_tx,
                AudioStatusLevel::Error,
                format!("failed to start streaming audio loader: {error}"),
            );
            let _ = static_tx.send(LoaderMessage::Shutdown);
            return;
        }
    };

    let mut provider = None;
    let mut provider_epoch = 0u64;
    let mut next_generation = 1u64;
    let mut slots = BTreeMap::new();
    // The `WaveSoundBuffer.filters` chain of every instance that has one, by
    // the `interface` values the engine read out of the instance's array.  It
    // outlives a `Stop` — the reference clears the chain in `Clear`
    // (`sound/win32/WaveImpl.cpp:2336`, i.e. at `Open`), not in `Stop` — so a
    // stop/play cycle plays with the same filters.
    let mut wave_filters: BTreeMap<AudioInstanceId, Vec<i64>> = BTreeMap::new();

    loop {
        match rx.recv_timeout(Duration::from_millis(16)) {
            Ok(ControlMessage::Command(command)) => handle_audio_command(
                command,
                ControlContext {
                    backend: &mut backend,
                    provider: provider.clone(),
                    provider_epoch,
                    next_generation: &mut next_generation,
                    slots: &mut slots,
                    static_tx: &static_tx,
                    streaming_tx: &streaming_tx,
                    event_tx: &event_tx,
                    pcm_tap: &pcm_tap,
                    wave_filters: &mut wave_filters,
                },
            ),
            Ok(ControlMessage::WaveFilters { id, filters }) => {
                set_wave_filters(&mut wave_filters, id, filters);
            }
            Ok(ControlMessage::SetResourceProvider(next_provider)) => {
                backend.stop_all(Tween::default());
                stop_slot_taps(&slots);
                slots.clear();
                provider = next_provider;
                provider_epoch = provider_epoch.saturating_add(1);
            }
            Ok(ControlMessage::Prepared(prepared)) => {
                if prepared.provider_epoch == provider_epoch {
                    handle_prepared_audio(
                        *prepared,
                        &mut backend,
                        &mut slots,
                        &mut wave_filters,
                        &event_tx,
                        &pcm_tap,
                    );
                }
            }
            Ok(ControlMessage::Shutdown) => {
                backend.stop_all(Tween::default());
                stop_slot_taps(&slots);
                let _ = static_tx.send(LoaderMessage::Shutdown);
                let _ = streaming_tx.send(LoaderMessage::Shutdown);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        report_stopped_sounds(&mut backend, &mut slots, &event_tx);
        backend.sync_tap_positions(&slots);
    }
}

/// Replaces one instance's chain, the way `ClearFilterChain` followed by
/// `RebuildFilterChain` does at `Open` (`sound/win32/WaveImpl.cpp:2935`): the
/// filters the instance had are told to `Clear()` and the new list takes their
/// place.  An empty list drops the chain.
fn set_wave_filters(
    wave_filters: &mut BTreeMap<AudioInstanceId, Vec<i64>>,
    id: AudioInstanceId,
    filters: Vec<i64>,
) {
    let previous = wave_filters.insert(id, filters.clone());
    if previous.as_deref() == Some(filters.as_slice()) {
        return;
    }
    if let Some(previous) = previous {
        for filter_id in previous {
            if let Some(filter) = resolve_wave_filter(filter_id) {
                filter.clear();
            }
        }
    }
}

/// Marks every slot's tap stopped so readers see a silent window instead of
/// samples of a sound that no longer plays.
fn stop_slot_taps(slots: &BTreeMap<AudioInstanceId, SoundSlot>) {
    for slot in slots.values() {
        if let Some(tap) = &slot.tap {
            tap.stop();
        }
    }
}

struct ControlContext<'a> {
    backend: &'a mut KiraBackend,
    provider: Option<Arc<dyn StoragePort>>,
    provider_epoch: u64,
    next_generation: &'a mut u64,
    slots: &'a mut BTreeMap<AudioInstanceId, SoundSlot>,
    static_tx: &'a mpsc::Sender<LoaderMessage>,
    streaming_tx: &'a mpsc::Sender<LoaderMessage>,
    event_tx: &'a mpsc::Sender<AudioEvent>,
    pcm_tap: &'a PcmTap,
    wave_filters: &'a mut BTreeMap<AudioInstanceId, Vec<i64>>,
}

fn handle_audio_command(command: AudioCommand, mut context: ControlContext<'_>) {
    match command {
        AudioCommand::Play {
            id,
            bus,
            source,
            load_policy,
            looping,
            volume,
        } => {
            context.backend.stop_id(id, Tween::default());
            // The previous sound on this id is gone; drop its tap too so
            // readers cannot see stale samples of it.
            context.pcm_tap.remove(id);
            let generation = *context.next_generation;
            *context.next_generation = context.next_generation.saturating_add(1);
            context.slots.insert(
                id,
                SoundSlot {
                    generation,
                    bus,
                    looping,
                    volume,
                    paused: false,
                    tap: None,
                    filters: context.wave_filters.get(&id).cloned().unwrap_or_default(),
                },
            );
            dispatch_play_load(
                id,
                bus,
                generation,
                source,
                load_policy,
                looping,
                &mut context,
            );
        }
        AudioCommand::Preload {
            source,
            load_policy,
        } => dispatch_preload(source, load_policy, &context),
        AudioCommand::SetFilters { id, filters } => {
            set_wave_filters(context.wave_filters, id, filters);
        }
        AudioCommand::PlayPcmStream {
            id,
            bus,
            source,
            volume,
        } => {
            // Live PCM (movie soundtracks) needs no loader round-trip; play
            // it right away and register the slot so Stop/SetVolume/Pause and
            // end-of-stream reporting behave like any other sound.
            let generation = *context.next_generation;
            *context.next_generation = context.next_generation.saturating_add(1);
            let spec = source.spec;
            let tap = context.pcm_tap.register(id, spec);
            context.slots.insert(
                id,
                SoundSlot {
                    generation,
                    bus,
                    looping: false,
                    volume,
                    paused: false,
                    tap: Some(tap.clone()),
                    // A movie soundtrack does not go through a
                    // `WaveSoundBuffer`'s `filters`: the reference plays it
                    // through the movie graph (`VideoOverlay`), which has no
                    // chain of its own, and the engine never attaches one to a
                    // video instance's audio id.
                    filters: Vec::new(),
                },
            );
            if let Err(error) = context
                .backend
                .play_pcm_stream(id, bus, source, volume, tap)
            {
                context.slots.remove(&id);
                context.pcm_tap.remove(id);
                report_event(
                    context.event_tx,
                    AudioStatusLevel::Warning,
                    error.to_string(),
                );
            }
        }
        AudioCommand::Stop { id, fade_seconds } => {
            if let Some(slot) = context.slots.remove(&id)
                && let Some(tap) = &slot.tap
            {
                tap.stop();
            }
            context.backend.stop_id(id, tween(fade_seconds));
        }
        AudioCommand::SetVolume {
            id,
            volume,
            fade_seconds,
        } => {
            if let Some(slot) = context.slots.get_mut(&id) {
                slot.volume = volume;
            }
            context.backend.set_volume(id, volume, tween(fade_seconds));
        }
        AudioCommand::Pause { id, fade_seconds } => {
            if let Some(slot) = context.slots.get_mut(&id) {
                slot.paused = true;
                if let Some(tap) = &slot.tap {
                    tap.set_paused(true);
                }
            }
            context.backend.pause(id, tween(fade_seconds));
        }
        AudioCommand::Resume { id, fade_seconds } => {
            if let Some(slot) = context.slots.get_mut(&id) {
                slot.paused = false;
                if let Some(tap) = &slot.tap {
                    tap.set_paused(false);
                }
            }
            context.backend.resume(id, tween(fade_seconds));
        }
        AudioCommand::StopBus { bus, fade_seconds } => {
            let tween = tween(fade_seconds);
            context.backend.stop_bus(bus, tween);
            for slot in context.slots.values() {
                if slot.bus == bus
                    && let Some(tap) = &slot.tap
                {
                    tap.stop();
                }
            }
            context.slots.retain(|_, slot| slot.bus != bus);
        }
        AudioCommand::SetBusVolume {
            bus,
            volume,
            fade_seconds,
        } => {
            context
                .backend
                .set_bus_volume(bus, volume, tween(fade_seconds));
        }
    }
}

fn dispatch_play_load(
    id: AudioInstanceId,
    bus: AudioBus,
    generation: u64,
    source: AudioSourceRef,
    load_policy: AudioLoadPolicy,
    looping: bool,
    context: &mut ControlContext<'_>,
) {
    let Some(provider) = context.provider.clone() else {
        context.slots.remove(&id);
        report_event(
            context.event_tx,
            AudioStatusLevel::Warning,
            format!(
                "audio resource provider is not configured for `{}`",
                source.storage()
            ),
        );
        return;
    };
    let filters = context
        .slots
        .get(&id)
        .map(|slot| slot.filters.clone())
        .unwrap_or_default();
    // A buffer with a filter chain decodes whole and then plays through the
    // chain's own streaming decoder (`FilteredPcmDecoder`), because the chain
    // has to run inside the sample path: kira's own file decoder keeps its
    // frames private, so a streaming-policy load could not be filtered at all.
    // The reference has no such split — it decodes every buffer through the
    // chain on its own thread (`sound/win32/WaveImpl.cpp:2348-2364`) — so the
    // closest of this engine's paths is the static one, which is what a
    // filtered instance gets here.
    let effective_policy = if filters.is_empty() {
        resolve_play_policy(load_policy, bus, looping)
    } else {
        AudioLoadPolicy::StaticCached
    };
    let request = LoadRequest {
        source,
        load_policy: effective_policy,
        provider_revision: provider.revision(),
        provider,
        provider_epoch: context.provider_epoch,
        kind: LoadRequestKind::Play { id, generation },
    };
    let tx = match effective_policy {
        AudioLoadPolicy::Streaming => context.streaming_tx,
        AudioLoadPolicy::Auto | AudioLoadPolicy::StaticCached | AudioLoadPolicy::StaticUncached => {
            context.static_tx
        }
    };
    if let Err(error) = tx.send(LoaderMessage::Load(request)) {
        context.slots.remove(&id);
        report_event(
            context.event_tx,
            AudioStatusLevel::Error,
            format!("audio loader is unavailable: {error}"),
        );
    }
}

fn dispatch_preload(
    source: AudioSourceRef,
    load_policy: AudioLoadPolicy,
    context: &ControlContext<'_>,
) {
    if matches!(load_policy, AudioLoadPolicy::Streaming) {
        return;
    }
    let Some(provider) = context.provider.clone() else {
        report_event(
            context.event_tx,
            AudioStatusLevel::Warning,
            format!(
                "audio resource provider is not configured for `{}`",
                source.storage()
            ),
        );
        return;
    };
    let request = LoadRequest {
        source,
        load_policy,
        provider_revision: provider.revision(),
        provider,
        provider_epoch: context.provider_epoch,
        kind: LoadRequestKind::Preload,
    };
    if let Err(error) = context.static_tx.send(LoaderMessage::Load(request)) {
        report_event(
            context.event_tx,
            AudioStatusLevel::Error,
            format!("static audio loader is unavailable: {error}"),
        );
    }
}

fn handle_prepared_audio(
    prepared: PreparedAudio,
    backend: &mut KiraBackend,
    slots: &mut BTreeMap<AudioInstanceId, SoundSlot>,
    wave_filters: &mut BTreeMap<AudioInstanceId, Vec<i64>>,
    event_tx: &mpsc::Sender<AudioEvent>,
    pcm_tap: &PcmTap,
) {
    match prepared.kind {
        PreparedAudioKind::Play {
            id,
            generation,
            result,
        } => {
            let Some(slot) = slots.get_mut(&id) else {
                return;
            };
            if slot.generation != generation {
                return;
            }
            match *result {
                Ok(sound) => {
                    // The live list wins over the snapshot the play was queued
                    // with: the engine replaces it at `open`, which can land
                    // while the load is in flight.
                    let filters = wave_filters
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| slot.filters.clone());
                    let chain = prepared_sound_spec(&sound)
                        .map(|spec| build_chain(&filters, spec, event_tx, id))
                        .unwrap_or_else(|| {
                            if !filters.is_empty() {
                                report_event(
                                    event_tx,
                                    AudioStatusLevel::Warning,
                                    format!(
                                        "audio instance {} has {} filter(s) but its sound is \
                                         not filterable; playing unfiltered",
                                        id.0,
                                        filters.len()
                                    ),
                                );
                            }
                            None
                        });
                    // A filtered sound plays through the chain's own
                    // streaming decoder, which publishes the frames it renders
                    // (so the tap holds the filtered PCM, exactly where the
                    // reference's `getVisBuffer` reads the post-filter L2 unit,
                    // `sound/win32/WaveImpl.cpp:2507-2510`).  An unfiltered
                    // sound keeps its decoded buffer attached.
                    let tap = match &chain {
                        Some(_) => register_filtered_tap(pcm_tap, id, &sound, slot.paused),
                        None => register_sound_tap(pcm_tap, id, &sound, slot.looping, slot.paused),
                    };
                    slot.tap = tap.clone();
                    let request = PlayRequest {
                        id,
                        bus: slot.bus,
                        storage: prepared.source.storage,
                        looping: slot.looping,
                        volume: slot.volume,
                        paused: slot.paused,
                        chain,
                        tap,
                    };
                    if let Err(error) = backend.play_prepared(request, sound) {
                        if let Some(tap) = &slot.tap {
                            tap.stop();
                        }
                        slots.remove(&id);
                        report_event(event_tx, AudioStatusLevel::Warning, error.to_string());
                    }
                }
                Err(error) => {
                    slots.remove(&id);
                    report_event(
                        event_tx,
                        AudioStatusLevel::Warning,
                        format!(
                            "failed to prepare audio `{}`: {}",
                            error.storage, error.message
                        ),
                    );
                }
            }
        }
        PreparedAudioKind::Preload { result } => {
            if let Err(error) = result {
                report_event(
                    event_tx,
                    AudioStatusLevel::Warning,
                    format!(
                        "failed to preload audio `{}`: {}",
                        error.storage, error.message
                    ),
                );
            }
        }
    }
}

/// Registers the PCM tap of a sound that is about to play.
///
/// A static sound hands the tap the decoded buffer its [`StaticSoundData`] is
/// already holding (shared, never copied), so the whole waveform stays readable
/// at the playback cursor. kira decodes a streaming sound on its own thread into
/// a private queue and exposes neither the frames nor even its sample rate, so
/// there is nothing this tap can serve: the function returns `None` for a
/// streaming sound, **no tap instance is registered for its id**, and
/// [`PcmTap::read`], [`PcmTap::state`] and [`PcmTap::cursor`] answer `None` for
/// it — consumers must treat that as "no PCM available".
///
/// This is the readback's one real gap, and it is the *common* case: a looping
/// BGM is loaded with [`AudioLoadPolicy::Streaming`] by default
/// ([`resolve_play_policy`]), so the engine's `getVisBuffer`, `getSample.dll`
/// and `fftgraph.dll` see silence for it (`only_static_sounds_register_a_tap`
/// pins that).  Closing it needs a decoder of our own for that path —
/// `StreamingSoundData::from_decoder` with a file decoder that publishes every
/// chunk it decodes, which means decoding the formats ourselves instead of
/// letting kira do it — or loading such a sound whole (which costs its full
/// decoded size for every BGM, the thing the streaming path exists to avoid).
/// A buffer that carries filters or is loaded statically is unaffected:
/// [`FilteredPcmDecoder`] publishes what it renders, so its tap works.
fn register_sound_tap(
    pcm_tap: &PcmTap,
    id: AudioInstanceId,
    sound: &PreparedSound,
    looping: bool,
    paused: bool,
) -> Option<PcmTapFeed> {
    let data = match sound {
        PreparedSound::Static(data) => data,
        #[cfg(not(target_arch = "wasm32"))]
        PreparedSound::Streaming(_) => return None,
    };
    let feed = pcm_tap.register(
        id,
        PcmAudioSpec {
            sample_rate: data.sample_rate,
            channels: 2,
        },
    );
    feed.attach_decoded(Arc::clone(&data.frames), looping);
    if paused {
        feed.set_paused(true);
    }
    Some(feed)
}

/// The format a prepared sound decodes at, when the sound exposes one.
///
/// A kira streaming sound keeps its decoder (and therefore its sample rate and
/// frames) private — the same reason [`register_sound_tap`] cannot tap one —
/// so it has no spec here.  A filtered instance never reaches that case: its
/// load is forced onto the static path (`dispatch_play_load`).
fn prepared_sound_spec(sound: &PreparedSound) -> Option<PcmAudioSpec> {
    match sound {
        PreparedSound::Static(data) => Some(PcmAudioSpec {
            sample_rate: data.sample_rate,
            channels: 2,
        }),
        #[cfg(not(target_arch = "wasm32"))]
        PreparedSound::Streaming(_) => None,
    }
}

/// Builds the chain a playback runs through: the `filters` array the engine
/// read, folded over the sound's format in array order the way
/// `RebuildFilterChain` folds it (`sound/WaveIntf.cpp:898-905`), then reset
/// for the playback that is starting (`ResetFilterChain`, `:925-931`, from
/// `StartPlay`, `sound/win32/WaveImpl.cpp:2813`).
///
/// Elements the registry cannot resolve — a script-fabricated `interface`
/// value — and elements whose `recreate` refuses the format are reported as
/// warnings and left out; that is the port's stand-in for the reference's
/// unchecked cast (`RebuildFilterChain` fails silently on an element without
/// an `interface`, `:887`, and would fault on a bogus pointer).
fn build_chain(
    filters: &[i64],
    spec: PcmAudioSpec,
    event_tx: &mpsc::Sender<AudioEvent>,
    id: AudioInstanceId,
) -> Option<WaveFilterChain> {
    if filters.is_empty() {
        return None;
    }
    let (chain, skipped) = WaveFilterChain::build(filters, spec);
    for skip in skipped {
        report_event(
            event_tx,
            AudioStatusLevel::Warning,
            format!(
                "WaveSoundBuffer filter `{:#x}` of audio instance {} left the chain: {}",
                skip.id, id.0, skip.reason
            ),
        );
    }
    if chain.is_empty() {
        return None;
    }
    chain.reset();
    Some(chain)
}

/// Registers the tap of a sound that plays through a filter chain.
///
/// Unlike a static sound, whose decoded buffer is attached as it is
/// ([`register_sound_tap`]), a filtered sound is decoded by the chain's own
/// decoder, which publishes the filtered frames it renders
/// ([`PcmTapFeed::push_at`]): the tap then reads exactly the post-filter PCM
/// the reference's visualization ring holds
/// (`sound/win32/WaveImpl.cpp:2507-2510`).
fn register_filtered_tap(
    pcm_tap: &PcmTap,
    id: AudioInstanceId,
    sound: &PreparedSound,
    paused: bool,
) -> Option<PcmTapFeed> {
    let spec = prepared_sound_spec(sound)?;
    let feed = pcm_tap.register(id, spec);
    if paused {
        feed.set_paused(true);
    }
    Some(feed)
}

fn report_stopped_sounds(
    backend: &mut KiraBackend,
    slots: &mut BTreeMap<AudioInstanceId, SoundSlot>,
    event_tx: &mpsc::Sender<AudioEvent>,
) {
    for id in backend.take_stopped_non_looping_ids(slots) {
        if let Some(slot) = slots.remove(&id)
            && let Some(tap) = &slot.tap
        {
            tap.stop();
        }
        let _ = event_tx.send(AudioEvent::PlaybackStopped { id });
    }
}

fn spawn_static_loader(
    control_tx: mpsc::Sender<ControlMessage>,
) -> io::Result<mpsc::Sender<LoaderMessage>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("krkr-audio-static-loader".to_string())
        .spawn(move || static_loader_worker(rx, control_tx))?;
    Ok(tx)
}

fn spawn_streaming_loader(
    control_tx: mpsc::Sender<ControlMessage>,
) -> io::Result<mpsc::Sender<LoaderMessage>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("krkr-audio-streaming-loader".to_string())
        .spawn(move || streaming_loader_worker(rx, control_tx))?;
    Ok(tx)
}

fn static_loader_worker(
    rx: mpsc::Receiver<LoaderMessage>,
    control_tx: mpsc::Sender<ControlMessage>,
) {
    let mut cache =
        StaticSoundCache::new(STATIC_CACHE_CAPACITY_BYTES, STATIC_CACHE_MAX_ENTRY_BYTES);
    while let Ok(message) = rx.recv() {
        match message {
            LoaderMessage::Load(request) => {
                if send_static_load_result(request, &mut cache, &control_tx).is_err() {
                    break;
                }
            }
            LoaderMessage::Shutdown => break,
        }
    }
}

fn streaming_loader_worker(
    rx: mpsc::Receiver<LoaderMessage>,
    control_tx: mpsc::Sender<ControlMessage>,
) {
    while let Ok(message) = rx.recv() {
        match message {
            LoaderMessage::Load(request) => {
                if send_streaming_load_result(request, &control_tx).is_err() {
                    break;
                }
            }
            LoaderMessage::Shutdown => break,
        }
    }
}

fn send_static_load_result(
    request: LoadRequest,
    cache: &mut StaticSoundCache,
    control_tx: &mpsc::Sender<ControlMessage>,
) -> Result<(), mpsc::SendError<ControlMessage>> {
    let provider_epoch = request.provider_epoch;
    let source = request.source.clone();
    match request.kind {
        LoadRequestKind::Play { id, generation, .. } => {
            let result = load_static_or_auto_sound(request, cache);
            control_tx.send(ControlMessage::Prepared(Box::new(PreparedAudio {
                source,
                provider_epoch,
                kind: PreparedAudioKind::Play {
                    id,
                    generation,
                    result: Box::new(result),
                },
            })))
        }
        LoadRequestKind::Preload => {
            let result = preload_static_sound(request, cache);
            control_tx.send(ControlMessage::Prepared(Box::new(PreparedAudio {
                source,
                provider_epoch,
                kind: PreparedAudioKind::Preload { result },
            })))
        }
    }
}

fn send_streaming_load_result(
    request: LoadRequest,
    control_tx: &mpsc::Sender<ControlMessage>,
) -> Result<(), mpsc::SendError<ControlMessage>> {
    let provider_epoch = request.provider_epoch;
    let source = request.source.clone();
    let LoadRequestKind::Play { id, generation, .. } = request.kind else {
        return Ok(());
    };
    let result = load_streaming_or_opus_sound(request);
    control_tx.send(ControlMessage::Prepared(Box::new(PreparedAudio {
        source,
        provider_epoch,
        kind: PreparedAudioKind::Play {
            id,
            generation,
            result: Box::new(result),
        },
    })))
}

fn load_static_sound(
    request: LoadRequest,
    cache: &mut StaticSoundCache,
) -> Result<StaticSoundData, AudioLoadFailure> {
    let storage = request.source.storage().to_string();
    let should_cache = request.load_policy == AudioLoadPolicy::StaticCached;
    let key = StaticCacheKey {
        provider_epoch: request.provider_epoch,
        provider_revision: request.provider_revision,
        storage: storage.clone(),
    };
    if should_cache && let Some(data) = cache.get(&key) {
        return Ok(data);
    }

    let stream = request
        .provider
        .open(request.source.storage())
        .map_err(|error| AudioLoadFailure {
            storage: storage.clone(),
            message: error.to_string(),
        })?;
    let data = match StaticSoundData::from_media_source(ResourceMediaSource::new(stream)) {
        Ok(data) => data,
        Err(error) => load_opus_static_sound(&request).map_err(|opus_error| AudioLoadFailure {
            storage: storage.clone(),
            message: format!("{error}; Opus fallback failed: {opus_error}"),
        })?,
    };
    if should_cache {
        cache.insert(key, data.clone());
    }
    Ok(data)
}

fn load_static_or_auto_sound(
    mut request: LoadRequest,
    cache: &mut StaticSoundCache,
) -> Result<PreparedSound, AudioLoadFailure> {
    match request.load_policy {
        AudioLoadPolicy::Auto => {
            if should_stream_auto_source(&request)? {
                load_streaming_or_opus_sound(request)
            } else {
                request.load_policy = AudioLoadPolicy::StaticCached;
                load_static_sound(request, cache).map(PreparedSound::Static)
            }
        }
        AudioLoadPolicy::StaticCached | AudioLoadPolicy::StaticUncached => {
            load_static_sound(request, cache).map(PreparedSound::Static)
        }
        AudioLoadPolicy::Streaming => load_streaming_or_opus_sound(request),
    }
}

fn preload_static_sound(
    request: LoadRequest,
    cache: &mut StaticSoundCache,
) -> Result<(), AudioLoadFailure> {
    let policy = match request.load_policy {
        AudioLoadPolicy::Auto => {
            if !should_preload_static(&request)? {
                return Ok(());
            }
            AudioLoadPolicy::StaticCached
        }
        AudioLoadPolicy::StaticCached => AudioLoadPolicy::StaticCached,
        AudioLoadPolicy::StaticUncached | AudioLoadPolicy::Streaming => return Ok(()),
    };
    let request = LoadRequest {
        load_policy: policy,
        ..request
    };
    load_static_sound(request, cache).map(|_| ())
}

fn should_stream_auto_source(request: &LoadRequest) -> Result<bool, AudioLoadFailure> {
    if is_likely_voice_storage(request.source.storage()) {
        return Ok(true);
    }
    let storage = request.source.storage().to_string();
    Ok(request
        .provider
        .byte_len(request.source.storage())
        .map_err(|error| AudioLoadFailure {
            storage: storage.clone(),
            message: error.to_string(),
        })?
        .is_some_and(|len| len > PRELOAD_MAX_SOURCE_BYTES))
}

fn should_preload_static(request: &LoadRequest) -> Result<bool, AudioLoadFailure> {
    let storage = request.source.storage().to_string();
    Ok(request
        .provider
        .byte_len(request.source.storage())
        .map_err(|error| AudioLoadFailure {
            storage: storage.clone(),
            message: error.to_string(),
        })?
        .is_none_or(|len| len <= PRELOAD_MAX_SOURCE_BYTES))
}

#[cfg(not(target_arch = "wasm32"))]
fn load_streaming_or_opus_sound(request: LoadRequest) -> Result<PreparedSound, AudioLoadFailure> {
    match load_streaming_sound(&request) {
        Ok(data) => Ok(PreparedSound::Streaming(data)),
        Err(stream_error) => load_opus_static_sound(&request)
            .map(PreparedSound::Static)
            .map_err(|_| stream_error),
    }
}

#[cfg(target_arch = "wasm32")]
fn load_streaming_or_opus_sound(request: LoadRequest) -> Result<PreparedSound, AudioLoadFailure> {
    Err(AudioLoadFailure {
        storage: request.source.storage().to_string(),
        message: "streaming audio is provided by the browser Web Audio adapter".to_string(),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn load_streaming_sound(
    request: &LoadRequest,
) -> Result<StreamingSoundData<FromFileError>, AudioLoadFailure> {
    let storage = request.source.storage().to_string();
    let stream = request
        .provider
        .open(request.source.storage())
        .map_err(|error| AudioLoadFailure {
            storage: storage.clone(),
            message: error.to_string(),
        })?;
    StreamingSoundData::from_media_source(ResourceMediaSource::new(stream)).map_err(|error| {
        AudioLoadFailure {
            storage,
            message: error.to_string(),
        }
    })
}

/// kira streaming decoder that pulls PCM chunks from a live channel fed by
/// an external decoder (movie soundtracks decoded by krkr-video). The video
/// container never enters the audio file loaders.
///
/// Every chunk — and every silence block emitted while the producer stalls — is
/// published to the instance's [`PcmTapFeed`] at the stream frame coordinate it
/// will be rendered at, the same frame space as `SoundHandle::position()`.
/// kira's decode queue runs up to 16 384 frames ahead of playback; tagging each
/// published frame with its own coordinate is what keeps a tap read at the
/// cursor aligned with what is audible.
#[cfg(not(target_arch = "wasm32"))]
struct ChannelPcmDecoder {
    spec: krkr_core::PcmAudioSpec,
    total_frames: usize,
    stream: Arc<Mutex<Box<dyn krkr_core::PcmStream>>>,
    tap: Option<PcmTapFeed>,
    /// Frames handed to kira so far: the stream frame the next emitted frame
    /// will be rendered at, and therefore the coordinate every published chunk
    /// is tagged with.
    emitted_frames: u64,
    /// Silence published while the producer stalls, sized like one emitted
    /// block, so published coordinates keep covering what kira renders.
    silence: Vec<f32>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ChannelPcmDecoder {
    fn new(source: PcmStreamSource, tap: Option<PcmTapFeed>) -> Self {
        let channels = source.spec.channels.max(1) as usize;
        Self {
            spec: source.spec,
            total_frames: source.total_frames as usize,
            stream: source.stream,
            tap,
            emitted_frames: 0,
            silence: vec![0.0; STALL_SILENCE_FRAMES * channels],
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl kira::sound::streaming::Decoder for ChannelPcmDecoder {
    type Error = FromFileError;

    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }

    fn num_frames(&self) -> usize {
        self.total_frames
    }

    fn decode(&mut self) -> Result<Vec<Frame>, FromFileError> {
        let channels = self.spec.channels.max(1) as usize;
        let chunk = self
            .stream
            .lock()
            .ok()
            .and_then(|mut stream| stream.next_chunk());
        let Some(chunk) = chunk else {
            // Producer closed (end of stream) or stalled: emit silence so
            // kira's decode loop never spins on empty chunks. The transport
            // still stops the sound once `num_frames` is reached. These are
            // frames kira really renders, so they are published like any other
            // chunk: a tap read over them must report silence, not audio that
            // belongs to a different coordinate.
            if let Some(tap) = &self.tap {
                tap.push_at(self.emitted_frames, &self.silence);
            }
            self.emitted_frames = self
                .emitted_frames
                .saturating_add(STALL_SILENCE_FRAMES as u64);
            return Ok(vec![Frame::ZERO; STALL_SILENCE_FRAMES]);
        };
        if let Some(tap) = &self.tap {
            tap.push_at(self.emitted_frames, &chunk.samples);
        }
        self.emitted_frames = self
            .emitted_frames
            .saturating_add((chunk.samples.len() / channels) as u64);
        Ok(match self.spec.channels {
            1 => chunk
                .samples
                .iter()
                .map(|sample| Frame::from_mono(*sample))
                .collect(),
            _ => chunk
                .samples
                .chunks_exact(2)
                .map(|pair| Frame::new(pair[0], pair[1]))
                .collect(),
        })
    }

    fn seek(&mut self, index: usize) -> Result<usize, FromFileError> {
        // Live streams cannot rewind; movie seeks re-feed the channel from
        // the decoder side instead. Published coordinates assume the linear
        // playback of the one stream this decoder was built for, so a real
        // rewind would have to restart the decoder and the tap together.
        Ok(index)
    }
}

/// kira streaming decoder that runs a `WaveSoundBuffer.filters` chain over an
/// already-decoded sound.
///
/// The decoded frames of a static load are the reference's decoder output;
/// this decoder hands the chain one unit at a time — `UpdateFilterChain`
/// before each unit, `sound/WaveIntf.cpp:933-947` — and yields the filtered
/// frames, so the chain sits at the same point of the pipeline as the
/// reference's `FilterOutput` (between the decoder and the buffer,
/// `sound/WaveIntf.cpp:898-905`).  A loop wrap only moves the frame cursor:
/// the reference's loop manager sits *below* the filters, so the filter state
/// runs on across the wrap.
///
/// Every unit is published to the instance's [`PcmTapFeed`] at the stream
/// frame it will be rendered at, so the tap — and therefore `getVisBuffer` —
/// holds the post-filter PCM, which is where the reference copies its
/// visualization ring from (`sound/win32/WaveImpl.cpp:2507-2510`).
#[cfg(not(target_arch = "wasm32"))]
struct FilteredPcmDecoder {
    frames: Arc<[Frame]>,
    sample_rate: u32,
    /// First frame of the decoded buffer this sound covers (`StaticSoundData`'s
    /// slice; `0` for the whole buffer).
    start: usize,
    /// Frames the sound covers.
    total_frames: usize,
    /// Frame of the sound about to be decoded, relative to `start`.
    position: usize,
    chain: WaveFilterChain,
    tap: Option<PcmTapFeed>,
    unit: Vec<f32>,
}

/// Frames one chain unit carries.  The reference's unit is its L2 buffer unit
/// (~125 ms, `sound/win32/WaveImpl.cpp:2396`); this is the same order of
/// magnitude and keeps `Update()` — and therefore a script's parameter change —
/// taking effect while the buffer plays.
#[cfg(not(target_arch = "wasm32"))]
const FILTER_UNIT_FRAMES: usize = 4096;

#[cfg(not(target_arch = "wasm32"))]
impl FilteredPcmDecoder {
    fn new(
        sample_rate: u32,
        frames: Arc<[Frame]>,
        slice: Option<(usize, usize)>,
        chain: WaveFilterChain,
        tap: Option<PcmTapFeed>,
    ) -> Self {
        let start = slice.map(|(start, _)| start).unwrap_or(0);
        let total_frames = slice
            .map(|(start, end)| end.saturating_sub(start))
            .unwrap_or(frames.len());
        Self {
            frames,
            sample_rate,
            start,
            total_frames,
            position: 0,
            chain,
            tap,
            unit: Vec::with_capacity(FILTER_UNIT_FRAMES * 2),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl kira::sound::streaming::Decoder for FilteredPcmDecoder {
    type Error = FromFileError;

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn num_frames(&self) -> usize {
        self.total_frames
    }

    fn decode(&mut self) -> Result<Vec<Frame>, FromFileError> {
        if self.position >= self.total_frames || self.start >= self.frames.len() {
            // Past the end of the sound.  kira stops a non-looping sound at
            // `num_frames` and re-seeks a looping one, so this block is never
            // rendered; an empty chunk here would spin kira's decode loop
            // instead (`sound/streaming/sound/decode_scheduler.rs:160-182`),
            // which is why the stall block is silence, like
            // [`ChannelPcmDecoder`]'s.
            return Ok(vec![Frame::ZERO; FILTER_UNIT_FRAMES]);
        }
        let end = (self.position + FILTER_UNIT_FRAMES).min(self.total_frames);
        self.unit.clear();
        for frame in &self.frames[self.start + self.position..self.start + end] {
            self.unit.push(frame.left);
            self.unit.push(frame.right);
        }
        self.chain.update();
        self.chain.process(&mut self.unit);
        if let Some(tap) = &self.tap {
            tap.push_at(self.position as u64, &self.unit);
        }
        let frames = self
            .unit
            .chunks_exact(2)
            .map(|pair| Frame::new(pair[0], pair[1]))
            .collect();
        self.position = end;
        Ok(frames)
    }

    fn seek(&mut self, index: usize) -> Result<usize, FromFileError> {
        // Both directions are real here: the buffer is fully decoded, so a
        // loop wrap (kira seeks a looping sound back to its start) and a movie
        // -style forward seek both land where they ask.
        self.position = index.min(self.total_frames);
        Ok(self.position)
    }
}

#[cfg(feature = "opus")]
fn load_opus_static_sound(request: &LoadRequest) -> Result<StaticSoundData, String> {
    let stream = request
        .provider
        .open(request.source.storage())
        .map_err(|error| error.to_string())?;
    let media_source = Box::new(ResourceMediaSource::new(stream));
    let mut format = symphonia::default::get_probe()
        .format(
            &Default::default(),
            MediaSourceStream::new(media_source, Default::default()),
            &Default::default(),
            &Default::default(),
        )
        .map_err(|error| error.to_string())?
        .format;
    let track = format
        .default_track()
        .ok_or_else(|| "audio stream has no default track".to_string())?;
    if track.codec_params.codec != CODEC_TYPE_OPUS {
        return Err("audio stream is not Ogg Opus".to_string());
    }
    let track_id = track.id;
    let channels = match track.codec_params.channels.map(|channels| channels.count()) {
        Some(1) => OpusChannels::Mono,
        Some(2) => OpusChannels::Stereo,
        Some(count) => return Err(format!("unsupported Opus channel count {count}")),
        None => return Err("Opus stream has no channel layout".to_string()),
    };
    let mut decoder =
        OpusDecoder::new(OpusSampleRate::Hz48000, channels).map_err(|error| error.to_string())?;
    let channel_count = if channels.is_mono() { 1 } else { 2 };
    let mut frames = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(error) => return Err(error.to_string()),
        };
        if packet.track_id() != track_id || packet.data.is_empty() {
            continue;
        }
        let mut samples = vec![0.0_f32; 5760 * channel_count];
        let decoded = decoder
            .decode_float(
                Some(
                    OpusPacket::try_from(packet.data.as_ref())
                        .map_err(|error| error.to_string())?,
                ),
                MutSignals::try_from(&mut samples).map_err(|error| error.to_string())?,
                false,
            )
            .map_err(|error| error.to_string())?;
        let start = (packet.trim_start as usize).min(decoded);
        let end = decoded.saturating_sub(packet.trim_end as usize).max(start);
        for frame in samples[start * channel_count..end * channel_count].chunks_exact(channel_count)
        {
            frames.push(if channel_count == 1 {
                Frame::from_mono(frame[0])
            } else {
                Frame::new(frame[0], frame[1])
            });
        }
    }
    Ok(StaticSoundData {
        sample_rate: 48_000,
        frames: frames.into(),
        settings: kira::sound::static_sound::StaticSoundSettings::default(),
        slice: None,
    })
}

#[cfg(not(feature = "opus"))]
fn load_opus_static_sound(_request: &LoadRequest) -> Result<StaticSoundData, String> {
    Err("Opus decoding is disabled; enable the `opus` feature for native builds".to_string())
}

fn resolve_play_policy(policy: AudioLoadPolicy, bus: AudioBus, looping: bool) -> AudioLoadPolicy {
    match policy {
        AudioLoadPolicy::Auto => {
            if bus == AudioBus::Bgm || looping {
                AudioLoadPolicy::Streaming
            } else {
                AudioLoadPolicy::Auto
            }
        }
        other => other,
    }
}

fn is_likely_voice_storage(storage: &str) -> bool {
    storage
        .replace('\\', "/")
        .split('/')
        .any(|part| part.to_ascii_lowercase().contains("voice"))
}

impl KiraBackend {
    fn new() -> Result<Self, AudioError> {
        let mut manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
            .map_err(|error| AudioError::BackendUnavailable(error.to_string()))?;
        let bgm_track = manager
            .add_sub_track(TrackBuilder::new())
            .map_err(|error| AudioError::BackendUnavailable(error.to_string()))?;
        let se_track = manager
            .add_sub_track(TrackBuilder::new())
            .map_err(|error| AudioError::BackendUnavailable(error.to_string()))?;

        Ok(Self {
            manager,
            bgm_track,
            se_track,
            handles: BTreeMap::new(),
        })
    }

    fn play_prepared(
        &mut self,
        request: PlayRequest,
        sound: PreparedSound,
    ) -> Result<(), AudioError> {
        self.stop_id(request.id, Tween::default());
        let db = linear_volume_to_decibels(request.volume);
        let mut handle = match sound {
            PreparedSound::Static(data) => {
                #[cfg(not(target_arch = "wasm32"))]
                match request.chain {
                    Some(chain) => {
                        // The chain runs inside the playback: the decoder below
                        // hands it every unit before kira renders it, with the
                        // reference's `UpdateFilterChain` cadence
                        // (`sound/WaveIntf.cpp:933-947`, called from
                        // `FillL2Buffer`, `sound/win32/WaveImpl.cpp:2396`).
                        let decoder = FilteredPcmDecoder::new(
                            data.sample_rate,
                            Arc::clone(&data.frames),
                            data.slice,
                            chain,
                            request.tap.clone(),
                        );
                        let mut streaming = StreamingSoundData::from_decoder(decoder).volume(db);
                        if request.looping {
                            streaming = streaming.loop_region(..);
                        }
                        let handle = match request.bus {
                            AudioBus::Master => self.manager.play(streaming),
                            AudioBus::Bgm => self.bgm_track.play(streaming),
                            AudioBus::SoundEffect => self.se_track.play(streaming),
                        }
                        .map_err(|error| AudioError::PlaybackFailed {
                            storage: request.storage.clone(),
                            message: error.to_string(),
                        })?;
                        PlayingSound::Streaming {
                            bus: request.bus,
                            handle,
                        }
                    }
                    None => {
                        let mut data = data.volume(db);
                        if request.looping {
                            data = data.loop_region(..);
                        }
                        let handle = match request.bus {
                            AudioBus::Master => self.manager.play(data),
                            AudioBus::Bgm => self.bgm_track.play(data),
                            AudioBus::SoundEffect => self.se_track.play(data),
                        }
                        .map_err(|error| AudioError::PlaybackFailed {
                            storage: request.storage.clone(),
                            message: error.to_string(),
                        })?;
                        PlayingSound::Static {
                            bus: request.bus,
                            handle,
                        }
                    }
                }
                // The browser adapter owns the sample path, so a chain cannot
                // run there (the same reason `play_pcm_stream` is unavailable):
                // `dispatch_play_load` still routes a filtered instance
                // statically, and this is the unfiltered playback of it.
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = request.chain;
                    let mut data = data.volume(db);
                    if request.looping {
                        data = data.loop_region(..);
                    }
                    let handle = match request.bus {
                        AudioBus::Master => self.manager.play(data),
                        AudioBus::Bgm => self.bgm_track.play(data),
                        AudioBus::SoundEffect => self.se_track.play(data),
                    }
                    .map_err(|error| AudioError::PlaybackFailed {
                        storage: request.storage.clone(),
                        message: error.to_string(),
                    })?;
                    PlayingSound::Static {
                        bus: request.bus,
                        handle,
                    }
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            PreparedSound::Streaming(data) => {
                let mut data = data.volume(db);
                if request.looping {
                    data = data.loop_region(..);
                }
                let handle = match request.bus {
                    AudioBus::Master => self.manager.play(data),
                    AudioBus::Bgm => self.bgm_track.play(data),
                    AudioBus::SoundEffect => self.se_track.play(data),
                }
                .map_err(|error| AudioError::PlaybackFailed {
                    storage: request.storage.clone(),
                    message: error.to_string(),
                })?;
                PlayingSound::Streaming {
                    bus: request.bus,
                    handle,
                }
            }
        };
        if request.paused {
            handle.pause(Tween::default());
        }
        self.handles.insert(request.id, handle);
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn play_pcm_stream(
        &mut self,
        id: AudioInstanceId,
        bus: AudioBus,
        source: PcmStreamSource,
        volume: f32,
        tap: PcmTapFeed,
    ) -> Result<(), AudioError> {
        self.stop_id(id, Tween::default());
        let data =
            StreamingSoundData::from_decoder(ChannelPcmDecoder::new(source, Some(tap.clone())))
                .volume(linear_volume_to_decibels(volume));
        let handle = match bus {
            AudioBus::Master => self.manager.play(data),
            AudioBus::Bgm => self.bgm_track.play(data),
            AudioBus::SoundEffect => self.se_track.play(data),
        }
        .map_err(|error| AudioError::PlaybackFailed {
            storage: "pcm-stream".to_string(),
            message: error.to_string(),
        })?;
        self.handles
            .insert(id, PlayingSound::Streaming { bus, handle });
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    fn play_pcm_stream(
        &mut self,
        _id: AudioInstanceId,
        _bus: AudioBus,
        _source: PcmStreamSource,
        _volume: f32,
        _tap: PcmTapFeed,
    ) -> Result<(), AudioError> {
        Err(AudioError::CommandFailed(
            "PCM streams require the browser Web Audio adapter".to_string(),
        ))
    }

    /// Advances every playing instance's PCM tap to the source position kira
    /// last rendered. kira exposes this as an `f64` in seconds refreshed once
    /// per render batch, so the tap converts it to an exact frame index with
    /// the instance's own sample rate.
    fn sync_tap_positions(&mut self, slots: &BTreeMap<AudioInstanceId, SoundSlot>) {
        for (id, slot) in slots {
            let Some(tap) = &slot.tap else {
                continue;
            };
            let Some(handle) = self.handles.get(id) else {
                continue;
            };
            let spec = tap.spec();
            if spec.sample_rate == 0 {
                continue;
            }
            let seconds = handle.position().max(0.0);
            tap.set_source_position((seconds * spec.sample_rate as f64).round() as u64);
        }
    }

    fn stop_id(&mut self, id: AudioInstanceId, tween: Tween) {
        if let Some(mut handle) = self.handles.remove(&id) {
            handle.stop(tween);
        }
    }

    fn take_stopped_non_looping_ids(
        &mut self,
        slots: &BTreeMap<AudioInstanceId, SoundSlot>,
    ) -> Vec<AudioInstanceId> {
        let stopped = self
            .handles
            .iter()
            .filter_map(|(id, handle)| {
                let slot = slots.get(id)?;
                (!slot.looping && handle.state() == PlaybackState::Stopped).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in &stopped {
            self.handles.remove(id);
        }
        stopped
    }

    fn set_volume(&mut self, id: AudioInstanceId, volume: f32, tween: Tween) {
        if let Some(handle) = self.handles.get_mut(&id) {
            handle.set_volume(linear_volume_to_decibels(volume), tween);
        }
    }

    fn pause(&mut self, id: AudioInstanceId, tween: Tween) {
        if let Some(handle) = self.handles.get_mut(&id) {
            handle.pause(tween);
        }
    }

    fn resume(&mut self, id: AudioInstanceId, tween: Tween) {
        if let Some(handle) = self.handles.get_mut(&id) {
            handle.resume(tween);
        }
    }

    fn stop_bus(&mut self, bus: AudioBus, tween: Tween) {
        for handle in self.handles.values_mut() {
            if handle.bus() == bus {
                handle.stop(tween);
            }
        }
        self.handles.retain(|_, handle| handle.bus() != bus);
    }

    fn stop_all(&mut self, tween: Tween) {
        for handle in self.handles.values_mut() {
            handle.stop(tween);
        }
        self.handles.clear();
    }

    fn set_bus_volume(&mut self, bus: AudioBus, volume: f32, tween: Tween) {
        match bus {
            AudioBus::Master => self
                .manager
                .main_track()
                .set_volume(linear_volume_to_decibels(volume), tween),
            AudioBus::Bgm => self
                .bgm_track
                .set_volume(linear_volume_to_decibels(volume), tween),
            AudioBus::SoundEffect => self
                .se_track
                .set_volume(linear_volume_to_decibels(volume), tween),
        }
    }
}

impl PlayingSound {
    fn bus(&self) -> AudioBus {
        match self {
            Self::Static { bus, .. } => *bus,
            #[cfg(not(target_arch = "wasm32"))]
            Self::Streaming { bus, .. } => *bus,
        }
    }

    /// Source position kira last rendered, in seconds.
    fn position(&self) -> f64 {
        match self {
            Self::Static { handle, .. } => handle.position(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Streaming { handle, .. } => handle.position(),
        }
    }

    fn stop(&mut self, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.stop(tween),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Streaming { handle, .. } => handle.stop(tween),
        }
    }

    fn state(&self) -> PlaybackState {
        match self {
            Self::Static { handle, .. } => handle.state(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Streaming { handle, .. } => handle.state(),
        }
    }

    fn set_volume(&mut self, volume: Decibels, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.set_volume(volume, tween),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Streaming { handle, .. } => handle.set_volume(volume, tween),
        }
    }

    fn pause(&mut self, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.pause(tween),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Streaming { handle, .. } => handle.pause(tween),
        }
    }

    fn resume(&mut self, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.resume(tween),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Streaming { handle, .. } => handle.resume(tween),
        }
    }
}

impl StaticSoundCache {
    fn new(capacity_bytes: usize, max_entry_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            bytes: 0,
            capacity_bytes,
            max_entry_bytes,
        }
    }

    fn get(&mut self, key: &StaticCacheKey) -> Option<StaticSoundData> {
        let data = self.entries.get(key)?.data.clone();
        self.touch(key.clone());
        Some(data)
    }

    fn insert(&mut self, key: StaticCacheKey, data: StaticSoundData) {
        let bytes = static_sound_data_bytes(&data);
        if bytes > self.max_entry_bytes || bytes > self.capacity_bytes {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries
            .insert(key.clone(), StaticCacheEntry { data, bytes });
        self.touch(key);
        self.evict_to_capacity();
    }

    fn touch(&mut self, key: StaticCacheKey) {
        self.lru.retain(|item| item != &key);
        self.lru.push_back(key);
    }

    fn evict_to_capacity(&mut self) {
        while self.bytes > self.capacity_bytes {
            let Some(key) = self.lru.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

impl ResourceMediaSource {
    fn new(mut stream: Box<dyn ResourceStream>) -> Self {
        let current = stream.stream_position().ok();
        let byte_len = stream.seek(SeekFrom::End(0)).ok();
        if let Some(position) = current {
            let _ = stream.seek(SeekFrom::Start(position));
        }
        Self {
            stream: Mutex::new(stream),
            byte_len,
        }
    }
}

impl Read for ResourceMediaSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream
            .get_mut()
            .map_err(|_| io::Error::other("audio resource stream lock poisoned"))?
            .read(buffer)
    }
}

impl Seek for ResourceMediaSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.stream
            .get_mut()
            .map_err(|_| io::Error::other("audio resource stream lock poisoned"))?
            .seek(position)
    }
}

impl MediaSource for ResourceMediaSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

fn static_sound_data_bytes(data: &StaticSoundData) -> usize {
    std::mem::size_of_val(data.frames.as_ref())
}

fn report_event(event_tx: &mpsc::Sender<AudioEvent>, level: AudioStatusLevel, message: String) {
    let _ = event_tx.send(AudioEvent::Status(AudioStatusEvent { level, message }));
}

fn tween(seconds: f32) -> Tween {
    Tween {
        duration: Duration::from_secs_f32(seconds.max(0.0)),
        ..Tween::default()
    }
}

fn linear_volume_to_decibels(volume: f32) -> Decibels {
    let volume = volume.clamp(0.0, 1.0);
    if volume <= 0.0 {
        Decibels::SILENCE
    } else {
        Decibels(20.0 * volume.log10())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The process-wide handle the plugin-facing readback uses: an
    /// `AudioSystem` publishes its tap's registry at construction
    /// ([`active_pcm_tap`]), and the handle dies with the system that owns it
    /// (the slot holds a weak reference), so a reader can never address a dead
    /// sink's ring buffers.
    #[test]
    fn the_active_tap_is_the_newest_systems_registry_and_dies_with_it() {
        let id = AudioInstanceId(4242);
        {
            let system = AudioSystem::new();
            let spec = PcmAudioSpec {
                sample_rate: 44_100,
                channels: 2,
            };
            let feed = system.pcm_tap().register(id, spec);
            feed.attach_decoded(
                (0..8)
                    .map(|index| Frame::new(index as f32, index as f32))
                    .collect::<Vec<_>>()
                    .into(),
                false,
            );
            let published = active_pcm_tap().expect("the system published its tap");
            assert!(
                published.contains(id),
                "the published handle is this system's registry"
            );
        }
        // The system is gone: whoever reads next finds no registry (or another
        // system's), never this one's retired instance.
        assert!(
            active_pcm_tap().is_none_or(|tap| !tap.contains(id)),
            "a dropped audio system must not stay reachable"
        );
    }

    #[test]
    fn converts_linear_volume_to_decibels() {
        assert_eq!(linear_volume_to_decibels(1.0), Decibels::IDENTITY);
        assert_eq!(linear_volume_to_decibels(0.0), Decibels::SILENCE);
        assert!(linear_volume_to_decibels(0.5).0 < 0.0);
    }

    #[test]
    fn auto_policy_streams_looping_or_bgm_audio() {
        assert_eq!(
            resolve_play_policy(AudioLoadPolicy::Auto, AudioBus::Bgm, false),
            AudioLoadPolicy::Streaming
        );
        assert_eq!(
            resolve_play_policy(AudioLoadPolicy::Auto, AudioBus::SoundEffect, true),
            AudioLoadPolicy::Streaming
        );
        assert_eq!(
            resolve_play_policy(AudioLoadPolicy::Auto, AudioBus::SoundEffect, false),
            AudioLoadPolicy::Auto
        );
    }

    #[test]
    fn auto_policy_detects_voice_storage_names() {
        assert!(is_likely_voice_storage("voice/hero_001.ogg"));
        assert!(is_likely_voice_storage("sound\\VOICE_A.ogg"));
        assert!(!is_likely_voice_storage("sound/07.click.ogg"));
    }

    /// Pins the tap contract of both prepared kinds: a static sound hands over
    /// its decoded buffer and is readable at the reported position, while a
    /// streaming sound registers no instance at all, so readers see `None`.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn only_static_sounds_register_a_tap() {
        struct StubStreamingDecoder;

        impl kira::sound::streaming::Decoder for StubStreamingDecoder {
            type Error = FromFileError;

            fn sample_rate(&self) -> u32 {
                48_000
            }

            fn num_frames(&self) -> usize {
                0
            }

            fn decode(&mut self) -> Result<Vec<Frame>, FromFileError> {
                Ok(Vec::new())
            }

            fn seek(&mut self, index: usize) -> Result<usize, FromFileError> {
                Ok(index)
            }
        }

        let tap = PcmTap::new(64);
        let id = AudioInstanceId(77);

        let streaming =
            PreparedSound::Streaming(StreamingSoundData::from_decoder(StubStreamingDecoder));
        assert!(register_sound_tap(&tap, id, &streaming, false, false).is_none());
        assert!(!tap.contains(id));
        assert!(tap.read(id, PcmTapWindow::ahead(8)).is_none());
        assert!(tap.state(id).is_none());
        assert!(tap.cursor(id).is_none());

        let frames: Arc<[Frame]> = (0..16)
            .map(|index| Frame::new(index as f32, -(index as f32)))
            .collect::<Vec<_>>()
            .into();
        let static_sound = PreparedSound::Static(StaticSoundData {
            sample_rate: 48_000,
            frames,
            settings: kira::sound::static_sound::StaticSoundSettings::default(),
            slice: None,
        });
        let feed = register_sound_tap(&tap, id, &static_sound, false, false).expect("static tap");
        assert_eq!(
            tap.spec(id),
            Some(PcmAudioSpec {
                sample_rate: 48_000,
                channels: 2
            })
        );
        feed.set_source_position(8);
        let snapshot = tap.read(id, PcmTapWindow::recent(4)).expect("snapshot");
        assert_eq!(snapshot.first_frame, 4);
        assert_eq!(snapshot.available_frames, 4);
        assert_eq!(
            snapshot.frames,
            [4.0, -4.0, 5.0, -5.0, 6.0, -6.0, 7.0, -7.0]
        );

        // The slot is retired with the sound.
        feed.stop();
        assert_eq!(tap.state(id), Some(PcmTapState::Stopped));
    }

    /// Pins the producer half of the live-PCM tap contract: the decoder tags
    /// every frame it hands to kira with its stream coordinate — chunk samples
    /// and the silence blocks it emits while the producer stalls alike — so the
    /// tap's coordinates stay in `SoundHandle::position()`'s frame space. A
    /// regression here (not advancing on the stall path, or counting interleaved
    /// samples instead of frames) would silently reintroduce the round-1
    /// misalignment.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_live_pcm_decoder_publishes_coordinates_through_a_stall() {
        use kira::sound::streaming::Decoder as _;
        use krkr_core::{PcmAudioChunk, PcmStream, PcmStreamSource};

        struct ScriptedStream {
            step: usize,
        }

        impl PcmStream for ScriptedStream {
            fn next_chunk(&mut self) -> Option<PcmAudioChunk> {
                let step = self.step;
                self.step += 1;
                let samples: Vec<f32> = match step {
                    0 => (0..8)
                        .flat_map(|index| [index as f32, index as f32 + 1000.0])
                        .collect(),
                    1 => return None,
                    _ => (0..8)
                        .flat_map(|index| [100.0 + index as f32, 1100.0 + index as f32])
                        .collect(),
                };
                Some(PcmAudioChunk {
                    pts_ms: 0,
                    samples: Arc::from(samples),
                })
            }
        }

        let tap = PcmTap::new(16_384);
        let id = AudioInstanceId(88);
        let spec = PcmAudioSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        let source = PcmStreamSource::new(spec, 0, Box::new(ScriptedStream { step: 0 }));
        let feed = tap.register(id, spec);
        let mut decoder = ChannelPcmDecoder::new(source, Some(feed.clone()));

        // An unread tap ignores publishes, so arm it first.
        let _ = tap.read(id, PcmTapWindow::ahead(0));

        assert_eq!(decoder.decode().expect("first chunk").len(), 8);
        let snapshot = tap.read(id, PcmTapWindow::ahead(8)).expect("snapshot");
        assert_eq!(snapshot.cursor, 0);
        assert_eq!(snapshot.available_frames, 8);
        assert_eq!(
            snapshot.frames,
            (0..8)
                .flat_map(|index| [index as f32, index as f32 + 1000.0])
                .collect::<Vec<f32>>()
        );

        // Stall: the silence kira renders is published at its own coordinate, so
        // the frames of the following chunk do not slide forward.
        assert_eq!(
            decoder.decode().expect("stall silence").len(),
            STALL_SILENCE_FRAMES
        );
        feed.set_source_position(8);
        let snapshot = tap.read(id, PcmTapWindow::ahead(2_000)).expect("snapshot");
        assert_eq!(snapshot.available_frames, 2_000);
        assert!(snapshot.frames.iter().all(|sample| *sample == 0.0));

        let resumed_at = 8 + STALL_SILENCE_FRAMES as u64;
        assert_eq!(decoder.decode().expect("resumed chunk").len(), 8);
        feed.set_source_position(resumed_at);
        let snapshot = tap.read(id, PcmTapWindow::ahead(8)).expect("snapshot");
        assert_eq!(snapshot.first_frame, resumed_at);
        assert_eq!(snapshot.available_frames, 8);
        assert_eq!(
            snapshot.frames,
            (0..8)
                .flat_map(|index| [100.0 + index as f32, 1100.0 + index as f32])
                .collect::<Vec<f32>>()
        );
    }

    /// The `WaveSoundBuffer.filters` chain runs inside the sample path: the
    /// decoder hands every unit to the chain in array order with the sound's
    /// channel count, `Update()` runs before each unit and both the frames kira
    /// renders and the PCM tap hold post-filter samples — the reference's
    /// cadence (`UpdateFilterChain` before each decoded unit,
    /// `sound/WaveIntf.cpp:933-947`) at the reference's point in the pipeline
    /// (the visualization ring is filled from the post-filter L2 unit,
    /// `sound/win32/WaveImpl.cpp:2507-2510`).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_filter_chain_runs_over_the_rendered_samples_and_the_tap() {
        use kira::sound::streaming::Decoder as _;
        use std::sync::Mutex;

        #[derive(Default)]
        struct HalfGain {
            calls: Mutex<Vec<String>>,
            gains: Mutex<f32>,
        }

        impl HalfGain {
            fn record(&self, entry: &str) {
                self.calls.lock().expect("calls").push(entry.to_string());
            }

            fn calls(&self) -> Vec<String> {
                self.calls.lock().expect("calls").clone()
            }
        }

        impl WaveFilter for HalfGain {
            fn recreate(&self, spec: PcmAudioSpec) -> Result<PcmAudioSpec, String> {
                self.record(&format!("recreate:{}ch", spec.channels));
                Ok(spec)
            }

            fn clear(&self) {
                self.record("clear");
            }

            fn update(&self) {
                self.record("update");
            }

            fn reset(&self) {
                self.record("reset");
            }

            fn process(&self, frames: &mut [f32]) {
                self.record("process");
                let gain = *self.gains.lock().expect("gains");
                for sample in frames.iter_mut() {
                    *sample *= gain;
                }
            }
        }

        let filter = Arc::new(HalfGain {
            calls: Mutex::new(Vec::new()),
            gains: Mutex::new(0.5),
        });
        let filter_id = register_wave_filter(Arc::clone(&filter) as Arc<dyn WaveFilter>);
        let spec = PcmAudioSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        let frames: Arc<[Frame]> = (0..FILTER_UNIT_FRAMES + 4)
            .map(|index| Frame::new(index as f32, -(index as f32)))
            .collect::<Vec<_>>()
            .into();

        let (chain, skipped) = WaveFilterChain::build(&[filter_id.raw()], spec);
        assert!(
            skipped.is_empty(),
            "the filter joins the chain: {skipped:?}"
        );
        chain.reset();

        let tap = PcmTap::new(FILTER_UNIT_FRAMES as u32 + 64);
        let id = AudioInstanceId(99);
        let feed = tap.register(id, spec);
        // An unread tap ignores publishes, so arm it first.
        let _ = tap.read(id, PcmTapWindow::ahead(0));

        let mut decoder =
            FilteredPcmDecoder::new(48_000, Arc::clone(&frames), None, chain, Some(feed.clone()));
        assert_eq!(decoder.num_frames(), frames.len());
        assert_eq!(
            filter.calls(),
            vec!["recreate:2ch".to_string(), "reset".to_string()],
            "the chain is built and reset before playback"
        );

        let first = decoder.decode().expect("first unit");
        assert_eq!(first.len(), FILTER_UNIT_FRAMES);
        assert_eq!(
            filter.calls(),
            vec![
                "recreate:2ch".to_string(),
                "reset".to_string(),
                "update".to_string(),
                "process".to_string(),
            ],
            "update runs before the unit is processed"
        );
        // The rendered frames are the chain's output: half of the source.
        assert_eq!(first[0].left, 0.0);
        assert_eq!(first[1].left, 0.5);
        assert_eq!(first[1].right, -0.5);

        // The tap reads the same post-filter samples, at the stream coordinate
        // the unit will be rendered at.
        feed.set_source_position(0);
        let snapshot = tap.read(id, PcmTapWindow::ahead(2)).expect("snapshot");
        assert_eq!(snapshot.first_frame, 0);
        assert_eq!(snapshot.available_frames, 2);
        assert_eq!(snapshot.frames, [0.0, 0.0, 0.5, -0.5]);

        // A filter edited between units takes effect at the next unit: the
        // chain is not rebuilt, only `update` runs.
        *filter.gains.lock().expect("gains") = 0.25;
        let second = decoder.decode().expect("second unit");
        assert_eq!(second.len(), 4, "the tail of the buffer");
        assert_eq!(
            filter.calls(),
            vec![
                "recreate:2ch".to_string(),
                "reset".to_string(),
                "update".to_string(),
                "process".to_string(),
                "update".to_string(),
                "process".to_string(),
            ]
        );
        assert_eq!(second[0].left, (FILTER_UNIT_FRAMES as f32) * 0.25);

        feed.stop();
        assert!(unregister_wave_filter(filter_id));
    }

    /// A chain's registry link survives an element whose `interface` value
    /// resolves to nothing: the remaining elements still process (the port's
    /// stand-in for the reference's unchecked cast, which would fault).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_chain_with_an_unresolvable_element_still_processes_the_rest() {
        use kira::sound::streaming::Decoder as _;

        struct Doubler;

        impl WaveFilter for Doubler {
            fn recreate(&self, spec: PcmAudioSpec) -> Result<PcmAudioSpec, String> {
                Ok(spec)
            }
            fn clear(&self) {}
            fn update(&self) {}
            fn reset(&self) {}
            fn process(&self, frames: &mut [f32]) {
                for sample in frames.iter_mut() {
                    *sample *= 2.0;
                }
            }
        }

        let id = register_wave_filter(Arc::new(Doubler));
        let spec = PcmAudioSpec {
            sample_rate: 44_100,
            channels: 2,
        };
        let frames: Arc<[Frame]> = (0..4)
            .map(|index| Frame::new(index as f32, index as f32))
            .collect::<Vec<_>>()
            .into();
        let (chain, skipped) = WaveFilterChain::build(&[0xdead_beef, id.raw()], spec);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].id, 0xdead_beef);

        let mut decoder = FilteredPcmDecoder::new(44_100, frames, None, chain, None);
        let frames = decoder.decode().expect("unit");
        assert_eq!(frames[1].left, 2.0);
        assert_eq!(frames[3].left, 6.0);
        assert!(unregister_wave_filter(id));
    }

    /// Replacing an instance's chain clears the filters the old chain held
    /// (`ClearFilterChain`'s per-filter `Clear()`, `sound/WaveIntf.cpp:914-916`)
    /// and an identical list is left alone.
    #[test]
    fn replacing_a_chain_clears_the_filters_it_dropped() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static CLEARED: AtomicUsize = AtomicUsize::new(0);

        struct ClearCounter;

        impl WaveFilter for ClearCounter {
            fn recreate(&self, spec: PcmAudioSpec) -> Result<PcmAudioSpec, String> {
                Ok(spec)
            }
            fn clear(&self) {
                CLEARED.fetch_add(1, Ordering::SeqCst);
            }
            fn update(&self) {}
            fn reset(&self) {}
            fn process(&self, _frames: &mut [f32]) {}
        }

        let first = register_wave_filter(Arc::new(ClearCounter));
        let mut filters = BTreeMap::new();
        set_wave_filters(&mut filters, AudioInstanceId(5), vec![first.raw()]);
        assert_eq!(
            CLEARED.load(Ordering::SeqCst),
            0,
            "the first list clears nothing"
        );

        set_wave_filters(&mut filters, AudioInstanceId(5), vec![first.raw()]);
        assert_eq!(
            CLEARED.load(Ordering::SeqCst),
            0,
            "an unchanged list must not be rebuilt"
        );

        set_wave_filters(&mut filters, AudioInstanceId(5), Vec::new());
        assert_eq!(CLEARED.load(Ordering::SeqCst), 1);
        assert!(filters.get(&AudioInstanceId(5)).is_some_and(Vec::is_empty));
        unregister_wave_filter(first);
    }
}
