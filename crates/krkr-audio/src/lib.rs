use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    error::Error,
    fmt,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use kira::{
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Tween,
    sound::{
        FromFileError, PlaybackState,
        static_sound::{StaticSoundData, StaticSoundHandle},
        streaming::{StreamingSoundData, StreamingSoundHandle},
    },
    track::{TrackBuilder, TrackHandle},
};
use krkr_core::{
    AudioBus, AudioCommand, AudioInstanceId, AudioLoadPolicy, AudioSourceRef, ResourceProvider,
    ResourceStream,
};
use symphonia::core::io::MediaSource;

const STATIC_CACHE_CAPACITY_BYTES: usize = 64 * 1024 * 1024;
const STATIC_CACHE_MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;
const PRELOAD_MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioState {
    Stopped,
    Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioStatusLevel {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioStatusEvent {
    pub level: AudioStatusLevel,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioEvent {
    Status(AudioStatusEvent),
    PlaybackStopped { id: AudioInstanceId },
}

#[derive(Debug)]
pub enum AudioError {
    BackendUnavailable(String),
    WorkerUnavailable(String),
    CommandFailed(String),
    PlaybackFailed { storage: String, message: String },
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackendUnavailable(message) => {
                write!(formatter, "audio backend is unavailable: {message}")
            }
            Self::WorkerUnavailable(message) => {
                write!(formatter, "audio worker stopped: {message}")
            }
            Self::CommandFailed(message) => write!(formatter, "audio command failed: {message}"),
            Self::PlaybackFailed { storage, message } => {
                write!(formatter, "failed to play audio `{storage}`: {message}")
            }
        }
    }
}

impl Error for AudioError {}

pub struct AudioSystem {
    state: AudioState,
    control_tx: Option<mpsc::Sender<ControlMessage>>,
    event_rx: Option<mpsc::Receiver<AudioEvent>>,
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
}

enum PlayingSound {
    Static {
        bus: AudioBus,
        handle: StaticSoundHandle,
    },
    Streaming {
        bus: AudioBus,
        handle: StreamingSoundHandle<FromFileError>,
    },
}

enum PreparedSound {
    Static(StaticSoundData),
    Streaming(StreamingSoundData<FromFileError>),
}

enum ControlMessage {
    Command(AudioCommand),
    SetResourceProvider(Option<Arc<dyn ResourceProvider>>),
    Prepared(Box<PreparedAudio>),
    Shutdown,
}

enum LoaderMessage {
    Load(LoadRequest),
    Shutdown,
}

struct LoadRequest {
    source: AudioSourceRef,
    load_policy: AudioLoadPolicy,
    provider: Arc<dyn ResourceProvider>,
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
    pub const fn new() -> Self {
        Self {
            state: AudioState::Stopped,
            control_tx: None,
            event_rx: None,
        }
    }

    pub fn prepare(&mut self) -> Result<(), AudioError> {
        if self.control_tx.is_some() {
            self.state = AudioState::Ready;
            return Ok(());
        }

        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker_control_tx = control_tx.clone();
        thread::Builder::new()
            .name("krkr-audio-control".to_string())
            .spawn(move || audio_control_worker(control_rx, worker_control_tx, event_tx))
            .map_err(|error| AudioError::WorkerUnavailable(error.to_string()))?;

        self.control_tx = Some(control_tx);
        self.event_rx = Some(event_rx);
        self.state = AudioState::Ready;
        Ok(())
    }

    pub fn set_resource_provider(
        &mut self,
        provider: Option<Arc<dyn ResourceProvider>>,
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

fn audio_control_worker(
    rx: mpsc::Receiver<ControlMessage>,
    control_tx: mpsc::Sender<ControlMessage>,
    event_tx: mpsc::Sender<AudioEvent>,
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
                },
            ),
            Ok(ControlMessage::SetResourceProvider(next_provider)) => {
                backend.stop_all(Tween::default());
                slots.clear();
                provider = next_provider;
                provider_epoch = provider_epoch.saturating_add(1);
            }
            Ok(ControlMessage::Prepared(prepared)) => {
                if prepared.provider_epoch == provider_epoch {
                    handle_prepared_audio(*prepared, &mut backend, &mut slots, &event_tx);
                }
            }
            Ok(ControlMessage::Shutdown) => {
                backend.stop_all(Tween::default());
                let _ = static_tx.send(LoaderMessage::Shutdown);
                let _ = streaming_tx.send(LoaderMessage::Shutdown);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        report_stopped_sounds(&mut backend, &mut slots, &event_tx);
    }
}

struct ControlContext<'a> {
    backend: &'a mut KiraBackend,
    provider: Option<Arc<dyn ResourceProvider>>,
    provider_epoch: u64,
    next_generation: &'a mut u64,
    slots: &'a mut BTreeMap<AudioInstanceId, SoundSlot>,
    static_tx: &'a mpsc::Sender<LoaderMessage>,
    streaming_tx: &'a mpsc::Sender<LoaderMessage>,
    event_tx: &'a mpsc::Sender<AudioEvent>,
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
        AudioCommand::Stop { id, fade_seconds } => {
            context.slots.remove(&id);
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
            }
            context.backend.pause(id, tween(fade_seconds));
        }
        AudioCommand::Resume { id, fade_seconds } => {
            if let Some(slot) = context.slots.get_mut(&id) {
                slot.paused = false;
            }
            context.backend.resume(id, tween(fade_seconds));
        }
        AudioCommand::StopBus { bus, fade_seconds } => {
            let tween = tween(fade_seconds);
            context.backend.stop_bus(bus, tween);
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
    let effective_policy = resolve_play_policy(load_policy, bus, looping);
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
    event_tx: &mpsc::Sender<AudioEvent>,
) {
    match prepared.kind {
        PreparedAudioKind::Play {
            id,
            generation,
            result,
        } => {
            let Some(slot) = slots.get(&id) else {
                return;
            };
            if slot.generation != generation {
                return;
            }
            match *result {
                Ok(sound) => {
                    let request = PlayRequest {
                        id,
                        bus: slot.bus,
                        storage: prepared.source.storage,
                        looping: slot.looping,
                        volume: slot.volume,
                        paused: slot.paused,
                    };
                    if let Err(error) = backend.play_prepared(request, sound) {
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

fn report_stopped_sounds(
    backend: &mut KiraBackend,
    slots: &mut BTreeMap<AudioInstanceId, SoundSlot>,
    event_tx: &mpsc::Sender<AudioEvent>,
) {
    for id in backend.take_stopped_non_looping_ids(slots) {
        slots.remove(&id);
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
    let result = load_streaming_sound(request).map(PreparedSound::Streaming);
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
    let data =
        StaticSoundData::from_media_source(ResourceMediaSource::new(stream)).map_err(|error| {
            AudioLoadFailure {
                storage: storage.clone(),
                message: error.to_string(),
            }
        })?;
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
                load_streaming_sound(request).map(PreparedSound::Streaming)
            } else {
                request.load_policy = AudioLoadPolicy::StaticCached;
                load_static_sound(request, cache).map(PreparedSound::Static)
            }
        }
        AudioLoadPolicy::StaticCached | AudioLoadPolicy::StaticUncached => {
            load_static_sound(request, cache).map(PreparedSound::Static)
        }
        AudioLoadPolicy::Streaming => load_streaming_sound(request).map(PreparedSound::Streaming),
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

fn load_streaming_sound(
    request: LoadRequest,
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
            Self::Static { bus, .. } | Self::Streaming { bus, .. } => *bus,
        }
    }

    fn stop(&mut self, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.stop(tween),
            Self::Streaming { handle, .. } => handle.stop(tween),
        }
    }

    fn state(&self) -> PlaybackState {
        match self {
            Self::Static { handle, .. } => handle.state(),
            Self::Streaming { handle, .. } => handle.state(),
        }
    }

    fn set_volume(&mut self, volume: Decibels, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.set_volume(volume, tween),
            Self::Streaming { handle, .. } => handle.set_volume(volume, tween),
        }
    }

    fn pause(&mut self, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.pause(tween),
            Self::Streaming { handle, .. } => handle.pause(tween),
        }
    }

    fn resume(&mut self, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.resume(tween),
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
}
