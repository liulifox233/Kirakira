use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{self, Cursor, Read, Seek},
    sync::{Arc, Mutex},
};

pub mod media;
pub mod xp3;

pub use media::{FILE_MEDIA_NAME, StorageMediaProvider, is_valid_media_name, split_media_name};
pub use xp3::{
    Xp3ContentFilter, Xp3ContentFilterAction, Xp3ExtractionFilter, Xp3ExtractionFilterInfo,
    Xp3FilterContext, Xp3FilterRegistry,
};

pub trait ResourceStream: Read + Seek + Send {}

impl<T> ResourceStream for T where T: Read + Seek + Send {}

pub trait ResourceDataSource: Send + Sync {
    fn byte_len(&self) -> u64;

    fn as_bytes(&self) -> io::Result<Cow<'_, [u8]>>;

    fn to_arc_bytes(&self) -> io::Result<Arc<[u8]>> {
        match self.as_bytes()? {
            Cow::Borrowed(bytes) => Ok(Arc::from(bytes)),
            Cow::Owned(bytes) => Ok(Arc::from(bytes)),
        }
    }

    fn open_stream(&self) -> io::Result<Box<dyn ResourceStream>>;
}

#[derive(Clone)]
pub struct ResourceData {
    source: Arc<dyn ResourceDataSource>,
}

impl ResourceData {
    pub fn new(source: Arc<dyn ResourceDataSource>) -> Self {
        Self { source }
    }

    pub fn from_bytes(bytes: Arc<[u8]>) -> Self {
        Self::new(Arc::new(SharedBytesResourceData { bytes }))
    }

    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self::from_bytes(Arc::from(bytes))
    }

    pub fn byte_len(&self) -> u64 {
        self.source.byte_len()
    }

    pub fn as_bytes(&self) -> io::Result<Cow<'_, [u8]>> {
        self.source.as_bytes()
    }

    pub fn to_arc_bytes(&self) -> io::Result<Arc<[u8]>> {
        self.source.to_arc_bytes()
    }

    pub fn open_stream(&self) -> io::Result<Box<dyn ResourceStream>> {
        self.source.open_stream()
    }
}

struct SharedBytesResourceData {
    bytes: Arc<[u8]>,
}

impl ResourceDataSource for SharedBytesResourceData {
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn as_bytes(&self) -> io::Result<Cow<'_, [u8]>> {
        Ok(Cow::Borrowed(self.bytes.as_ref()))
    }

    fn to_arc_bytes(&self) -> io::Result<Arc<[u8]>> {
        Ok(Arc::clone(&self.bytes))
    }

    fn open_stream(&self) -> io::Result<Box<dyn ResourceStream>> {
        Ok(Box::new(Cursor::new(Arc::clone(&self.bytes))))
    }
}

/// Backend-neutral description of a KRKR font.  The core protocol carries
/// this value through draw and message-layer commands; font discovery and
/// rasterisation live in `krkr-font` (or a platform font backend).
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub face: String,
    pub height: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub angle: i32,
    pub face_is_file_name: bool,
    pub rasterizer: String,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            face: String::new(),
            height: 24.0,
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            angle: 0,
            face_is_file_name: false,
            rasterizer: String::new(),
        }
    }
}

impl FontSpec {
    pub fn resolved_height(&self) -> f32 {
        self.height.abs().max(1.0)
    }
}

/// Backend-neutral style applied to a text draw command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextStyle {
    pub color: [u8; 4],
    pub anti_alias: bool,
    pub shadow: Option<ShadowStyle>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: [255, 255, 255, 255],
            anti_alias: true,
            shadow: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowStyle {
    pub offset_x: i32,
    pub offset_y: i32,
    pub color: [u8; 4],
}

/// Canonical read-only package boundary used by every host backend.
pub trait StoragePort: Send + Sync {
    fn open(&self, path: &str) -> io::Result<Box<dyn ResourceStream>>;

    fn exists(&self, path: &str) -> bool;

    fn data(&self, path: &str) -> io::Result<ResourceData> {
        let mut stream = self.open(path)?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        Ok(ResourceData::from_vec(bytes))
    }

    fn byte_len(&self, path: &str) -> io::Result<Option<u64>> {
        let mut stream = self.open(path)?;
        let current = stream.stream_position().ok();
        let len = stream.seek(io::SeekFrom::End(0)).ok();
        if let Some(position) = current {
            let _ = stream.seek(io::SeekFrom::Start(position));
        }
        Ok(len)
    }

    fn revision(&self) -> u64 {
        0
    }

    /// Revision of the name-to-file layout as far as decoded graphics are
    /// concerned.
    ///
    /// KRKR keys its graphic cache on the storage name alone and only drops
    /// entries on `System.clearGraphicCache`, a compact event or an
    /// out-of-memory retry (`GraphicsLoaderIntf.cpp`); writing a file never
    /// touches it. `revision` still has to move on every write so the
    /// lookup and raw-byte caches stay coherent, so graphics track this
    /// separate counter, which only advances when the search path, archive
    /// set or catalogue changes.
    fn graphic_revision(&self) -> u64 {
        self.revision()
    }
}

/// Mutable project-storage capability consumed by the engine.
///
/// The engine only depends on this platform-neutral contract. Filesystem,
/// XP3, memory-overlay and browser-manifest implementations live in host
/// crates (currently `krkr-assets`); a mobile or remote host can provide a
/// different implementation without linking those backends into the engine.
pub trait ProjectStoragePort: StoragePort {
    fn storage_exists(&self, name: &str) -> bool {
        self.exists(name)
    }

    /// Checks the logical name without the resource loader's convenience
    /// extension probing. Script-facing `Storages.isExistentStorage` uses
    /// this exact operation so a stem cannot match an unrelated file type.
    fn storage_exists_exact(&self, name: &str) -> bool {
        self.exists(name)
    }

    fn is_directory(&self, name: &str) -> bool;

    fn list_directory(&self, name: &str) -> io::Result<Vec<String>>;

    /// Returns a native path only when the adapter has one. Virtual/archive
    /// stores return `None`; the string avoids leaking a platform `Path` into
    /// the engine/core boundary.
    fn placed_path(&self, name: &str) -> Option<String>;

    /// Returns the logical normalized name selected by lookup. Filesystem
    /// adapters may return the same value as `placed_path`; archive-aware
    /// adapters should return their `archive.xp3>entry` spelling.
    fn resolved_storage_name(&self, name: &str) -> Option<String> {
        self.placed_path(name)
    }

    fn read_binary_storage(&self, name: &str) -> io::Result<ResourceData>;

    /// Reads the bytes a graphic load resolves to.
    ///
    /// `TVPInternalLoadGraphic` suggests extensions at the graphics-loader
    /// layer, not inside storage lookup: an extensionless name is completed
    /// with the registered graphic handlers' extensions and the first
    /// `name + extension` that exists wins, while a name whose extension no
    /// handler serves is rejected outright (`GraphicsLoaderIntf.cpp:2307-2336`
    /// in the stock krkr2 tree; `krkrz/visual/GraphicsLoaderIntf.cpp:1452`,
    /// suggestion loop `:1478-1506`). An image load must therefore not complete
    /// a bare stem with a same-stem non-graphic sidecar -- 纸上的魔法使's
    /// `PageBreak` reaching `PageBreak.asd` instead of `PageBreak.png` is that
    /// divergence. Backends without a graphic-aware resolver keep the plain
    /// binary read.
    fn read_image_storage(&self, name: &str) -> io::Result<ResourceData> {
        self.read_binary_storage(name)
    }

    fn read_text_storage(&self, name: &str, configured_encoding: &str) -> io::Result<String>;

    /// Reads text with an optional KRKR offset mode. Adapters can preserve
    /// their native encoding hints while applying the mode before decoding.
    fn read_text_storage_mode(
        &self,
        name: &str,
        mode: &str,
        configured_encoding: &str,
    ) -> io::Result<String> {
        let _ = mode;
        self.read_text_storage(name, configured_encoding)
    }

    fn write_text_storage(&self, name: &str, mode: &str, text: &str) -> io::Result<()>;

    fn write_binary_storage(&self, name: &str, mode: &str, bytes: &[u8]) -> io::Result<()>;

    /// Creates the writable directory `name` on the backend's write root, for
    /// the script-facing `Storages.createDirectory` (`fstat.dll`).
    ///
    /// The reference resolves the storage name to its local path and calls
    /// Win32 `CreateDirectory(dir, NULL)` (`krkr2
    /// src/plugins/win32/fstat/Main.cpp:578-596`), so the answers this must
    /// reproduce are all `false` (which fstat reports as `0`):
    ///
    /// * an **already-existing** directory — `ERROR_ALREADY_EXISTS`; a
    ///   successful `createDirectory` is a *creation*, not "it is there now";
    /// * a **missing parent** — `CreateDirectory` creates only the final
    ///   component, so this is `ERROR_PATH_NOT_FOUND`;
    /// * a refusal from the filesystem (permissions, a read-only mount).
    ///
    /// The default refuses, which is what a backend without a writable
    /// filesystem (a browser host, a test double) must do.
    fn create_directory(&self, name: &str) -> io::Result<()> {
        let _ = name;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this storage backend does not create directories",
        ))
    }

    fn add_auto_path(&self, path: &str);

    fn remove_auto_path(&self, path: &str) -> bool;

    fn clear_archive_cache(&self) -> io::Result<()>;

    fn catalog_contains(&self, name: &str) -> bool;

    /// Load-time counterpart of [`Self::catalog_contains`]: whether a deferred
    /// asset can satisfy a read of `name` once storage extensions are
    /// suggested, the way KRKR's graphic/sound loaders do.
    fn catalog_contains_for_load(&self, name: &str) -> bool;

    /// Replaces deferred logical names for a new package while retaining
    /// resident memory files owned by this storage view.
    fn set_catalog_paths(&self, paths: &[String]);

    /// Mounts a resident virtual file in the project namespace. Unlike an
    /// externally fetched resource, these bytes stay available until they are
    /// replaced or the storage view is discarded.
    fn insert_memory(&self, path: &str, bytes: Vec<u8>);

    fn insert_external_memory(&self, path: &str, bytes: Vec<u8>);

    fn drain_memory_writes(&self) -> Vec<(String, Vec<u8>)>;

    /// The XP3 filter registry this backend's archives consult
    /// (`TVPSetXP3ArchiveExtractionFilter`/`TVPSetXP3ArchiveContentFilter`,
    /// `Kirikiroid2/src/core/base/XP3Archive.cpp:30-41`).
    ///
    /// A plugin installs its filter into this object *after* the project's
    /// archives were opened, and that is what the reference does too: the
    /// archive holds no copy of the callbacks, it reads the slots when it
    /// creates an entry stream and on every read. A backend whose archives are
    /// not read by `krkr-xp3` — or that has none — answers `None`, so a plugin
    /// can tell "no archives to filter here" from "installed".
    fn xp3_filter_registry(&self) -> Option<Arc<Xp3FilterRegistry>> {
        None
    }

    /// Registers a storage media (`TVPRegisterStorageMedia`,
    /// `StorageIntf.cpp:530-538`), the plugin-facing way to add a URI scheme:
    /// `psb://`, `lzfs://`, `proxy://`, `steam://`, `var://`, `zip://`.
    ///
    /// The default refuses the registration, which is what a backend without a
    /// media registry (a browser host, a test double) must do: media
    /// registration fails cleanly there instead of panicking.
    fn register_storage_media(&self, media: Arc<dyn StorageMediaProvider>) -> io::Result<()> {
        let _ = media;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this storage backend does not support media registration",
        ))
    }

    /// Unregisters a storage media (`TVPUnregisterStorageMedia`,
    /// `StorageIntf.cpp:535-538`). Returns whether a media was registered under
    /// `media_name`; the built-in `file` media can never be removed.
    fn unregister_storage_media(&self, media_name: &str) -> bool {
        let _ = media_name;
        false
    }

    /// Names of the registered media, sorted. Diagnostics and tests only; the
    /// built-in `file` media is implicit and never listed.
    fn storage_media_names(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Opaque identifier for a resource request that may complete after a frame.
///
/// The runtime deliberately uses a small polling protocol instead of an
/// `async fn` in the core API. Native hosts can complete requests from a worker
/// thread while browser hosts can feed completions produced by `fetch` without
/// making the TJS VM depend on a particular executor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AssetRequestId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AssetKind {
    Binary,
    Text,
    Image,
    Font,
    Media,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetRequest {
    pub id: AssetRequestId,
    pub path: String,
    pub kind: AssetKind,
}

impl AssetRequest {
    pub fn new(id: AssetRequestId, path: impl Into<String>, kind: AssetKind) -> Self {
        Self {
            id,
            path: path.into(),
            kind,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AssetEvent {
    Ready {
        id: AssetRequestId,
        path: String,
        kind: AssetKind,
        data: Arc<[u8]>,
    },
    Failed {
        id: AssetRequestId,
        path: String,
        kind: AssetKind,
        message: String,
    },
}

/// Platform-neutral asynchronous resource scheduler.
pub trait AssetScheduler {
    fn request(&mut self, path: &str, kind: AssetKind) -> AssetRequestId;

    fn poll(&mut self) -> Vec<AssetEvent>;

    /// Cancels delivery to a waiter. A backend may keep a shared network
    /// fetch alive for other waiters, but the cancelled request must never
    /// produce a completion event for the caller.
    fn cancel(&mut self, _id: AssetRequestId) -> bool {
        false
    }

    fn retry(&mut self, path: &str, kind: AssetKind) -> AssetRequestId {
        self.request(path, kind)
    }

    fn revision(&self) -> u64 {
        0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SaveRequestId(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub enum SaveEvent {
    Loaded {
        id: SaveRequestId,
        profile: String,
        key: String,
        data: Option<Arc<[u8]>>,
    },
    Saved {
        id: SaveRequestId,
        profile: String,
        key: String,
    },
    Failed {
        id: SaveRequestId,
        profile: String,
        key: String,
        message: String,
    },
}

/// Host-owned asynchronous profile storage.  Game packages remain read-only;
/// save implementations may use files, IndexedDB, Room or an iOS profile
/// database without exposing those APIs to the engine.
pub trait SaveStore {
    fn load(&mut self, profile: &str, key: &str) -> SaveRequestId;
    fn save(&mut self, profile: &str, key: &str, data: Arc<[u8]>) -> SaveRequestId;
    fn poll(&mut self) -> Vec<SaveEvent>;
}

/// Deterministic save backend for probes and host conformance tests.
#[derive(Clone, Debug, Default)]
pub struct MemorySaveStore {
    entries: BTreeMap<(String, String), Arc<[u8]>>,
    events: Vec<SaveEvent>,
    next_id: u64,
}

impl MemorySaveStore {
    pub fn insert(&mut self, profile: impl Into<String>, key: impl Into<String>, data: Arc<[u8]>) {
        self.entries.insert((profile.into(), key.into()), data);
    }
}

impl SaveStore for MemorySaveStore {
    fn load(&mut self, profile: &str, key: &str) -> SaveRequestId {
        let id = SaveRequestId(self.next_id.saturating_add(1));
        self.next_id = id.0;
        self.events.push(SaveEvent::Loaded {
            id,
            profile: profile.to_string(),
            key: key.to_string(),
            data: self
                .entries
                .get(&(profile.to_string(), key.to_string()))
                .cloned(),
        });
        id
    }

    fn save(&mut self, profile: &str, key: &str, data: Arc<[u8]>) -> SaveRequestId {
        let id = SaveRequestId(self.next_id.saturating_add(1));
        self.next_id = id.0;
        self.entries
            .insert((profile.to_string(), key.to_string()), data);
        self.events.push(SaveEvent::Saved {
            id,
            profile: profile.to_string(),
            key: key.to_string(),
        });
        id
    }

    fn poll(&mut self) -> Vec<SaveEvent> {
        std::mem::take(&mut self.events)
    }
}

/// Monotonic host clock used by timers and frame budgets. Keeping this trait
/// in the core lets Web (performance.now), desktop and deterministic probes
/// share the same runtime without importing platform time APIs.
pub trait Clock {
    fn now_millis(&mut self) -> i64;

    /// Advances deterministic clocks at the runtime boundary. Wall-clock
    /// implementations leave this as a no-op; VirtualClock uses it so
    /// terminal/debugger sessions cannot remain frozen at zero.
    fn advance(&mut self, _delta: std::time::Duration) {}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct VirtualClock {
    now_millis: i64,
}

impl VirtualClock {
    pub const fn new(now_millis: i64) -> Self {
        Self { now_millis }
    }

    pub fn advance_millis(&mut self, delta: i64) {
        self.now_millis = self.now_millis.saturating_add(delta.max(0));
    }
}

impl Clock for VirtualClock {
    fn now_millis(&mut self) -> i64 {
        self.now_millis
    }

    fn advance(&mut self, delta: std::time::Duration) {
        let millis = delta.as_millis().min(i64::MAX as u128) as i64;
        self.advance_millis(millis);
    }
}

/// Deterministic in-memory implementation used by tests, debugger probes and
/// hosts that have already fetched a static web package.
#[derive(Clone, Debug, Default)]
pub struct MemoryAssetStore {
    entries: BTreeMap<String, Arc<[u8]>>,
    pending: Vec<AssetEvent>,
    next_request_id: u64,
    revision: u64,
}

impl MemoryAssetStore {
    pub fn insert(&mut self, path: impl Into<String>, data: impl Into<Arc<[u8]>>) {
        self.entries.insert(path.into(), data.into());
        self.revision = self.revision.saturating_add(1);
    }

    pub fn queue_failure(
        &mut self,
        path: impl Into<String>,
        kind: AssetKind,
        message: impl Into<String>,
    ) {
        self.pending.push(AssetEvent::Failed {
            id: AssetRequestId(0),
            path: path.into(),
            kind,
            message: message.into(),
        });
    }
}

impl AssetScheduler for MemoryAssetStore {
    fn request(&mut self, path: &str, kind: AssetKind) -> AssetRequestId {
        self.next_request_id = self.next_request_id.saturating_add(1);
        let id = AssetRequestId(self.next_request_id);
        let event = match self.entries.get(path) {
            Some(data) => AssetEvent::Ready {
                id,
                path: path.to_string(),
                kind,
                data: Arc::clone(data),
            },
            None => AssetEvent::Failed {
                id,
                path: path.to_string(),
                kind,
                message: "asset was not found".to_string(),
            },
        };
        self.pending.push(event);
        id
    }

    fn poll(&mut self) -> Vec<AssetEvent> {
        std::mem::take(&mut self.pending)
    }

    fn cancel(&mut self, id: AssetRequestId) -> bool {
        let before = self.pending.len();
        self.pending.retain(|event| match event {
            AssetEvent::Ready { id: event_id, .. } | AssetEvent::Failed { id: event_id, .. } => {
                *event_id != id
            }
        });
        before != self.pending.len()
    }

    fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AudioInstanceId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioBus {
    Master,
    Bgm,
    SoundEffect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioLoadPolicy {
    Auto,
    Streaming,
    StaticCached,
    StaticUncached,
}

/// Format of an externally decoded PCM stream (movie audio tracks).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmAudioSpec {
    pub sample_rate: u32,
    pub channels: u32,
}

/// One chunk of an externally decoded PCM stream: interleaved f32 samples in
/// the layout described by [`PcmAudioSpec`].
#[derive(Clone, Debug, PartialEq)]
pub struct PcmAudioChunk {
    pub pts_ms: i64,
    pub samples: Arc<[f32]>,
}

/// Host-owned pull interface for a live PCM source. The concrete transport may
/// be a bounded channel, platform callback or browser queue; the protocol does
/// not expose `std::sync::mpsc` (or any other platform threading primitive).
pub trait PcmStream: Send {
    fn next_chunk(&mut self) -> Option<PcmAudioChunk>;
}

/// A live PCM source fed by an external decoder (krkr-video decodes movie
/// soundtracks). Clones share the same host-owned stream handle, so there is
/// still only one consumer.
#[derive(Clone)]
pub struct PcmStreamSource {
    pub spec: PcmAudioSpec,
    /// Approximate total frames per channel; used for end-of-stream
    /// detection only.
    pub total_frames: u64,
    pub stream: Arc<Mutex<Box<dyn PcmStream>>>,
}

impl PcmStreamSource {
    pub fn new(spec: PcmAudioSpec, total_frames: u64, stream: Box<dyn PcmStream>) -> Self {
        Self {
            spec,
            total_frames,
            stream: Arc::new(Mutex::new(stream)),
        }
    }
}

impl fmt::Debug for PcmStreamSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PcmStreamSource")
            .field("spec", &self.spec)
            .field("total_frames", &self.total_frames)
            .finish_non_exhaustive()
    }
}

impl PartialEq for PcmStreamSource {
    fn eq(&self, other: &Self) -> bool {
        self.spec == other.spec
            && self.total_frames == other.total_frames
            && Arc::ptr_eq(&self.stream, &other.stream)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AudioSourceRef {
    pub storage: String,
}

impl AudioSourceRef {
    pub fn new(storage: impl Into<String>) -> Self {
        Self {
            storage: storage.into(),
        }
    }

    pub fn storage(&self) -> &str {
        &self.storage
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AudioCommand {
    Play {
        id: AudioInstanceId,
        bus: AudioBus,
        source: AudioSourceRef,
        load_policy: AudioLoadPolicy,
        looping: bool,
        volume: f32,
    },
    /// Plays a live PCM stream produced by an external decoder. Used for
    /// movie soundtracks: the movie container belongs to the video backend,
    /// not to the audio file loaders.
    PlayPcmStream {
        id: AudioInstanceId,
        bus: AudioBus,
        source: PcmStreamSource,
        volume: f32,
    },
    Preload {
        source: AudioSourceRef,
        load_policy: AudioLoadPolicy,
    },
    /// Attaches an instance's `WaveSoundBuffer.filters` chain, replacing any
    /// chain it had.
    ///
    /// `filters` carries the `interface` value of each element of the
    /// instance's script-side `filters` array, in array order — the
    /// reference's `RebuildFilterChain` (`sound/WaveIntf.cpp:865-905`) reads
    /// exactly those elements and casts each element's `interface` to
    /// `iTVPBasicWaveFilter*` (`sound/WaveIntf.h:130`).  In this engine the
    /// integer is the opaque id of a filter a plugin registered with the audio
    /// backend (`krkr_audio::register_wave_filter`), which is the closest
    /// honest stand-in for the raw pointer; an id nothing is registered under
    /// leaves the chain and is reported.  An empty list drops the chain
    /// (`ClearFilterChain`, `:907-923`), which is what `open` on a buffer with
    /// an empty array means.
    SetFilters {
        id: AudioInstanceId,
        filters: Vec<i64>,
    },
    Stop {
        id: AudioInstanceId,
        fade_seconds: f32,
    },
    SetVolume {
        id: AudioInstanceId,
        volume: f32,
        fade_seconds: f32,
    },
    Pause {
        id: AudioInstanceId,
        fade_seconds: f32,
    },
    Resume {
        id: AudioInstanceId,
        fade_seconds: f32,
    },
    StopBus {
        bus: AudioBus,
        fade_seconds: f32,
    },
    SetBusVolume {
        bus: AudioBus,
        volume: f32,
        fade_seconds: f32,
    },
}

/// Lifecycle state reported by an audio backend.  These protocol types live
/// in core so the engine does not depend on the native Kira/CPAL crate.
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

#[derive(Clone, Debug, Eq, PartialEq)]
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

impl std::error::Error for AudioError {}

/// Backend-neutral audio surface implemented by Kira, WebAudio and mobile
/// audio adapters.
pub trait AudioSink {
    fn prepare(&mut self) -> Result<(), AudioError>;
    fn submit(&mut self, commands: &[AudioCommand]) -> Result<(), AudioError>;
    fn poll_events(&mut self) -> Vec<AudioEvent>;

    /// Commands for host-owned media APIs such as WebAudio. Native sinks
    /// execute commands directly and return an empty queue.
    fn take_commands(&mut self) -> Vec<AudioCommand> {
        Vec::new()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.width
            && point.y < self.y + self.height
    }

    pub fn inset(self, amount: f32) -> Self {
        Self {
            x: self.x + amount,
            y: self.y + amount,
            width: (self.width - amount * 2.0).max(0.0),
            height: (self.height - amount * 2.0).max(0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb_u8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectCommand {
    pub rect: Rect,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextCommand {
    pub position: Point,
    pub text: String,
    pub color: Color,
    pub size: f32,
    pub font: FontSpec,
    pub style: TextStyle,
}

pub type TextureId = u64;
pub type LayerId = u64;

#[derive(Clone, Debug)]
struct UploadedImageState {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageUpload {
    pub texture_id: TextureId,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

impl ImageUpload {
    pub fn new(texture_id: TextureId, width: u32, height: u32, rgba: Arc<[u8]>) -> Self {
        Self {
            texture_id,
            width,
            height,
            rgba,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageCommand {
    pub texture_id: TextureId,
    pub rect: Rect,
    pub source_rect: Rect,
    pub texture_size: Size,
    pub opacity: f32,
    /// Official `GetOperationModeFromType` → `omOpaque` presents the RGB
    /// through `TVPCopyOpaqueImage` (`0xff000000 | src`), ignoring stored
    /// alpha. Kirakira's GPU path must do the same for `ltOpaque`.
    pub opaque: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DrawCommand {
    Rect(RectCommand),
    Text(TextCommand),
    Image(ImageCommand),
}

/// The transition providers this build has a kernel for, keyed by the exact
/// name the reference registers (`TVPAddTransHandlerProvider`,
/// `TransIntf.cpp:307-324`, which stores `iTVPTransHandlerProvider::GetName`).
///
/// `crossfade`, `universal` and `scroll` are the reference's built-ins
/// (`TVPRegisterDefaultTransHandlerProvider`, `TransIntf.cpp:1196-1213`); the
/// other seven come from `extrans.dll` (`extrans/Main.cpp:32-36` and each
/// provider's `GetName`, e.g. `wave.cpp:297`, `mosaic.cpp:408`, `turn.cpp:526`,
/// `rotatetrans.cpp:152/252/424`, `ripple.cpp:1549`).
///
/// Providers that a *loaded plugin* registers but this build has no kernel for
/// -- extNagano's twelve names, GlitchEffect's three, the kaicho family -- are
/// deliberately absent: official only resolves a name while some registered
/// provider answers to it, so the engine's lookup has to consult the plugin
/// host's provider registry for those and decide between registering a
/// provider and reporting the name as unknown.  See `UnknownTransitionName`.
pub const TRANSITION_PROVIDER_NAMES: &[(&str, TransitionMethod)] = &[
    ("crossfade", TransitionMethod::Crossfade),
    ("universal", TransitionMethod::Universal),
    ("scroll", TransitionMethod::Scroll),
    ("wave", TransitionMethod::Wave),
    ("mosaic", TransitionMethod::Mosaic),
    ("turn", TransitionMethod::Turn),
    ("rotatezoom", TransitionMethod::RotateZoom),
    ("rotatevanish", TransitionMethod::RotateVanish),
    ("rotateswap", TransitionMethod::RotateSwap),
    ("ripple", TransitionMethod::Ripple),
];

/// A transition provider name, and the kernel state of its implementation in
/// this build.
///
/// Every name in `TRANSITION_PROVIDER_NAMES` has a kernel in
/// `krkr-render/transition.wgsl`.  The fidelity of the ones that predate the
/// M36 port is *approximate* and recorded per variant below, so no silent
/// approximation survives in the code: `mosaic`, `turn`, the three rotate
/// kernels and `ripple` reproduce the family of effect, not the reference's
/// element-by-element result, and each variant documents what differs.  `wave`
/// is the one ported against `extrans/wave.cpp` directly (M36).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TransitionMethod {
    /// `tTVPCrossFadeTransHandlerProvider` (`TransIntf.cpp:1196`).
    ///
    /// Faithful: the composite reproduces `TVPConstAlphaBlend_SD`
    /// (`TransIntf.cpp:680`, per-channel lerp including alpha) over the scene
    /// beneath the destination layer; the expansion and its test live in
    /// `krkr-render` (`transition_composite_plan`, `composite_plan_reproduces_...`).
    Crossfade = 0,
    /// `tTVPUniversalTransHandler` (`TransIntf.cpp:777`): 8-bit rule graphic,
    /// `vague` (default 64) as the softness ramp.
    ///
    /// Approximate: the reference advances a `tjs_int` phase and blends through
    /// the rule's threshold; the kernel walks `progress * (1 + vague)` and
    /// reads the rule as a scroll-and-repeat texture instead of the reference's
    /// `GetScanLine` sampling.
    ///
    /// The rule's geometry follows the reference and never the window, which is
    /// worth recording because it was once wrong.  The provider loads the rule
    /// **tiled to the destination layer's own size**
    /// (`imagepro->LoadImage(rulename, 8, 0x02ffffff, src1w, src1h, &scpro)`,
    /// `TransIntf.cpp:781`, handed the destination layer's `GetWidth()`/
    /// `GetHeight()` at `LayerIntf.cpp:6336-6346`; the loader grows its buffer
    /// to that size and repeats the source into it
    /// (`TVPLoadGraphic_SizeCallback`, `GraphicsLoaderIntf.cpp:1795-1824`, the
    /// clamp at `:1803`; `TVPLoadGraphic_ScanLineCallback`, `:1886`, `:1895`,
    /// `:1915`; the contract is stated at `:2286`) and samples it at
    /// `data.Left`/`data.Top` -- offsets *inside* that bitmap, in its own
    /// logical pixels (`LayerIntf.cpp:6665-6676`, read at
    /// `TransIntf.cpp:825-851`: the scan line at `:836`, `rule += data->Left`
    /// at `:851`).
    /// So the repeat period is the rule's natural size, a screen-sized rule is
    /// never repeated at all, and neither the window's size nor the DPI scale
    /// enters the geometry.  `transition_universal`
    /// (`krkr-render/src/transition.wgsl`) samples through `image_local` and
    /// repeats the rule only below the destination rectangle's own size.  Until
    /// M206 it derived `rule_uv` from the frame's *physical* viewport
    /// (`config.width/height`) and wrapped it with `fract` below that viewport,
    /// so a window larger than the game's screen repeated the whole transition
    /// once per (window / rule) axis -- four quarter-screen copies at a 2x
    /// window, the reported symptom.  Every rule image the three shipped titles
    /// load is exactly a screen size (measured from their archives: GINKA's
    /// `rule/*` is 142 entries, all 1280x720; 少女世界的生存之道's `rule/map*`
    /// is 72, all 1920x1080; PARQUET's is 91 -- 72 at 1280x720 and 19 at
    /// 1920x1080).
    Universal = 1,
    /// `tTVPScrollTransHandler` (`TransIntf.cpp:925`): `from` picks the
    /// direction, `stay` keeps one face in place.
    ///
    /// Approximate: same three-part composition (leaving face, entering face,
    /// empty band), but the kernel resolves the direction from the numeric
    /// `from` code only and the band widths follow `progress` linearly while
    /// the reference computes its phase from the tick clock.
    Scroll = 2,
    /// `wave` (`extrans/wave.cpp:16-265`): raster scroll, `maxh` (50),
    /// `maxomega` (0.2), `bgcolor1`/`bgcolor2` (0), `wavetype` (0).
    ///
    /// Faithful: ported element by element in M36, with the deviations the port
    /// could not remove recorded at the kernel (`krkr-render/transition.wgsl`,
    /// `transition_wave`): the millisecond clock is reconstructed from
    /// `progress` and `TransitionParams::duration_millis`, the destination
    /// layer type has no channel in the model, the lerp is the float form of
    /// the reference's per-byte integer blend, and a non-opaque destination
    /// reads the scene beneath at the shifted position.
    Wave = 3,
    /// `mosaic` (`extrans/mosaic.cpp:11`): `maxsize` (30), one averaged colour
    /// per animated block.
    ///
    /// Approximate: the reference ramps the block size as the integer triangle
    /// `(maxsize-2) * t/HalfTime + 2` and re-anchors the block grid to the image
    /// centre every frame (`mosaic.cpp:123-143`), while the kernel uses a float
    /// `1 + sin(pi*p) * maxsize` ramp with the grid anchored at the destination
    /// bitmap's origin; the reference samples the block's centre pixel exactly
    /// and fills the block with `Blend`, the kernel samples through a bilinear
    /// sampler.
    Mosaic = 4,
    /// `turn` (`extrans/turn.cpp:15`): `bgcolor` (0), 64x64 tiles folded by a
    /// generated per-phase line table plus a specular gloss.
    ///
    /// Approximate: the kernel has neither the fold table
    /// (`turntrans_table.cpp`, 63 x 64 entries of 16.16 source scans) nor the
    /// diagonal phase sweep (`phase = Phase - (x-y)*2`, `turn.cpp:141-144`)
    /// nor the gloss; it squeezes each tile horizontally and reveals the tiles
    /// in a pseudo-random order (`transition.wgsl`, `hash_tile`).  The 64x64
    /// tile grid is the reference's: it is counted from the destination
    /// bitmap's size (`xcount = (Width-1)/64 + 1`, `turn.cpp:141-142`).
    Turn = 5,
    /// `rotatezoom` (`extrans/rotatetrans.cpp:18-120`): `factor` (1),
    /// `accel` (0), `twist` (2), `twistaccel` (-2), `centerx`/`centery`
    /// (w/2, h/2), destination fixed and the source rotated/scaled to 1.
    ///
    /// Approximate: the scale and twist ramps match (`pow`/`1-(1-x)^-a`,
    /// `rotatetrans.cpp:72-103`) but the kernel spins the source the opposite
    /// way (`2*pi*twist*(1 - ramp)` against `2*pi*Twist*ramp`, `:105`), keeps a
    /// fixed pivot where the reference drifts the centre toward the screen
    /// centre (`:89-90`), rotates in aspect-normalized uv (a shear on
    /// non-square frames) and cross-fades inside the quad where the reference
    /// copies pixels (`:117`, `rotatebase.cpp` has no blending).
    ///
    /// The pivot itself is the destination bitmap's, as in the reference:
    /// `centerx`/`centery` are pixels of that bitmap and default to its centre
    /// (`rotatetrans.cpp:185-186`, `:206-210`).  `transition.wgsl` converts
    /// them through its `image_local`/`frame_uv` pair, so they are
    /// destination-logical rather than window-physical; dividing them by the
    /// physical viewport (which the kernel did until M206) put the pivot at
    /// `centerx / (logical size * window scale)` of the frame -- off by exactly
    /// the render scale on any scaled window.  `RotateVanish` shares the pivot
    /// and the conversion; `RotateSwap` reads no `centerx`/`centery` and
    /// pivots on the destination bitmap's centre.
    RotateZoom = 6,
    /// `rotatevanish` (`extrans/rotatetrans.cpp:222-316`): the same handler
    /// with `factor` 1 -> 0 and the source not fixed; `accel` (2), `twist` (2),
    /// `twistaccel` (2).
    ///
    /// Approximate, and the closest of the six: scale ramp `1 - pow(p, accel)`,
    /// twist sign and ramp `2*pi*twist*pow(p, twistaccel)`, which face is the
    /// background and what the pixels outside the quad show all match.  What
    /// differs is the same as `RotateZoom` (in-quad crossfade, aspect shear,
    /// fixed pivot, bilinear taps instead of integer copies).
    RotateVanish = 7,
    /// `rotateswap` (`extrans/rotatetrans.cpp:318-475`): `bgcolor` (0),
    /// `twist` (1); the two faces spin out and in, one on top of the other.
    ///
    /// Approximate: the twist angles match in sign and magnitude, but the
    /// reference gives each face a lateral sine slide and a vertical squash
    /// (`:352-387`), uses the `zm^2` / `1-(1-zm)^2` ramps, and composites the
    /// two faces as hard-edged regions that switch order at half time, while
    /// the kernel only cross-fades them.
    RotateSwap = 8,
    /// `ripple` (`extrans/ripple.cpp:1519`): `centerx`/`centery`,
    /// `rwidth` (128, 16/32/64/128), `roundness` (1.0), `speed` (6 rad/s),
    /// `maxdrift` (24).
    ///
    /// Approximate: the reference evaluates a standing displacement wave from
    /// cached `DisplaceMap`/`DriftMap` tables (`ripple.cpp:148-263`) whose
    /// amplitude is `sin(pi*CurTime/Time)`, quantized to 8.8 fixed point and
    /// mirror-wrapped at the borders, while the kernel draws a travelling
    /// Gaussian band around a centre-out front, uses `rwidth` as a band width
    /// instead of the wave's wavelength mask and `speed` as a spatial frequency
    /// instead of a phase advance.  The front, the band and the drift are
    /// measured in the destination bitmap's pixels, as the reference's tables
    /// are (`centerx`/`centery`, `rwidth` and `maxdrift` are pixels of that
    /// bitmap, `ripple.cpp:1072`, `:1478-1525`).
    Ripple = 9,
}

impl TransitionMethod {
    /// The reference's provider lookup (`TVPFindTransHandlerProvider`,
    /// `TransIntf.cpp:341-359`): a hash-table find on the name, so the match is
    /// **exact and case-sensitive** -- `"Wave"` is not `"wave"`, and the
    /// reference throws for it.
    ///
    /// `name` is the caller's own spelling; `UnknownTransitionName` carries it
    /// back so the script sees the official text.
    ///
    /// The plugin-provided names (`extnagano`: `zoomfade`, `blurfade`,
    /// `scanline`, `3duniversal`, `rgbfade`, `spin`, `flutter`, `imagewipe`,
    /// `book`, `honeyturn`, `morphing`, `multiripple`; `GlitchEffect`:
    /// `glitch`, `fadeglitch`, `loopglitch`) are not in the table: official
    /// resolves them only while the plugin's provider is registered, and this
    /// build's plugin host does not publish a provider registry yet.  A caller
    /// that has one must check it *before* reporting the name as unknown.
    pub fn try_from_name(name: &str) -> Result<Self, UnknownTransitionName> {
        TRANSITION_PROVIDER_NAMES
            .iter()
            .find(|(provider, _)| *provider == name)
            .map(|(_, method)| *method)
            .ok_or_else(|| UnknownTransitionName::new(name))
    }

    /// The lenient name mapping the engine's transition call sites use.
    ///
    /// It lower-cases the name and maps everything it does not know to
    /// `Crossfade`.  That is neither the reference's behaviour
    /// (`try_from_name`: a provider lookup that throws
    /// `TVPCannotFindTransHander`, `TransIntf.cpp:354`) nor Kirikiroid2's
    /// (one warning box, then crossfade,
    /// `Kirikiroid2/src/core/visual/TransIntf.cpp:365-372`); it is Kirakira's
    /// own silent degradation and the reason the fidelity table above records
    /// each kernel's state.  New call sites use `try_from_name` and report
    /// `UnknownTransitionName::message()`; this stays for the paths that must
    /// not abort a scenario until they are migrated.
    pub fn from_name(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        match name.as_str() {
            "universal" => Self::Universal,
            "scroll" => Self::Scroll,
            "wave" => Self::Wave,
            "mosaic" => Self::Mosaic,
            "turn" => Self::Turn,
            "rotatezoom" => Self::RotateZoom,
            "rotatevanish" => Self::RotateVanish,
            "rotateswap" => Self::RotateSwap,
            "ripple" => Self::Ripple,
            "crossfade" | "" => Self::Crossfade,
            _ => Self::Crossfade,
        }
    }

    pub const fn as_code(self) -> f32 {
        self as u8 as f32
    }

    pub const fn as_name(self) -> &'static str {
        match self {
            Self::Crossfade => "crossfade",
            Self::Universal => "universal",
            Self::Scroll => "scroll",
            Self::Wave => "wave",
            Self::Mosaic => "mosaic",
            Self::Turn => "turn",
            Self::RotateZoom => "rotatezoom",
            Self::RotateVanish => "rotatevanish",
            Self::RotateSwap => "rotateswap",
            Self::Ripple => "ripple",
        }
    }
}

/// The reference's `TVPCannotFindTransHander` (`TransIntf.cpp:354`): the name
/// no registered provider answered to.
///
/// Script identity: `TVPFindTransHandlerProvider` reports it through
/// `TVPThrowExceptionMessage` (`MsgIntf.cpp:123-140`), a message-only
/// `eTJSError`.  At a script `try` boundary
/// `TJS_CONVERT_TO_TJS_EXCEPTION_OBJECT` builds the caught object with
/// `TJSGetExceptionObject(tjs, result, msg, NULL)`
/// (`tjs2/tjsError.h:115-146`; the try opcode is `tjsInterCodeExec.cpp:1551-1586`),
/// so the script sees:
///
/// * `e.message == message()` -- the exception text and nothing else,
/// * no numeric `tjs_error` code: an `eTJSError` is not built from a `tjs_error`
///   value, so the engine raises it as `TjsError::runtime(...)`
///   (`TjsErrorKind::Runtime`, whose `tjs_error_code()` is `None`),
/// * no `trace`: the reference passes `NULL` for the trace, so the exception is
///   message-only where a caught `throw` carries a trace.
///
/// KAG3 does not catch it inside the tag: `MainWindow.tjs:5482-5487` calls
/// `beginTransition` directly.  The Conductor's outer loop catches it, stops the
/// scenario (`timer.enabled = false`, `onStop()`), reports `dm(msg)` and
/// rethrows a `ConductorException` to the global exception handler (kirikiri2
/// `kag3/template/system/Conductor.tjs:55-186`), so an unknown name kills the
/// scenario instead of degrading it.  Kirikiroid2 is the outlier: one warning
/// box, then the `crossfade` provider
/// (`Kirikiroid2/src/core/visual/TransIntf.cpp:365-372`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownTransitionName {
    /// The name exactly as the caller spelled it: `TVPFormatMessage` substitutes
    /// it into the message verbatim, so `"Wave"` stays `"Wave"`.
    pub name: String,
}

impl UnknownTransitionName {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// The text the reference substitutes `%1` into
    /// (`IDS_TVP_CANNOT_FIND_TRANS_HANDER`, `vc2012/string_table_en.rc:154`).
    pub fn message(&self) -> String {
        format!("Cannot find transition handler {}", self.name)
    }
}

impl fmt::Display for UnknownTransitionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for UnknownTransitionName {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TransitionScrollFrom {
    Left = 0,
    Top = 1,
    Right = 2,
    Bottom = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TransitionScrollStay {
    NoStay = 0,
    StayDest = 1,
    StaySrc = 2,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransitionParams {
    pub method: TransitionMethod,
    pub vague: f32,
    pub scroll_from: TransitionScrollFrom,
    pub scroll_stay: TransitionScrollStay,
    pub wave_type: f32,
    pub max_h: f32,
    pub max_omega: f32,
    pub bg_color1: Color,
    pub bg_color2: Color,
    pub max_size: f32,
    pub bg_color: Color,
    pub factor: f32,
    pub accel: f32,
    pub twist: f32,
    pub twist_accel: f32,
    pub center_x: f32,
    pub center_y: f32,
    pub ripple_width: f32,
    pub roundness: f32,
    pub speed: f32,
    pub max_drift: f32,
    /// The transition's duration in milliseconds: exactly the `time` option the
    /// reference constructs its handler with (`tTVPWaveTransHandler::Time`,
    /// `wave.cpp:324-355`).  Milliseconds, whole number, non-negative, no wrap
    /// (the reference stores it in a `tjs_uint64`); `f32` holds every duration a
    /// game can specify (`2^24` ms is over four and a half hours, and above that
    /// only sub-millisecond precision is lost, which the reference's integer
    /// clock never needs).
    ///
    /// **Invariant: `0.0`, or at least `2.0`.**  Every provider clamps `time`
    /// before it builds its handler -- `if(time < 2) time = 2;`
    /// (`extrans/wave.cpp:336`, the ctor call at `:355`; the same idiom in
    /// `mosaic.cpp`, `turn.cpp`, `rotatetrans.cpp`, `ripple.cpp`), and the
    /// crossfade family clamps at `TransIntf.cpp:528`.  A handler therefore
    /// never runs on a duration below 2 ms, and `HalfTime = Time / 2`
    /// (`wave.cpp:47`) is never zero.  The kernels clamp again so that a caller
    /// which stores the raw option cannot produce a division by zero, but a
    /// caller reading this field may rely on the invariant.
    ///
    /// The extrans kernels need it because their ramps run on the millisecond
    /// clock (`CurTime = tick - StartTick`, `wave.cpp:130-160`;
    /// `BlendRatio = CurTime * 255 / Time`, `:156`; `ripple.cpp:1258-1266`),
    /// not on the normalized phase.  `0.0` means "the caller did not supply the
    /// clock": the kernel then derives the clock from `FrameTransition::progress`
    /// (`CurTime ~= progress * Time`), which is the same curve minus the
    /// reference's integer-millisecond quantization.
    pub duration_millis: f32,
}

impl Default for TransitionParams {
    fn default() -> Self {
        Self {
            method: TransitionMethod::Crossfade,
            vague: 64.0,
            scroll_from: TransitionScrollFrom::Left,
            scroll_stay: TransitionScrollStay::NoStay,
            wave_type: 0.0,
            max_h: 50.0,
            max_omega: 0.2,
            // The reference's own defaults are `bgcolor1 = 0` / `bgcolor2 = 0`
            // (`wave.cpp:327-331`, `turn.cpp:555-564`, `rotatetrans.cpp:181-210`)
            // and a `tjs_uint32` option value is ARGB, so `0` is transparent
            // black and a kernel that fills a vacated region with it lets the
            // scene beneath the layer show through (`wave.cpp:203-221`).
            bg_color1: Color::new(0.0, 0.0, 0.0, 0.0),
            bg_color2: Color::new(0.0, 0.0, 0.0, 0.0),
            max_size: 30.0,
            bg_color: Color::new(0.0, 0.0, 0.0, 0.0),
            factor: 1.0,
            accel: 0.0,
            twist: 2.0,
            twist_accel: -2.0,
            center_x: -1.0,
            center_y: -1.0,
            ripple_width: 128.0,
            roundness: 1.0,
            speed: 6.0,
            max_drift: 24.0,
            duration_millis: 0.0,
        }
    }
}

/// One running transition as the presentation layer sees it.
///
/// Official KRKR keeps a transition per layer (`tTJSNI_BaseLayer::InTransition`,
/// `LayerIntf.cpp:6334`) and the handler composites the destination and source
/// bitmaps inside the destination layer's own rectangle
/// (`tTVPDivisibleData`, `LayerIntf.cpp:6665-6676`).  `dest_rect` is that
/// rectangle in frame coordinates, so unrelated layers can transition at the
/// same time and each one only rewrites its own area.  `None` means the
/// destination has no measurable geometry, and the composite then covers the
/// whole frame (the engine's projection cannot confine it).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameTransition {
    pub method: String,
    pub progress: f32,
    pub params: TransitionParams,
    pub dest_rect: Option<Rect>,
    pub rule_texture_id: Option<TextureId>,
    pub rule_image_upload: Option<ImageUpload>,
    pub frozen_draw_commands: Vec<DrawCommand>,
    pub frozen_image_uploads: Vec<ImageUpload>,
    /// The scene without the destination layer's subtree.
    ///
    /// Official's transition blends the two layer bitmaps and lets the *layer
    /// manager* composite the result, so the destination's pixels are not
    /// pre-composited over the scene when the blend runs: at a pixel the source
    /// does not cover the destination's alpha scales by `1 - progress`
    /// (`const_alpha_blend_functor`, `blend_functor_c.h:584-594`) and the
    /// layers underneath show through.  Rendering the incoming face over this
    /// base is what gives the composite that behaviour.
    pub under_draw_commands: Vec<DrawCommand>,
    pub under_image_uploads: Vec<ImageUpload>,
    /// The incoming face (`tTVPDivisibleData::Src2`, `LayerIntf.cpp:6611`): the
    /// source layer's own content, positioned where the destination draws.
    ///
    /// Official draws neither layer during the transition -- the handler
    /// composites the destination's own bitmap (`Src1`) with the source's
    /// (`Src2`, `TransSrc->Complete(destrect)`, `:6604`) and only the stop's
    /// `Exchange`/`Swap` moves content between the objects.  The engine
    /// therefore hands the renderer both faces instead of writing the source
    /// into the destination layer.
    pub source_draw_commands: Vec<DrawCommand>,
    pub source_image_uploads: Vec<ImageUpload>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameOutput {
    pub clear_color: Color,
    pub clip: Option<Rect>,
    pub draw_commands: Vec<DrawCommand>,
    pub image_uploads: Vec<ImageUpload>,
    /// Texture ids no longer referenced by this frame. Presentation backends
    /// can release their GPU/Canvas resources immediately instead of keeping
    /// every image ever seen by a long-running game.
    pub image_releases: Vec<TextureId>,
    /// Every transition running this frame, in start order.  Each entry is
    /// composited inside its own `dest_rect` on top of `draw_commands`.
    pub transitions: Vec<FrameTransition>,
}

impl FrameOutput {
    pub fn new(clear_color: Color, draw_commands: Vec<DrawCommand>) -> Self {
        Self {
            clear_color,
            clip: None,
            draw_commands,
            image_uploads: Vec::new(),
            image_releases: Vec::new(),
            transitions: Vec::new(),
        }
    }

    pub fn with_clip(mut self, clip: Rect) -> Self {
        self.clip = Some(clip);
        self
    }

    pub fn with_image_uploads(mut self, image_uploads: Vec<ImageUpload>) -> Self {
        self.image_uploads = image_uploads;
        self
    }

    pub fn with_transitions(mut self, transitions: Vec<FrameTransition>) -> Self {
        self.transitions = transitions;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerImage {
    pub upload: ImageUpload,
}

impl LayerImage {
    pub fn new(texture_id: TextureId, width: u32, height: u32, rgba: Arc<[u8]>) -> Self {
        Self {
            upload: ImageUpload::new(texture_id, width, height, rgba),
        }
    }

    pub fn size(&self) -> Size {
        Size::new(self.upload.width as f32, self.upload.height as f32)
    }
}

/// Official `tTJSNI_BaseLayer::ProvinceImage` (`LayerIntf.cpp:410`): an 8-bit
/// map used for `htProvince` hit testing, one province index per pixel. It is
/// loaded from a palettized or grayscale graphic and must match the main
/// image's size; it is never composited.
#[derive(Clone, Debug, PartialEq)]
pub struct ProvinceImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

impl ProvinceImage {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels: Arc::from(pixels),
        }
    }

    pub fn pixel(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return 0;
        }
        self.pixels
            .get((y as usize) * (self.width as usize) + x as usize)
            .copied()
            .unwrap_or(0)
    }

    /// `tTVPBaseBitmap::SetSizeWithFill` on the province plane
    /// (`LayerIntf.cpp:2047`): the overlapping region keeps its values and the
    /// expanded band is filled with 0, matching `ChangeImageSize`.
    pub fn resized(&self, width: u32, height: u32) -> ProvinceImage {
        if self.width == width && self.height == height {
            return self.clone();
        }
        let mut pixels = vec![0u8; width as usize * height as usize];
        let copy_width = self.width.min(width) as usize;
        let copy_height = self.height.min(height) as usize;
        for row in 0..copy_height {
            let source = row * self.width as usize;
            let dest = row * width as usize;
            pixels[dest..dest + copy_width]
                .copy_from_slice(&self.pixels[source..source + copy_width]);
        }
        ProvinceImage::new(width, height, pixels)
    }

    pub fn set_pixel(&mut self, x: i64, y: i64, value: u8) {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return;
        }
        self.make_independent(true);
        let index = (y as usize) * (self.width as usize) + x as usize;
        if let Some(pixels) = Arc::get_mut(&mut self.pixels)
            && let Some(pixel) = pixels.get_mut(index)
        {
            *pixel = value;
        }
    }

    /// `tTVPBaseBitmap::Fill` on the province plane: clips to the plane and
    /// writes `value` into the given layer-local rectangle.
    pub fn fill_rect(&mut self, x: i64, y: i64, width: i64, height: i64, value: u8) {
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x.saturating_add(width)).min(self.width as i64);
        let y1 = (y.saturating_add(height)).min(self.height as i64);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        self.make_independent(true);
        let Some(pixels) = Arc::get_mut(&mut self.pixels) else {
            return;
        };
        for py in y0..y1 {
            let row = (py as usize) * (self.width as usize);
            for px in x0..x1 {
                pixels[row + px as usize] = value;
            }
        }
    }

    /// Official `tTJSNI_BaseLayer::IndependProvinceImage`: `copy` detaches the
    /// bitmap from any shared source, `IndependNoCopy` only drops the sharing
    /// marker. Kirakira's copy-on-write is automatic, so `copy == false` is a
    /// no-op and `copy == true` materializes a private buffer.
    pub fn make_independent(&mut self, copy: bool) {
        if copy && Arc::strong_count(&self.pixels) > 1 {
            self.pixels = Arc::from(self.pixels.to_vec());
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerNode {
    pub id: LayerId,
    pub name: String,
    pub parent: Option<LayerId>,
    pub left: f32,
    pub top: f32,
    /// Frame translation of the owning window's client origin.
    ///
    /// Official KRKR gives every `Window` its own layer tree owner and places
    /// its client area at the window position, so a dialog's layers are drawn
    /// where `Window.setPos`/`left`/`top` put the window.  Kirakira composites
    /// every window into one frame, so the subtree root of a non-main window
    /// carries the window's offset relative to the main window here.  Only
    /// render-tree roots are non-zero; children inherit it through traversal.
    pub window_offset: Point,
    pub width: f32,
    pub height: f32,
    pub image_left: f32,
    pub image_top: f32,
    pub image_width: f32,
    pub image_height: f32,
    pub visible: bool,
    pub renderable: bool,
    pub enabled: bool,
    pub node_enabled: bool,
    pub opacity: u8,
    pub z_order: i32,
    pub layer_type: i32,
    pub face: i32,
    pub hit_type: i32,
    pub hit_threshold: i32,
    pub image: Option<LayerImage>,
    /// `htProvince` hit-test map (`tTJSNI_BaseLayer::ProvinceImage`).
    pub province: Option<ProvinceImage>,
    /// Official `tTJSNI_BaseLayer::ClipRect` (`LayerIntf.cpp`): every blit and
    /// fill is clipped to this layer-local rectangle. `None` is the
    /// `ResetClip()` state, where the clip equals the layer rectangle.
    pub clip: Option<Rect>,
}

impl LayerNode {
    pub fn new(
        id: LayerId,
        name: impl Into<String>,
        parent: Option<LayerId>,
        z_order: i32,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            parent,
            left: 0.0,
            top: 0.0,
            window_offset: Point::new(0.0, 0.0),
            width: 0.0,
            height: 0.0,
            image_left: 0.0,
            image_top: 0.0,
            image_width: 0.0,
            image_height: 0.0,
            visible: false,
            renderable: true,
            enabled: true,
            node_enabled: true,
            opacity: 255,
            z_order,
            layer_type: 2,
            face: 128,
            hit_type: 0,
            hit_threshold: 0,
            image: None,
            province: None,
            clip: None,
        }
    }

    pub fn rect(&self) -> Rect {
        Rect::new(self.left, self.top, self.width, self.height)
    }

    pub fn image_rect(&self) -> Rect {
        Rect::new(
            self.image_left,
            self.image_top,
            self.image_width,
            self.image_height,
        )
    }

    fn hit_test(&self, origin: Point, point: Point) -> bool {
        match self.hit_type {
            // htProvince
            1 => self.province_hit_test(origin, point),
            _ => self.alpha_hit_test(origin, point),
        }
    }

    /// Official `tTJSNI_BaseLayer::_HitTestNoVisibleCheck` (`LayerIntf.cpp:2911`):
    /// a province layer hits where its province index is non-zero and never
    /// hits without a province image.
    fn province_hit_test(&self, origin: Point, point: Point) -> bool {
        let Some(province) = &self.province else {
            return false;
        };
        let x = (point.x - origin.x - self.image_left).floor() as i64;
        let y = (point.y - origin.y - self.image_top).floor() as i64;
        province.pixel(x, y) != 0
    }

    fn alpha_hit_test(&self, origin: Point, point: Point) -> bool {
        let Some(image) = &self.image else {
            return self.hit_threshold <= 0;
        };
        let x = (point.x - origin.x - self.image_left).floor() as i64;
        let y = (point.y - origin.y - self.image_top).floor() as i64;
        if x < 0 || y < 0 || x >= image.upload.width as i64 || y >= image.upload.height as i64 {
            return false;
        }
        let index = ((y as u32 * image.upload.width + x as u32) * 4 + 3) as usize;
        i32::from(image.upload.rgba[index]) >= self.hit_threshold
    }

    fn image_command(
        &self,
        origin: Point,
        clip: Rect,
        inherited_opacity: f32,
    ) -> Option<ImageCommand> {
        let image = self.image.as_ref()?;
        if self.width <= 0.0 || self.height <= 0.0 {
            return None;
        }

        // Official `DrawSelf` (`LayerIntf.cpp:5366`) presents `MainImage` at
        // the bitmap size. Kirakira's `imageWidth` can be 0 on AffineLayer
        // dests because the TJS getter reads `_image.imageWidth`, not the
        // dest bitmap; skipping those uploads blacks out GINKA's background.
        let texture_size = image.size();
        let image_width = texture_size.width;
        let image_height = texture_size.height;
        let layer_x0 = origin.x;
        let layer_y0 = origin.y;
        let layer_x1 = origin.x + self.width;
        let layer_y1 = origin.y + self.height;
        let image_x0 = origin.x + self.image_left;
        let image_y0 = origin.y + self.image_top;
        let image_x1 = image_x0 + image_width;
        let image_y1 = image_y0 + image_height;

        let target_x0 = layer_x0.max(image_x0).max(clip.x);
        let target_y0 = layer_y0.max(image_y0).max(clip.y);
        let target_x1 = layer_x1.min(image_x1).min(clip.x + clip.width);
        let target_y1 = layer_y1.min(image_y1).min(clip.y + clip.height);
        if target_x1 <= target_x0 || target_y1 <= target_y0 {
            return None;
        }

        Some(ImageCommand {
            texture_id: image.upload.texture_id,
            rect: Rect::new(
                target_x0,
                target_y0,
                target_x1 - target_x0,
                target_y1 - target_y0,
            ),
            source_rect: Rect::new(
                target_x0 - image_x0,
                target_y0 - image_y0,
                target_x1 - target_x0,
                target_y1 - target_y0,
            ),
            texture_size,
            opacity: inherited_opacity * self.opacity as f32 / 255.0,
            // `tTJSNI_BaseLayer::GetOperationModeFromType` (`LayerIntf.cpp:1404`):
            // `ltOpaque` → `omOpaque`. Unknown types also fall through to
            // `omOpaque`; binders have no image so they never reach here.
            opaque: self.layer_type == 1,
        })
    }

    pub fn set_image(&mut self, image: LayerImage) {
        let size = image.size();
        self.image_width = size.width;
        self.image_height = size.height;
        if self.width <= 0.0 || self.height <= 0.0 {
            self.width = size.width;
            self.height = size.height;
        }
        self.image = Some(image);
    }

    pub fn clear_image(&mut self) {
        self.image = None;
        self.image_width = 0.0;
        self.image_height = 0.0;
    }

    /// Copies the render state a KAG page transition projects from one layer
    /// onto another (`copy_render_content`): geometry, the image plane, and
    /// the visual properties.  `clip`, `order`, `renderable`, the name and the
    /// tree edge stay with the destination, so restoring a projected layer
    /// only undoes what the projection wrote.
    pub fn copy_render_state_from(&mut self, source: &LayerNode) {
        self.left = source.left;
        self.top = source.top;
        self.width = source.width;
        self.height = source.height;
        self.image_left = source.image_left;
        self.image_top = source.image_top;
        self.image_width = source.image_width;
        self.image_height = source.image_height;
        self.visible = source.visible;
        self.enabled = source.enabled;
        self.node_enabled = source.node_enabled;
        self.opacity = source.opacity;
        self.layer_type = source.layer_type;
        self.face = source.face;
        self.hit_type = source.hit_type;
        self.hit_threshold = source.hit_threshold;
        self.image = source.image.clone();
        self.province = source.province.clone();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerTree {
    layers: BTreeMap<LayerId, LayerNode>,
    next_layer_id: LayerId,
    /// Current modal layer (`tTVPLayerManager::GetCurrentModalLayer`,
    /// `LayerManager.cpp:926`): `Layer.setMode()` pushes it and
    /// `Layer.removeMode()` pops it.  While one is set, every layer that is
    /// not an ancestor-or-self of it is disabled by mode
    /// (`tTJSNI_BaseLayer::IsDisabledByMode`, `LayerIntf.cpp:3416`), which
    /// both `NodeEnabled` and `GetMostFrontChildAt` consult.
    modal_layer: Option<LayerId>,
}

impl Default for LayerTree {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerTree {
    pub fn new() -> Self {
        Self {
            layers: BTreeMap::new(),
            next_layer_id: 1,
            modal_layer: None,
        }
    }

    pub fn modal_layer(&self) -> Option<LayerId> {
        self.modal_layer
    }

    pub fn set_modal_layer(&mut self, layer: Option<LayerId>) {
        self.modal_layer = layer.filter(|id| self.layers.contains_key(id));
    }

    /// `tTJSNI_BaseLayer::IsDisabledByMode` (`LayerIntf.cpp:3416`):
    /// `!IsAncestorOrSelf(currentModalLayer)`, and the official
    /// `IsAncestorOrSelf(ancestor)` (`LayerIntf.h:256`) asks whether the
    /// *argument* is an ancestor of `this`.  So while a modal layer is set,
    /// only that layer's own subtree stays hittable; everything else -- the
    /// modal layer's ancestors and every unrelated layer -- is disabled, which
    /// is what makes `GetMostFrontChildAt` report "no layer at this point"
    /// (`LayerIntf.cpp:3000`) instead of delivering to the background.
    pub fn is_disabled_by_mode(&self, id: LayerId) -> bool {
        match self
            .modal_layer
            .filter(|modal| self.layers.contains_key(modal))
        {
            Some(modal) => !self.is_ancestor_or_self(modal, id),
            None => false,
        }
    }

    /// True when `ancestor` is `id` itself or one of its ancestors.
    pub fn is_ancestor_or_self(&self, ancestor: LayerId, id: LayerId) -> bool {
        let mut current = Some(id);
        while let Some(node) = current.and_then(|id| self.layers.get(&id)) {
            if node.id == ancestor {
                return true;
            }
            current = node.parent;
        }
        false
    }

    /// `tTJSNI_BaseLayer::GetNodeEnabled` (`LayerIntf.h:651`):
    /// `GetEnabled() && ParentEnabled() && !IsDisabledByMode()`.
    /// `ParentEnabled()` walks every ancestor's own `Enabled`
    /// (`LayerIntf.cpp:3489`), so disabling a layer disables its whole subtree
    /// without touching the children's own flags -- which is why this must be
    /// computed from the live tree rather than from a cached per-node flag.
    pub fn node_enabled(&self, id: LayerId) -> bool {
        if self.is_disabled_by_mode(id) {
            return false;
        }
        let mut current = Some(id);
        while let Some(node) = current.and_then(|id| self.layers.get(&id)) {
            if !node.enabled {
                return false;
            }
            current = node.parent;
        }
        true
    }

    /// `tTJSNI_BaseLayer::GetNodeVisible` (`LayerIntf.h:308`):
    /// `GetParentVisible() && Visible` -- the layer and every ancestor must be
    /// visible. Opacity is not consulted.
    pub fn node_visible(&self, id: LayerId) -> bool {
        let mut current = Some(id);
        while let Some(node) = current.and_then(|id| self.layers.get(&id)) {
            if !node.visible {
                return false;
            }
            current = node.parent;
        }
        true
    }

    pub fn create_layer(
        &mut self,
        name: impl Into<String>,
        parent: Option<LayerId>,
        z_order: i32,
    ) -> LayerId {
        let id = self.next_layer_id;
        self.next_layer_id = self.next_layer_id.saturating_add(1);
        self.layers
            .insert(id, LayerNode::new(id, name, parent, z_order));
        id
    }

    pub fn layer(&self, id: LayerId) -> Option<&LayerNode> {
        self.layers.get(&id)
    }

    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut LayerNode> {
        self.layers.get_mut(&id)
    }

    pub fn layers(&self) -> impl Iterator<Item = &LayerNode> {
        self.layers.values()
    }

    pub fn remove_layer(&mut self, id: LayerId) -> Option<LayerNode> {
        self.layers.remove(&id)
    }

    /// Put a previously-created layer id back in the tree.
    ///
    /// Official `tTJSNI_BaseLayer` stays allocated for the TJS Layer object's
    /// lifetime (`LayerIntf.cpp`). Kirakira must not lose the node while that
    /// handle is still live — GINKA `StandLayer` / `PSDLayer` dests were
    /// dropped after construction, so `hasImage` / `FillRect` / `CopyRect`
    /// became no-ops and stands never composited.
    pub fn ensure_layer(
        &mut self,
        id: LayerId,
        name: impl Into<String>,
        parent: Option<LayerId>,
        z_order: i32,
    ) -> bool {
        if self.layers.contains_key(&id) {
            return false;
        }
        self.layers
            .insert(id, LayerNode::new(id, name, parent, z_order));
        if id >= self.next_layer_id {
            self.next_layer_id = id.saturating_add(1);
        }
        true
    }

    pub fn set_parent(&mut self, id: LayerId, parent: Option<LayerId>) -> bool {
        if parent == Some(id) || parent.is_some_and(|parent| self.is_descendant(parent, id)) {
            return false;
        }
        let Some(layer) = self.layers.get_mut(&id) else {
            return false;
        };
        layer.parent = parent;
        true
    }

    /// Position of `id` among its parent's children in draw order, the index
    /// the official `GetOrderIndex()` reports (`LayerIntf.h:278`, computed from
    /// the parent's `Children` vector). `None` when the layer is unknown; a
    /// parentless layer is filed among the other roots the render tree keeps
    /// side by side, so callers that need `if(!Parent) return 0` filter first.
    pub fn order_index(&self, id: LayerId) -> Option<usize> {
        let node = self.layers.get(&id)?;
        Some(
            self.sorted_children(node.parent)
                .iter()
                .position(|child| child.id == id)?,
        )
    }

    pub fn absolute_position(&self, id: LayerId) -> Option<Point> {
        let mut x = 0.0;
        let mut y = 0.0;
        let mut current = Some(id);
        while let Some(layer_id) = current {
            let layer = self.layers.get(&layer_id)?;
            x += layer.left + layer.window_offset.x;
            y += layer.top + layer.window_offset.y;
            current = layer.parent;
        }
        Some(Point::new(x, y))
    }

    pub fn hit_test(&self, point: Point) -> Option<LayerId> {
        self.hit_test_all(point).into_iter().next()
    }

    pub fn hit_test_all(&self, point: Point) -> Vec<LayerId> {
        let mut hits = Vec::new();
        let roots = self.sorted_children(None);
        for root in roots.into_iter().rev() {
            if self.hit_test_layer_all(root.id, Point::new(0.0, 0.0), point, &mut hits)
                == HitTestOutcome::Blocked
            {
                break;
            }
        }
        hits
    }

    pub fn draw_model(&self) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        self.draw_model_filtered(|_| true)
    }

    pub fn draw_model_suppressing_images(
        &self,
        suppressed_images: &BTreeSet<LayerId>,
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        self.draw_model_filtered_suppressing_images(|_| true, suppressed_images)
    }

    pub fn draw_model_filtered<F>(&self, filter: F) -> (Vec<DrawCommand>, Vec<ImageUpload>)
    where
        F: FnMut(&LayerNode) -> bool,
    {
        self.draw_model_filtered_suppressing_images(filter, &BTreeSet::new())
    }

    pub fn draw_model_filtered_suppressing_images<F>(
        &self,
        mut filter: F,
        suppressed_images: &BTreeSet<LayerId>,
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>)
    where
        F: FnMut(&LayerNode) -> bool,
    {
        let mut model = LayerDrawModel {
            suppressed_images,
            commands: Vec::new(),
            uploads: Vec::new(),
        };
        let clip = Rect::new(0.0, 0.0, f32::MAX / 4.0, f32::MAX / 4.0);
        for root in self.sorted_children(None) {
            self.draw_layer(
                root.id,
                Point::new(0.0, 0.0),
                clip,
                1.0,
                &mut filter,
                &mut model,
            );
        }
        (model.commands, model.uploads)
    }

    /// The transition source face: the content of `roots`, shifted by `offset`.
    ///
    /// Official hands the handler the source layer's cached bitmap
    /// (`tTJSNI_BaseLayer::Complete(rect)`, `LayerIntf.cpp:6104`) and
    /// `tTransDrawable::DrawCompleted` (`:6596-6613`) aligns that bitmap with
    /// the destination's, so the source face is the source subtree drawn where
    /// the destination is.  `extra_roots` carries the rest of a KAG staging
    /// page, whose layers are independent objects rather than one subtree.
    ///
    /// `source`'s own `Visible` is ignored: KAG transitions out of a page that
    /// is never displayed directly (`fore.base` transitions from `back.base`),
    /// and official renders that cache all the same.  Every other layer,
    /// including the other staging-page layers, keeps its own visibility and
    /// opacity exactly as the ordinary completion pass draws them.
    pub fn source_face(
        &self,
        source: LayerId,
        extra_roots: &[LayerId],
        offset: Point,
        with_children: bool,
    ) -> (Vec<DrawCommand>, Vec<ImageUpload>) {
        let suppressed_images = BTreeSet::new();
        let mut model = LayerDrawModel {
            suppressed_images: &suppressed_images,
            commands: Vec::new(),
            uploads: Vec::new(),
        };
        let clip = Rect::new(0.0, 0.0, f32::MAX / 4.0, f32::MAX / 4.0);
        for (index, root) in std::iter::once(&source).chain(extra_roots).enumerate() {
            let Some(node) = self.layers.get(root) else {
                continue;
            };
            // The source layer's own state is the transition's; the rest of the
            // page keeps its ordinary drawing rules.
            let is_source = index == 0;
            if !is_source && (!node.visible || node.opacity == 0) {
                continue;
            }
            let origin = self
                .absolute_position(*root)
                .map(|origin| Point::new(origin.x + offset.x, origin.y + offset.y))
                .unwrap_or(offset);
            self.draw_source_face(
                node.id,
                // `draw_source_face` adds the node's own offset, so the walk
                // starts from its origin minus that offset.
                Point::new(
                    origin.x - node.left - node.window_offset.x,
                    origin.y - node.top - node.window_offset.y,
                ),
                clip,
                1.0,
                with_children,
                &mut model,
                is_source,
            );
        }
        (model.commands, model.uploads)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_source_face(
        &self,
        id: LayerId,
        parent_origin: Point,
        parent_clip: Rect,
        parent_opacity: f32,
        with_children: bool,
        model: &mut LayerDrawModel<'_>,
        ignore_own_visibility: bool,
    ) {
        let Some(layer) = self.layers.get(&id) else {
            return;
        };
        // `renderable` is an engine bookkeeping flag (a parted layer, or a KAG
        // page that is not the displayed one), not an official drawing
        // property: `tTJSNI_BaseLayer::Complete` (`LayerIntf.cpp:6104`) renders
        // the source layer's cache whatever the engine thinks of its page.
        if !ignore_own_visibility && (layer.opacity == 0 || !layer.visible) {
            return;
        }
        let origin = Point::new(
            parent_origin.x + layer.left + layer.window_offset.x,
            parent_origin.y + layer.top + layer.window_offset.y,
        );
        let layer_rect = Rect::new(origin.x, origin.y, layer.width, layer.height);
        let Some(clip) = intersect_rect(parent_clip, layer_rect) else {
            return;
        };
        let opacity = parent_opacity * layer.opacity as f32 / 255.0;

        if let Some(command) = layer.image_command(origin, clip, parent_opacity) {
            model.commands.push(DrawCommand::Image(command));
            if let Some(image) = &layer.image {
                model.uploads.push(image.upload.clone());
            }
        }

        if !with_children {
            return;
        }
        for child in self.sorted_children(Some(id)) {
            self.draw_source_face(child.id, origin, clip, opacity, with_children, model, false);
        }
    }

    fn draw_layer<F>(
        &self,
        id: LayerId,
        parent_origin: Point,
        parent_clip: Rect,
        parent_opacity: f32,
        filter: &mut F,
        model: &mut LayerDrawModel<'_>,
    ) where
        F: FnMut(&LayerNode) -> bool,
    {
        let Some(layer) = self.layers.get(&id) else {
            return;
        };
        if !filter(layer) {
            return;
        }
        if !layer.renderable || !layer.visible || layer.opacity == 0 {
            return;
        }
        let origin = Point::new(
            parent_origin.x + layer.left + layer.window_offset.x,
            parent_origin.y + layer.top + layer.window_offset.y,
        );
        let layer_rect = Rect::new(origin.x, origin.y, layer.width, layer.height);
        let Some(clip) = intersect_rect(parent_clip, layer_rect) else {
            return;
        };
        let opacity = parent_opacity * layer.opacity as f32 / 255.0;

        if !model.suppressed_images.contains(&id)
            && let Some(command) = layer.image_command(origin, clip, parent_opacity)
        {
            model.commands.push(DrawCommand::Image(command));
            if let Some(image) = &layer.image {
                model.uploads.push(image.upload.clone());
            }
        }

        for child in self.sorted_children(Some(id)) {
            self.draw_layer(child.id, origin, clip, opacity, filter, model);
        }
    }

    fn hit_test_layer_all(
        &self,
        id: LayerId,
        parent_origin: Point,
        point: Point,
        hits: &mut Vec<LayerId>,
    ) -> HitTestOutcome {
        let Some(layer) = self.layers.get(&id) else {
            return HitTestOutcome::None;
        };
        // Only `Visible` and the rectangle gate a hit. Opacity is a drawing
        // property: official `GetMostFrontChildAt` (`LayerIntf.cpp:2967`)
        // checks `Visible` alone, and `GetNodeVisible()` is explicitly
        // documented as "this does not check opacity" (`LayerIntf.h:308`).
        // `IsSeen()` -- visible *and* non-zero opacity -- is used by the draw
        // pass only. KAGEX buttons fade a state image to opacity 0 while the
        // pointer is on them, so honouring opacity here drops the layer out
        // from under the cursor and makes the hover state oscillate.
        if !layer.renderable || !layer.visible || layer.width <= 0.0 || layer.height <= 0.0 {
            return HitTestOutcome::None;
        }

        let origin = Point::new(
            parent_origin.x + layer.left + layer.window_offset.x,
            parent_origin.y + layer.top + layer.window_offset.y,
        );
        let rect = Rect::new(origin.x, origin.y, layer.width, layer.height);
        if !rect.contains(point) {
            return HitTestOutcome::None;
        }

        for child in self.sorted_children(Some(id)).into_iter().rev() {
            match self.hit_test_layer_all(child.id, origin, point, hits) {
                HitTestOutcome::Blocked => return HitTestOutcome::Blocked,
                HitTestOutcome::Hit | HitTestOutcome::None => {}
            }
        }

        if layer.hit_test(origin, point) {
            // `GetMostFrontChildAt` only reports "disabled or under a modal
            // layer" once the layer's own mask/province test passed
            // (`LayerIntf.cpp:2996`): a disabled layer that is transparent at
            // this point stays transparent instead of swallowing the event,
            // while a hit on a disabled layer blocks lower siblings with a
            // NULL result.  `GetNodeEnabled()` is `GetEnabled() &&
            // ParentEnabled() && !IsDisabledByMode()` and is recomputed on
            // every query (`LayerIntf.h:651`).
            if !self.node_enabled(id) {
                return HitTestOutcome::Blocked;
            }
            hits.push(id);
            return HitTestOutcome::Hit;
        }
        HitTestOutcome::None
    }

    fn sorted_children(&self, parent: Option<LayerId>) -> Vec<&LayerNode> {
        let mut children = self
            .layers
            .values()
            .filter(|layer| layer.parent == parent)
            .collect::<Vec<_>>();
        children.sort_by_key(|layer| (layer.z_order, layer.id));
        children
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HitTestOutcome {
    None,
    Hit,
    Blocked,
}

impl LayerTree {
    fn is_descendant(&self, id: LayerId, ancestor: LayerId) -> bool {
        let mut current = Some(id);
        while let Some(layer_id) = current {
            if layer_id == ancestor {
                return true;
            }
            current = self.layers.get(&layer_id).and_then(|layer| layer.parent);
        }
        false
    }
}

struct LayerDrawModel<'a> {
    suppressed_images: &'a BTreeSet<LayerId>,
    commands: Vec<DrawCommand>,
    uploads: Vec<ImageUpload>,
}

fn collect_image_texture_ids(commands: &[DrawCommand], texture_ids: &mut BTreeSet<TextureId>) {
    for command in commands {
        if let DrawCommand::Image(image) = command {
            texture_ids.insert(image.texture_id);
        }
    }
}

fn intersect_rect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessageLayerModel {
    pub lines: Vec<String>,
    pub waiting_for_click: bool,
    pub page: usize,
    pub font: FontSpec,
    pub style: TextStyle,
    pub cursor_x: i32,
    pub cursor_y: i32,
}

impl MessageLayerModel {
    pub fn clear(&mut self) {
        self.lines.clear();
        self.waiting_for_click = false;
        self.page = 0;
        self.font = FontSpec::default();
        self.style = TextStyle::default();
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    pub fn clear_text(&mut self) {
        self.lines.clear();
    }

    pub fn append_text(&mut self, text: &str) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        if let Some(line) = self.lines.last_mut() {
            line.push_str(text);
        }
    }

    pub fn newline(&mut self) {
        self.lines.push(String::new());
    }

    pub fn page_break(&mut self) {
        self.page = self.page.saturating_add(1);
        self.waiting_for_click = true;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SafeAreaInsets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    #[default]
    Landscape,
    Portrait,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInput {
    pub viewport_size: Size,
    pub delta_seconds: f32,
    pub safe_area: SafeAreaInsets,
    pub orientation: Orientation,
}

impl FrameInput {
    pub const fn new(viewport_size: Size, delta_seconds: f32) -> Self {
        Self {
            viewport_size,
            delta_seconds,
            safe_area: SafeAreaInsets {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            },
            orientation: Orientation::Landscape,
        }
    }

    pub const fn with_safe_area(mut self, safe_area: SafeAreaInsets) -> Self {
        self.safe_area = safe_area;
        self
    }

    pub const fn with_orientation(mut self, orientation: Orientation) -> Self {
        self.orientation = orientation;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    pub initial_viewport: Size,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            initial_viewport: Size::new(960.0, 600.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
    Other(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineKey {
    Escape,
    Enter,
    Space,
    Tab,
    Left,
    Up,
    Right,
    Down,
    PageUp,
    PageDown,
    Backspace,
    Delete,
    Shift,
    Control,
    Alt,
    Character(char),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineEvent {
    CursorMoved {
        position: Point,
    },
    PointerInput {
        button: PointerButton,
        state: ButtonState,
    },
    MouseWheel {
        delta: i32,
    },
    KeyboardInput {
        key: EngineKey,
        state: ButtonState,
        repeat: bool,
    },
    /// Touch/pointer input with a stable contact id.  Mouse adapters can keep
    /// using `PointerInput`; mobile adapters should preserve ids across move
    /// events so gesture recognisers do not have to infer them from position.
    TouchInput {
        id: u64,
        position: Point,
        phase: TouchPhase,
    },
    /// Application/surface state transition delivered by the host.
    Lifecycle {
        state: LifecycleState,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextInputEvent {
    pub text: String,
    pub composing: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleState {
    Foreground,
    Background,
    SurfaceSuspended,
    SurfaceResumed,
    MemoryPressure,
    AudioInterrupted,
    AudioResumed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Panel {
    Launcher,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiElement {
    Start,
    OpenProject,
    Settings,
    Back,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiAction {
    LaunchRequested,
    OpenProjectRequested,
    SettingsOpened,
    SettingsClosed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LauncherViewModel {
    pub panel: Panel,
    pub hovered: Option<UiElement>,
    pub pressed: Option<UiElement>,
    pub last_action: Option<UiAction>,
    pub launch_requests: u32,
    pub open_project_requests: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiLayout {
    pub top_bar: Rect,
    pub side_rail: Rect,
    pub settings_nav: Rect,
    pub content: Rect,
    pub hero: Rect,
    pub start_button: Rect,
    pub open_project_button: Rect,
    pub settings_button: Rect,
    pub back_button: Rect,
    panel: Panel,
}

impl UiLayout {
    pub fn new(size: Size, panel: Panel) -> Self {
        let width = size.width.max(320.0);
        let height = size.height.max(360.0);
        let top_h = 56.0;
        let side_w = if width >= 560.0 { 76.0 } else { 0.0 };
        let margin = if width >= 560.0 { 28.0 } else { 16.0 };
        let gap = 16.0;
        let content_x = side_w + margin;
        let content_y = top_h + margin;
        let content_w = (width - content_x - margin).max(0.0);
        let content_h = (height - content_y - margin).max(0.0);
        let hero_h = content_h.mul_add(0.28, 0.0).clamp(92.0, 150.0);
        let hero = Rect::new(content_x, content_y, content_w, hero_h);
        let tile_y = hero.y + hero.height + 28.0;

        let (start_button, settings_button) = if content_w >= 560.0 {
            let start_w = (content_w - gap) * 0.62;
            (
                Rect::new(content_x, tile_y, start_w, 132.0),
                Rect::new(
                    content_x + start_w + gap,
                    tile_y,
                    content_w - start_w - gap,
                    132.0,
                ),
            )
        } else {
            (
                Rect::new(content_x, tile_y, content_w, 112.0),
                Rect::new(content_x, tile_y + 128.0, content_w, 92.0),
            )
        };

        let open_project_button = Rect::new(
            start_button.x + 18.0,
            start_button.y + start_button.height - 42.0,
            (start_button.width - 36.0).max(0.0),
            24.0,
        );
        let settings_nav = Rect::new(18.0, top_h + 22.0, 40.0, 40.0);
        let back_button = Rect::new(content_x, content_y + hero.height + 20.0, 104.0, 42.0);

        Self {
            top_bar: Rect::new(0.0, 0.0, width, top_h),
            side_rail: Rect::new(0.0, top_h, side_w, height - top_h),
            settings_nav,
            content: Rect::new(content_x, content_y, content_w, content_h),
            hero,
            start_button,
            open_project_button,
            settings_button,
            back_button,
            panel,
        }
    }

    pub fn hit_test(&self, point: Point) -> Option<UiElement> {
        if self.panel == Panel::Settings && self.back_button.contains(point) {
            return Some(UiElement::Back);
        }

        if self.panel == Panel::Launcher {
            if self.open_project_button.contains(point) {
                return Some(UiElement::OpenProject);
            }
            if self.start_button.contains(point) {
                return Some(UiElement::Start);
            }
            if self.settings_button.contains(point) || self.settings_nav.contains(point) {
                return Some(UiElement::Settings);
            }
        } else if self.settings_nav.contains(point) {
            return Some(UiElement::Settings);
        }

        None
    }
}

#[derive(Debug)]
pub struct Engine {
    viewport_size: Size,
    uploaded_images: BTreeMap<TextureId, UploadedImageState>,
    cursor_position: Option<Point>,
    panel: Panel,
    hovered: Option<UiElement>,
    pressed: Option<UiElement>,
    last_action: Option<UiAction>,
    status_level: Option<StatusLevel>,
    launch_requests: u32,
    open_project_requests: u32,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            viewport_size: config.initial_viewport,
            uploaded_images: BTreeMap::new(),
            cursor_position: None,
            panel: Panel::Launcher,
            hovered: None,
            pressed: None,
            last_action: None,
            status_level: None,
            launch_requests: 0,
            open_project_requests: 0,
        }
    }

    pub fn handle_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::CursorMoved { position } => {
                self.cursor_position = Some(position);
                self.update_hover();
            }
            EngineEvent::PointerInput {
                button: PointerButton::Primary,
                state: ButtonState::Pressed,
            } => {
                self.update_hover();
                self.pressed = self.hovered;
            }
            EngineEvent::PointerInput {
                button: PointerButton::Primary,
                state: ButtonState::Released,
            } => {
                self.update_hover();
                let pressed = self.pressed.take();
                if pressed.is_some() && pressed == self.hovered {
                    self.activate(pressed);
                }
            }
            EngineEvent::KeyboardInput {
                key: EngineKey::Escape,
                state: ButtonState::Pressed,
                ..
            } => {
                if self.panel == Panel::Settings {
                    self.panel = Panel::Launcher;
                    self.last_action = Some(UiAction::SettingsClosed);
                    self.update_hover();
                }
            }
            EngineEvent::KeyboardInput {
                key: EngineKey::Enter | EngineKey::Space,
                state: ButtonState::Pressed,
                ..
            } => {
                self.activate(self.hovered);
            }
            EngineEvent::PointerInput { .. }
            | EngineEvent::MouseWheel { .. }
            | EngineEvent::KeyboardInput { .. }
            | EngineEvent::TouchInput { .. }
            | EngineEvent::Lifecycle { .. } => {}
        }
    }

    pub fn tick(&mut self, input: FrameInput) -> FrameOutput {
        if !input.viewport_size.is_empty() {
            self.viewport_size = input.viewport_size;
        }
        self.update_hover();

        let layout = UiLayout::new(self.viewport_size, self.panel);
        let mut draw_commands = Vec::with_capacity(28);
        self.draw_shell(&mut draw_commands, layout);

        match self.panel {
            Panel::Launcher => self.draw_launcher(&mut draw_commands, layout),
            Panel::Settings => self.draw_settings(&mut draw_commands, layout),
        }

        FrameOutput::new(palette::BACKGROUND, draw_commands)
    }

    pub fn tick_running(&mut self, input: FrameInput) -> FrameOutput {
        self.tick_running_with_message(input, &MessageLayerModel::default())
    }

    pub fn tick_running_with_message(
        &mut self,
        input: FrameInput,
        message: &MessageLayerModel,
    ) -> FrameOutput {
        if !input.viewport_size.is_empty() {
            self.viewport_size = input.viewport_size;
        }

        let layout = UiLayout::new(self.viewport_size, Panel::Launcher);
        let mut draw_commands = Vec::with_capacity(24);
        self.draw_shell(&mut draw_commands, layout);
        self.draw_running(&mut draw_commands, layout, message);

        FrameOutput::new(palette::RUNTIME_BACKGROUND, draw_commands)
    }

    pub fn tick_running_with_layers(
        &mut self,
        input: FrameInput,
        layers: &LayerTree,
        message: &MessageLayerModel,
    ) -> FrameOutput {
        self.tick_running_with_layers_suppressing_images(input, layers, message, &BTreeSet::new())
    }

    pub fn tick_running_with_layers_suppressing_images(
        &mut self,
        input: FrameInput,
        layers: &LayerTree,
        message: &MessageLayerModel,
        suppressed_images: &BTreeSet<LayerId>,
    ) -> FrameOutput {
        let output =
            self.running_layer_frame_output(input, layers, message, suppressed_images, None);
        self.finalize_frame_output(output)
    }

    pub fn tick_running_with_layers_suppressing_images_and_transitions(
        &mut self,
        input: FrameInput,
        layers: &LayerTree,
        message: &MessageLayerModel,
        suppressed_images: &BTreeSet<LayerId>,
        transitions: Vec<FrameTransition>,
    ) -> FrameOutput {
        let output =
            self.running_layer_frame_output(input, layers, message, suppressed_images, transitions);
        self.finalize_frame_output(output)
    }

    fn running_layer_frame_output(
        &mut self,
        input: FrameInput,
        layers: &LayerTree,
        message: &MessageLayerModel,
        suppressed_images: &BTreeSet<LayerId>,
        transitions: impl IntoIterator<Item = FrameTransition>,
    ) -> FrameOutput {
        if !input.viewport_size.is_empty() {
            self.viewport_size = input.viewport_size;
        }

        let (mut draw_commands, image_uploads) =
            layers.draw_model_suppressing_images(suppressed_images);
        self.draw_message_overlay(&mut draw_commands, message);

        FrameOutput::new(palette::RUNTIME_BACKGROUND, draw_commands)
            .with_image_uploads(image_uploads)
            .with_transitions(transitions.into_iter().collect())
    }

    fn filter_new_image_uploads(&mut self, uploads: Vec<ImageUpload>) -> Vec<ImageUpload> {
        uploads
            .into_iter()
            .filter(|upload| {
                let state = UploadedImageState {
                    width: upload.width,
                    height: upload.height,
                    rgba: upload.rgba.clone(),
                };
                if self
                    .uploaded_images
                    .get(&upload.texture_id)
                    .is_some_and(|previous| {
                        previous.width == state.width
                            && previous.height == state.height
                            && Arc::ptr_eq(&previous.rgba, &state.rgba)
                    })
                {
                    return false;
                }
                self.uploaded_images.insert(upload.texture_id, state);
                true
            })
            .collect()
    }

    fn finalize_frame_output(&mut self, mut output: FrameOutput) -> FrameOutput {
        output.image_uploads = self.filter_new_image_uploads(output.image_uploads);
        for transition in &mut output.transitions {
            transition.frozen_image_uploads =
                self.filter_new_image_uploads(std::mem::take(&mut transition.frozen_image_uploads));
            transition.source_image_uploads =
                self.filter_new_image_uploads(std::mem::take(&mut transition.source_image_uploads));
            transition.under_image_uploads =
                self.filter_new_image_uploads(std::mem::take(&mut transition.under_image_uploads));
            if let Some(upload) = transition.rule_image_upload.take() {
                let mut uploads = self.filter_new_image_uploads(vec![upload]);
                transition.rule_image_upload = uploads.pop();
            }
        }
        let mut referenced_textures = BTreeSet::new();
        collect_image_texture_ids(&output.draw_commands, &mut referenced_textures);
        for transition in &output.transitions {
            collect_image_texture_ids(&transition.frozen_draw_commands, &mut referenced_textures);
            collect_image_texture_ids(&transition.under_draw_commands, &mut referenced_textures);
            collect_image_texture_ids(&transition.source_draw_commands, &mut referenced_textures);
            if let Some(texture_id) = transition.rule_texture_id {
                referenced_textures.insert(texture_id);
            }
        }
        let released = self
            .uploaded_images
            .keys()
            .copied()
            .filter(|texture_id| !referenced_textures.contains(texture_id))
            .collect::<Vec<_>>();
        for texture_id in &released {
            self.uploaded_images.remove(texture_id);
        }
        output.image_releases = released;
        output
    }

    pub fn view_model(&self) -> LauncherViewModel {
        LauncherViewModel {
            panel: self.panel,
            hovered: self.hovered,
            pressed: self.pressed,
            last_action: self.last_action,
            launch_requests: self.launch_requests,
            open_project_requests: self.open_project_requests,
        }
    }

    pub fn layout(&self) -> UiLayout {
        UiLayout::new(self.viewport_size, self.panel)
    }

    pub fn panel(&self) -> Panel {
        self.panel
    }

    pub fn set_panel(&mut self, panel: Panel) {
        if self.panel == panel {
            return;
        }

        self.panel = panel;
        self.pressed = None;
        self.update_hover();
    }

    pub fn set_status_level(&mut self, level: Option<StatusLevel>) {
        self.status_level = level;
    }

    pub fn take_last_action(&mut self) -> Option<UiAction> {
        self.last_action.take()
    }

    fn activate(&mut self, element: Option<UiElement>) {
        match element {
            Some(UiElement::Start) => {
                self.launch_requests = self.launch_requests.saturating_add(1);
                self.last_action = Some(UiAction::LaunchRequested);
            }
            Some(UiElement::OpenProject) => {
                self.open_project_requests = self.open_project_requests.saturating_add(1);
                self.last_action = Some(UiAction::OpenProjectRequested);
            }
            Some(UiElement::Settings) => {
                self.panel = Panel::Settings;
                self.last_action = Some(UiAction::SettingsOpened);
                self.pressed = None;
                self.update_hover();
            }
            Some(UiElement::Back) => {
                self.panel = Panel::Launcher;
                self.last_action = Some(UiAction::SettingsClosed);
                self.pressed = None;
                self.update_hover();
            }
            None => {}
        }
    }

    fn update_hover(&mut self) {
        self.hovered = self
            .cursor_position
            .and_then(|point| UiLayout::new(self.viewport_size, self.panel).hit_test(point));
    }

    fn draw_shell(&self, commands: &mut Vec<DrawCommand>, layout: UiLayout) {
        rect(commands, layout.top_bar, palette::TOP_BAR);
        if layout.side_rail.width > 0.0 {
            rect(commands, layout.side_rail, palette::SIDE_RAIL);
            rect(
                commands,
                layout.settings_nav,
                self.element_color(UiElement::Settings),
            );
            let mark = layout.settings_nav.inset(12.0);
            rect(commands, mark, palette::ACCENT_GREEN);
        }

        let traffic_y = 20.0;
        rect(
            commands,
            Rect::new(20.0, traffic_y, 12.0, 12.0),
            palette::ACCENT_RED,
        );
        rect(
            commands,
            Rect::new(42.0, traffic_y, 12.0, 12.0),
            palette::ACCENT_YELLOW,
        );
        rect(
            commands,
            Rect::new(64.0, traffic_y, 12.0, 12.0),
            palette::ACCENT_GREEN,
        );
        rect(
            commands,
            Rect::new(layout.top_bar.width - 184.0, 18.0, 128.0, 20.0),
            palette::TOP_BAR_LINE,
        );

        if let Some(level) = self.status_level {
            let color = match level {
                StatusLevel::Info => palette::ACCENT_BLUE,
                StatusLevel::Warning => palette::ACCENT_YELLOW,
                StatusLevel::Error => palette::ACCENT_RED,
            };
            rect(commands, Rect::new(96.0, 18.0, 72.0, 20.0), color);
        }
    }

    fn draw_launcher(&self, commands: &mut Vec<DrawCommand>, layout: UiLayout) {
        rect(commands, layout.hero, palette::PANEL);
        rect(commands, layout.hero.inset(18.0), palette::PANEL_INSET);
        rect(
            commands,
            Rect::new(layout.hero.x + 28.0, layout.hero.y + 28.0, 180.0, 22.0),
            palette::ACCENT_BLUE,
        );
        rect(
            commands,
            Rect::new(
                layout.hero.x + 28.0,
                layout.hero.y + 66.0,
                (layout.hero.width * 0.52).max(80.0),
                14.0,
            ),
            palette::MUTED_LINE,
        );
        rect(
            commands,
            Rect::new(
                layout.hero.x + 28.0,
                layout.hero.y + 92.0,
                (layout.hero.width * 0.38).max(64.0),
                14.0,
            ),
            palette::MUTED_LINE,
        );

        rect(
            commands,
            layout.start_button,
            self.element_color(UiElement::Start),
        );
        rect(
            commands,
            Rect::new(
                layout.start_button.x + 18.0,
                layout.start_button.y + 18.0,
                layout.start_button.width * 0.42,
                20.0,
            ),
            palette::ON_ACTION,
        );
        rect(
            commands,
            layout.open_project_button,
            self.element_color(UiElement::OpenProject),
        );

        rect(
            commands,
            layout.settings_button,
            self.element_color(UiElement::Settings),
        );
        let center_x = layout.settings_button.x + layout.settings_button.width * 0.5 - 18.0;
        rect(
            commands,
            Rect::new(center_x, layout.settings_button.y + 28.0, 36.0, 36.0),
            palette::ACCENT_YELLOW,
        );
        rect(
            commands,
            Rect::new(
                layout.settings_button.x + 20.0,
                layout.settings_button.y + layout.settings_button.height - 34.0,
                (layout.settings_button.width - 40.0).max(0.0),
                14.0,
            ),
            palette::MUTED_LINE,
        );

        let strip_y = layout.start_button.y + layout.start_button.height + 28.0;
        for index in 0..3 {
            rect(
                commands,
                Rect::new(
                    layout.content.x + (index as f32 * 126.0),
                    strip_y,
                    96.0,
                    18.0,
                ),
                palette::PANEL_INSET,
            );
        }
    }

    fn draw_settings(&self, commands: &mut Vec<DrawCommand>, layout: UiLayout) {
        rect(commands, layout.hero, palette::PANEL);
        rect(
            commands,
            Rect::new(layout.hero.x + 28.0, layout.hero.y + 30.0, 154.0, 24.0),
            palette::ACCENT_YELLOW,
        );
        rect(
            commands,
            layout.back_button,
            self.element_color(UiElement::Back),
        );
        rect(
            commands,
            Rect::new(
                layout.back_button.x + 18.0,
                layout.back_button.y + 14.0,
                42.0,
                14.0,
            ),
            palette::ON_ACTION,
        );

        let rows_top = layout.back_button.y + layout.back_button.height + 28.0;
        for index in 0..4 {
            let row_y = rows_top + index as f32 * 56.0;
            rect(
                commands,
                Rect::new(layout.content.x, row_y, layout.content.width, 40.0),
                palette::PANEL_INSET,
            );
            rect(
                commands,
                Rect::new(layout.content.x + 18.0, row_y + 13.0, 180.0, 14.0),
                palette::MUTED_LINE,
            );
            rect(
                commands,
                Rect::new(
                    layout.content.x + layout.content.width - 74.0,
                    row_y + 10.0,
                    48.0,
                    20.0,
                ),
                if index % 2 == 0 {
                    palette::ACCENT_GREEN
                } else {
                    palette::MUTED_LINE
                },
            );
        }
    }

    fn draw_running(
        &self,
        commands: &mut Vec<DrawCommand>,
        layout: UiLayout,
        message: &MessageLayerModel,
    ) {
        let stage_margin = if layout.content.width >= 640.0 {
            32.0
        } else {
            16.0
        };
        let stage = layout.content.inset(stage_margin);
        rect(commands, stage, palette::STAGE);

        let stage_inner = fit_rect(stage.inset(18.0), 16.0 / 9.0);
        rect(commands, stage_inner, palette::STAGE_INNER);

        rect(
            commands,
            Rect::new(
                stage_inner.x + 24.0,
                stage_inner.y + stage_inner.height - 82.0,
                (stage_inner.width - 48.0).max(0.0),
                70.0,
            ),
            palette::TEXT_BOX,
        );
        rect(
            commands,
            Rect::new(stage_inner.x + 48.0, stage_inner.y + 40.0, 120.0, 20.0),
            palette::ACCENT_GREEN,
        );
        rect(
            commands,
            Rect::new(
                stage_inner.x + 48.0,
                stage_inner.y + 76.0,
                (stage_inner.width * 0.42).max(80.0),
                14.0,
            ),
            palette::MUTED_LINE,
        );

        let text_x = stage_inner.x + 42.0;
        let mut text_y = stage_inner.y + stage_inner.height - 66.0;
        let first_line = message.lines.len().saturating_sub(3);
        for line in message.lines.iter().skip(first_line) {
            text(
                commands,
                Point::new(text_x, text_y),
                line,
                palette::TEXT,
                18.0,
                Some((&message.font, &message.style)),
            );
            text_y += 21.0;
        }
        if message.waiting_for_click {
            rect(
                commands,
                Rect::new(
                    stage_inner.x + stage_inner.width - 58.0,
                    stage_inner.y + stage_inner.height - 36.0,
                    12.0,
                    12.0,
                ),
                palette::ACCENT_YELLOW,
            );
        }

        let meter_y = layout.content.y + layout.content.height - 22.0;
        for index in 0..5 {
            rect(
                commands,
                Rect::new(
                    layout.content.x + index as f32 * 34.0,
                    meter_y,
                    20.0,
                    8.0 + index as f32 * 2.0,
                ),
                palette::PANEL_INSET,
            );
        }
    }

    fn draw_message_overlay(&self, commands: &mut Vec<DrawCommand>, message: &MessageLayerModel) {
        if message.lines.is_empty() && !message.waiting_for_click {
            return;
        }

        let width = self.viewport_size.width.max(320.0);
        let height = self.viewport_size.height.max(240.0);
        let margin = if width >= 640.0 { 42.0 } else { 20.0 };
        let box_height = 116.0_f32.min((height - margin * 2.0).max(64.0));
        let box_rect = Rect::new(
            margin,
            (height - box_height - margin).max(margin),
            (width - margin * 2.0).max(0.0),
            box_height,
        );
        rect(commands, box_rect, palette::TEXT_BOX);

        let text_x = box_rect.x + 24.0;
        let mut text_y = box_rect.y + 20.0;
        let first_line = message.lines.len().saturating_sub(3);
        for line in message.lines.iter().skip(first_line) {
            text(
                commands,
                Point::new(text_x, text_y),
                line,
                palette::TEXT,
                18.0,
                Some((&message.font, &message.style)),
            );
            text_y += 23.0;
        }
        if message.waiting_for_click {
            rect(
                commands,
                Rect::new(
                    box_rect.x + box_rect.width - 30.0,
                    box_rect.y + box_rect.height - 30.0,
                    12.0,
                    12.0,
                ),
                palette::ACCENT_YELLOW,
            );
        }
    }

    fn element_color(&self, element: UiElement) -> Color {
        if self.pressed == Some(element) {
            return palette::PRESSED;
        }
        if self.hovered == Some(element) {
            return palette::HOVERED;
        }

        match element {
            UiElement::Start => palette::ACTION,
            UiElement::OpenProject => palette::ACTION_SECONDARY,
            UiElement::Settings | UiElement::Back => palette::CONTROL,
        }
    }
}

fn rect(commands: &mut Vec<DrawCommand>, rect: Rect, color: Color) {
    if rect.width > 0.0 && rect.height > 0.0 {
        commands.push(DrawCommand::Rect(RectCommand { rect, color }));
    }
}

fn text(
    commands: &mut Vec<DrawCommand>,
    position: Point,
    text: &str,
    color: Color,
    size: f32,
    font_style: Option<(&FontSpec, &TextStyle)>,
) {
    if !text.is_empty() && size > 0.0 {
        let (font, style) = match font_style {
            Some((font, style)) => (font.clone(), *style),
            None => {
                let font = FontSpec {
                    height: size,
                    ..FontSpec::default()
                };
                let style = TextStyle {
                    color: color_to_u8(color),
                    ..TextStyle::default()
                };
                (font, style)
            }
        };
        commands.push(DrawCommand::Text(TextCommand {
            position,
            text: text.to_string(),
            color,
            size,
            font,
            style,
        }));
    }
}

fn color_to_u8(color: Color) -> [u8; 4] {
    [
        (color.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.a.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn fit_rect(bounds: Rect, aspect_ratio: f32) -> Rect {
    if bounds.width <= 0.0 || bounds.height <= 0.0 || aspect_ratio <= 0.0 {
        return Rect::default();
    }

    let bounds_ratio = bounds.width / bounds.height;
    if bounds_ratio > aspect_ratio {
        let width = bounds.height * aspect_ratio;
        Rect::new(
            bounds.x + (bounds.width - width) * 0.5,
            bounds.y,
            width,
            bounds.height,
        )
    } else {
        let height = bounds.width / aspect_ratio;
        Rect::new(
            bounds.x,
            bounds.y + (bounds.height - height) * 0.5,
            bounds.width,
            height,
        )
    }
}

mod palette {
    use super::Color;

    pub const BACKGROUND: Color = Color::rgb_u8(18, 20, 23);
    pub const RUNTIME_BACKGROUND: Color = Color::rgb_u8(10, 12, 14);
    pub const TOP_BAR: Color = Color::rgb_u8(32, 35, 40);
    pub const TOP_BAR_LINE: Color = Color::rgb_u8(67, 74, 84);
    pub const SIDE_RAIL: Color = Color::rgb_u8(25, 28, 32);
    pub const PANEL: Color = Color::rgb_u8(42, 48, 54);
    pub const PANEL_INSET: Color = Color::rgb_u8(57, 65, 73);
    pub const STAGE: Color = Color::rgb_u8(20, 23, 28);
    pub const STAGE_INNER: Color = Color::rgb_u8(12, 14, 18);
    pub const TEXT_BOX: Color = Color::new(0.07, 0.08, 0.1, 0.84);
    pub const TEXT: Color = Color::rgb_u8(232, 238, 242);
    pub const MUTED_LINE: Color = Color::rgb_u8(105, 116, 128);
    pub const CONTROL: Color = Color::rgb_u8(77, 86, 96);
    pub const HOVERED: Color = Color::rgb_u8(103, 118, 132);
    pub const PRESSED: Color = Color::rgb_u8(70, 151, 137);
    pub const ACTION: Color = Color::rgb_u8(37, 130, 177);
    pub const ACTION_SECONDARY: Color = Color::rgb_u8(72, 91, 109);
    pub const ON_ACTION: Color = Color::rgb_u8(224, 230, 235);
    pub const ACCENT_BLUE: Color = Color::rgb_u8(77, 163, 214);
    pub const ACCENT_GREEN: Color = Color::rgb_u8(76, 175, 140);
    pub const ACCENT_YELLOW: Color = Color::rgb_u8(232, 181, 83);
    pub const ACCENT_RED: Color = Color::rgb_u8(214, 90, 82);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_inside_points_only() {
        let rect = Rect::new(10.0, 20.0, 100.0, 50.0);

        assert!(rect.contains(Point::new(10.0, 20.0)));
        assert!(rect.contains(Point::new(109.9, 69.9)));
        assert!(!rect.contains(Point::new(110.0, 70.0)));
        assert!(!rect.contains(Point::new(9.9, 20.0)));
    }

    #[test]
    fn launcher_layout_hits_interactive_regions() {
        let layout = UiLayout::new(Size::new(960.0, 600.0), Panel::Launcher);

        assert_eq!(
            layout.hit_test(Point::new(
                layout.start_button.x + 12.0,
                layout.start_button.y + 12.0
            )),
            Some(UiElement::Start)
        );
        assert_eq!(
            layout.hit_test(Point::new(
                layout.open_project_button.x + 4.0,
                layout.open_project_button.y + 4.0
            )),
            Some(UiElement::OpenProject)
        );
        assert_eq!(
            layout.hit_test(Point::new(
                layout.settings_button.x + 12.0,
                layout.settings_button.y + 12.0
            )),
            Some(UiElement::Settings)
        );
    }

    #[test]
    fn clicking_settings_switches_panel() {
        let mut engine = Engine::new(EngineConfig::default());
        let layout = engine.layout();
        let point = Point::new(
            layout.settings_button.x + 8.0,
            layout.settings_button.y + 8.0,
        );

        engine.handle_event(EngineEvent::CursorMoved { position: point });
        engine.handle_event(EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Pressed,
        });
        engine.handle_event(EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Released,
        });

        let view_model = engine.view_model();
        assert_eq!(view_model.panel, Panel::Settings);
        assert_eq!(view_model.last_action, Some(UiAction::SettingsOpened));
        assert_eq!(engine.take_last_action(), Some(UiAction::SettingsOpened));
        assert_eq!(engine.take_last_action(), None);
    }

    #[test]
    fn redraw_between_press_and_release_keeps_press_state() {
        let mut engine = Engine::new(EngineConfig::default());
        let layout = engine.layout();
        let point = Point::new(layout.start_button.x + 8.0, layout.start_button.y + 8.0);

        engine.handle_event(EngineEvent::CursorMoved { position: point });
        engine.handle_event(EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Pressed,
        });
        engine.set_panel(Panel::Launcher);
        let _frame = engine.tick(FrameInput::new(Size::new(960.0, 600.0), 0.016));
        engine.handle_event(EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Released,
        });

        assert_eq!(engine.take_last_action(), Some(UiAction::LaunchRequested));
    }

    #[test]
    fn draw_list_reflects_hover_state() {
        let mut engine = Engine::new(EngineConfig::default());
        let idle = engine.tick(FrameInput::new(Size::new(960.0, 600.0), 0.0));
        let layout = engine.layout();

        engine.handle_event(EngineEvent::CursorMoved {
            position: Point::new(layout.start_button.x + 10.0, layout.start_button.y + 10.0),
        });
        let hovered = engine.tick(FrameInput::new(Size::new(960.0, 600.0), 0.0));

        assert_eq!(idle.draw_commands.len(), hovered.draw_commands.len());
        assert_ne!(idle.draw_commands, hovered.draw_commands);
        assert_eq!(engine.view_model().hovered, Some(UiElement::Start));
    }

    #[test]
    fn running_frame_draws_stage_shell() {
        let mut engine = Engine::new(EngineConfig::default());
        let frame = engine.tick_running(FrameInput::new(Size::new(960.0, 600.0), 0.0));

        assert_eq!(frame.clear_color, palette::RUNTIME_BACKGROUND);
        assert!(frame.draw_commands.len() >= 10);
        assert_eq!(engine.panel(), Panel::Launcher);
    }

    #[test]
    fn running_frame_draws_message_text() {
        let mut engine = Engine::new(EngineConfig::default());
        let mut message = MessageLayerModel::default();
        message.append_text("Hello");
        message.newline();
        message.append_text("World");

        let frame = engine
            .tick_running_with_message(FrameInput::new(Size::new(960.0, 600.0), 0.0), &message);

        assert!(
            frame
                .draw_commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Text(text) if text.text == "Hello"))
        );
        assert!(
            frame
                .draw_commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Text(text) if text.text == "World"))
        );
    }

    #[test]
    fn running_layer_uploads_are_emitted_only_once_per_texture() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255, 255, 255, 255]);
        let image = LayerImage::new(42, 1, 1, pixels);
        let mut layers = LayerTree::new();
        let id = layers.create_layer("image", None, 0);
        {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 1.0;
            layer.height = 1.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let mut engine = Engine::new(EngineConfig::default());
        let first = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );
        let second = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );

        assert_eq!(first.image_uploads.len(), 1);
        assert!(second.image_uploads.is_empty());
        assert_eq!(first.draw_commands, second.draw_commands);
    }

    #[test]
    fn running_layer_reuploads_replaced_pixels_with_the_same_texture_id() {
        let mut layers = LayerTree::new();
        let id = layers.create_layer("image", None, 0);
        {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 1.0;
            layer.height = 1.0;
            layer.visible = true;
            layer.set_image(LayerImage::new(42, 1, 1, Arc::from([255, 0, 0, 255])));
        }

        let mut engine = Engine::new(EngineConfig::default());
        let first = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );
        layers
            .layer_mut(id)
            .expect("layer")
            .set_image(LayerImage::new(42, 1, 1, Arc::from([0, 255, 0, 255])));
        let second = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );

        assert_eq!(first.image_uploads.len(), 1);
        assert_eq!(second.image_uploads.len(), 1);
        assert_eq!(second.image_uploads[0].rgba.as_ref(), &[0, 255, 0, 255]);
    }

    #[test]
    fn running_layer_uploads_are_reemitted_after_texture_leaves_frame() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255, 255, 255, 255]);
        let image = LayerImage::new(42, 1, 1, pixels);
        let mut layers = LayerTree::new();
        let id = layers.create_layer("image", None, 0);
        {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 1.0;
            layer.height = 1.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let mut engine = Engine::new(EngineConfig::default());
        let first = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );
        layers.layer_mut(id).expect("layer").visible = false;
        let hidden = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );
        layers.layer_mut(id).expect("layer").visible = true;
        let visible_again = engine.tick_running_with_layers(
            FrameInput::new(Size::new(320.0, 240.0), 0.0),
            &layers,
            &MessageLayerModel::default(),
        );

        assert_eq!(first.image_uploads.len(), 1);
        assert!(hidden.image_uploads.is_empty());
        assert_eq!(hidden.image_releases, vec![42]);
        assert_eq!(visible_again.image_uploads.len(), 1);
        assert_eq!(visible_again.image_uploads[0].texture_id, 42);
    }

    #[test]
    fn layer_tree_draw_model_sorts_visible_images_by_z_order() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255, 255, 255, 255]);
        let low_image = LayerImage::new(1, 1, 1, pixels.clone());
        let high_image = LayerImage::new(2, 1, 1, pixels);
        let mut layers = LayerTree::new();
        let high = layers.create_layer("high", None, 20);
        let low = layers.create_layer("low", None, 10);

        {
            let layer = layers.layer_mut(high).expect("high layer");
            layer.left = 20.0;
            layer.top = 30.0;
            layer.visible = true;
            layer.set_image(high_image);
        }
        {
            let layer = layers.layer_mut(low).expect("low layer");
            layer.left = 2.0;
            layer.top = 3.0;
            layer.visible = true;
            layer.set_image(low_image);
        }

        let (commands, uploads) = layers.draw_model();

        assert_eq!(uploads.len(), 2);
        assert!(matches!(
            &commands[..],
            [
                DrawCommand::Image(first),
                DrawCommand::Image(second)
            ] if first.texture_id == 1 && second.texture_id == 2
        ));
    }

    #[test]
    fn layer_tree_suppresses_layer_image_without_hiding_children() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255, 255, 255, 255]);
        let parent_image = LayerImage::new(1, 1, 1, pixels.clone());
        let child_image = LayerImage::new(2, 1, 1, pixels);
        let mut layers = LayerTree::new();
        let parent = layers.create_layer("parent", None, 0);
        let child = layers.create_layer("child", Some(parent), 0);
        for (id, image) in [(parent, parent_image), (child, child_image)] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 1.0;
            layer.height = 1.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let (commands, uploads) = layers.draw_model_suppressing_images(&BTreeSet::from([parent]));

        assert_eq!(uploads.len(), 1);
        assert!(matches!(
            &commands[..],
            [DrawCommand::Image(image)] if image.texture_id == 2
        ));
    }

    #[test]
    fn layer_tree_draw_model_filtered_excludes_subtree() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255, 255, 255, 255]);
        let root_image = LayerImage::new(1, 1, 1, pixels.clone());
        let parent_image = LayerImage::new(2, 1, 1, pixels.clone());
        let child_image = LayerImage::new(3, 1, 1, pixels);
        let mut layers = LayerTree::new();
        let root = layers.create_layer("root", None, 0);
        let parent = layers.create_layer("kag:message0", None, 10);
        let child = layers.create_layer("child", Some(parent), 0);
        for (id, image, left) in [
            (root, root_image, 1.0),
            (parent, parent_image, 2.0),
            (child, child_image, 3.0),
        ] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.left = left;
            layer.width = 1.0;
            layer.height = 1.0;
            layer.visible = true;
            layer.opacity = 128;
            layer.set_image(image);
        }

        let (commands, uploads) =
            layers.draw_model_filtered(|layer| !layer.name.starts_with("kag:message"));

        assert_eq!(uploads.len(), 1);
        assert!(matches!(
            &commands[..],
            [DrawCommand::Image(image)]
                if image.texture_id == 1
                    && image.rect == Rect::new(1.0, 0.0, 1.0, 1.0)
                    && (image.opacity - 128.0 / 255.0).abs() < 0.001
        ));
    }

    #[test]
    fn layer_tree_draw_model_clips_negative_image_offsets_for_sprite_sheets() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255; 12 * 4]);
        let image = LayerImage::new(7, 12, 1, pixels);
        let mut layers = LayerTree::new();
        let id = layers.create_layer("button", None, 10);
        {
            let layer = layers.layer_mut(id).expect("button layer");
            layer.left = 100.0;
            layer.top = 20.0;
            layer.width = 4.0;
            layer.height = 1.0;
            layer.image_left = -4.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let (commands, uploads) = layers.draw_model();

        assert_eq!(uploads.len(), 1);
        assert!(matches!(
            &commands[..],
            [DrawCommand::Image(image)]
                if image.rect == Rect::new(100.0, 20.0, 4.0, 1.0)
                    && image.source_rect == Rect::new(4.0, 0.0, 4.0, 1.0)
                    && image.texture_size == Size::new(12.0, 1.0)
        ));
    }

    #[test]
    fn layer_tree_draw_model_clips_positive_image_offsets_to_layer_viewport() {
        let pixels = std::sync::Arc::<[u8]>::from(vec![255; 8 * 4]);
        let image = LayerImage::new(9, 8, 1, pixels);
        let mut layers = LayerTree::new();
        let id = layers.create_layer("inset", None, 10);
        {
            let layer = layers.layer_mut(id).expect("inset layer");
            layer.left = 10.0;
            layer.top = 5.0;
            layer.width = 4.0;
            layer.height = 1.0;
            layer.image_left = 2.0;
            layer.visible = true;
            layer.set_image(image);
        }

        let (commands, _uploads) = layers.draw_model();

        assert!(matches!(
            &commands[..],
            [DrawCommand::Image(image)]
                if image.rect == Rect::new(12.0, 5.0, 2.0, 1.0)
                    && image.source_rect == Rect::new(0.0, 0.0, 2.0, 1.0)
        ));
    }

    #[test]
    fn layer_tree_hit_test_returns_topmost_visible_layer() {
        let mut layers = LayerTree::new();
        let low = layers.create_layer("low", None, 10);
        let high = layers.create_layer("high", None, 20);
        for id in [low, high] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.left = 10.0;
            layer.top = 20.0;
            layer.width = 30.0;
            layer.height = 40.0;
            layer.visible = true;
        }

        assert_eq!(layers.hit_test(Point::new(15.0, 25.0)), Some(high));
        assert_eq!(layers.hit_test(Point::new(50.0, 25.0)), None);

        layers.layer_mut(high).expect("high").visible = false;
        assert_eq!(layers.hit_test(Point::new(15.0, 25.0)), Some(low));
    }

    #[test]
    fn hit_testing_ignores_opacity_like_the_reference_engine() {
        // `GetMostFrontChildAt` (`LayerIntf.cpp:2967`) gates a hit on `Visible`
        // alone, and `GetNodeVisible()` is documented as "this does not check
        // opacity" (`LayerIntf.h:308`); `IsSeen()` (visible *and* non-zero
        // opacity) belongs to the draw pass. KAGEX buttons fade their state
        // image to opacity 0 while the pointer rests on them, so an opacity
        // gate here drops the hover target out from under the cursor.
        let mut layers = LayerTree::new();
        let low = layers.create_layer("low", None, 1);
        let high = layers.create_layer("high", None, 2);
        for id in [low, high] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 4.0;
            layer.height = 4.0;
            layer.visible = true;
        }
        layers.layer_mut(high).expect("high").opacity = 0;

        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), Some(high));
    }

    #[test]
    fn disabled_layers_block_only_after_their_own_hit_test_passes() {
        // `GetMostFrontChildAt` reports "disabled or under a modal layer" only
        // once the layer's own mask test succeeded (`LayerIntf.cpp:2996`), so
        // a disabled layer that is transparent at this point must stay
        // transparent instead of swallowing the event.
        let mut layers = LayerTree::new();
        let low = layers.create_layer("low", None, 1);
        let high = layers.create_layer("high", None, 2);
        for id in [low, high] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 2.0;
            layer.height = 2.0;
            layer.visible = true;
        }
        layers.layer_mut(high).expect("high").hit_threshold = 16;

        // No image and a threshold of 16: the high layer is transparent here.
        layers.layer_mut(high).expect("high").enabled = false;
        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), Some(low));

        // An opaque disabled layer hits its own mask and therefore blocks.
        layers
            .layer_mut(high)
            .expect("high")
            .set_image(LayerImage::new(
                1,
                2,
                2,
                Arc::from([
                    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
                ]),
            ));
        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), None);
    }

    #[test]
    fn disabling_a_parent_disables_the_whole_subtree_for_hit_testing() {
        // `GetNodeEnabled()` is `GetEnabled() && ParentEnabled() &&
        // !IsDisabledByMode()` and is recomputed on every query
        // (`LayerIntf.h:651`); `ParentEnabled()` walks every ancestor's own
        // `Enabled` (`LayerIntf.cpp:3489`).
        let mut layers = LayerTree::new();
        let low = layers.create_layer("low", None, 0);
        let parent = layers.create_layer("parent", None, 1);
        let child = layers.create_layer("child", Some(parent), 1);
        for id in [parent, child, low] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 4.0;
            layer.height = 4.0;
            layer.visible = true;
        }
        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), Some(child));
        assert!(layers.node_enabled(child));

        layers.layer_mut(parent).expect("parent").enabled = false;
        assert!(!layers.node_enabled(child));
        // The child's own mask still passes, so official reports "disabled"
        // rather than "no hit": the disabled subtree blocks the lower sibling
        // instead of letting the event fall through (`LayerIntf.cpp:2996`).
        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), None);
    }

    #[test]
    fn a_modal_layer_disables_everything_outside_its_own_subtree() {
        // `IsDisabledByMode()` is `!IsAncestorOrSelf(currentModalLayer)`
        // (`LayerIntf.cpp:3416`): only the modal layer's own subtree keeps
        // hitting; its ancestors and unrelated layers are mode-disabled.
        let mut layers = LayerTree::new();
        let background = layers.create_layer("background", None, 1);
        let modal = layers.create_layer("modal", None, 2);
        let modal_child = layers.create_layer("modal_child", Some(modal), 1);
        for id in [background, modal, modal_child] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 4.0;
            layer.height = 4.0;
            layer.visible = true;
        }

        layers.set_modal_layer(Some(modal));
        assert!(layers.is_disabled_by_mode(background));
        assert!(!layers.is_disabled_by_mode(modal));
        assert!(!layers.is_disabled_by_mode(modal_child));
        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), Some(modal_child));

        layers.set_modal_layer(None);
        assert!(!layers.is_disabled_by_mode(background));
    }

    #[test]
    fn province_layers_hit_test_through_the_province_plane() {
        let mut layers = LayerTree::new();
        let id = layers.create_layer("province", None, 1);
        let layer = layers.layer_mut(id).expect("layer");
        layer.width = 2.0;
        layer.height = 2.0;
        layer.visible = true;
        // htProvince
        layer.hit_type = 1;
        layer.province = Some(ProvinceImage::new(2, 2, vec![0, 0, 0, 5]));

        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), Some(id));
        assert_eq!(layers.hit_test(Point::new(0.0, 0.0)), None);

        // `tTJSNI_BaseLayer::_HitTestNoVisibleCheck` returns false without a
        // province plane.
        layers.layer_mut(id).expect("layer").province = None;
        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), None);
    }

    #[test]
    fn ordinary_layers_use_krkr_mask_threshold_and_disabled_layers_block_lower_siblings() {
        let mut layers = LayerTree::new();
        let low = layers.create_layer("low", None, 1);
        let high = layers.create_layer("high", None, 2);
        for id in [low, high] {
            let layer = layers.layer_mut(id).expect("layer");
            layer.width = 2.0;
            layer.height = 2.0;
            layer.visible = true;
            layer.set_image(LayerImage::new(
                id as u64,
                2,
                2,
                Arc::from([
                    255, 255, 255, 8, 255, 255, 255, 8, 255, 255, 255, 8, 255, 255, 255, 8,
                ]),
            ));
        }
        layers.layer_mut(low).expect("low").hit_threshold = 16;
        layers.layer_mut(high).expect("high").hit_threshold = 16;
        assert_eq!(layers.layer(low).expect("low").hit_threshold, 16);
        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), None);
        layers.layer_mut(high).expect("high").hit_threshold = 0;
        layers.layer_mut(high).expect("high").enabled = false;
        assert_eq!(layers.hit_test(Point::new(1.0, 1.0)), None);
    }

    #[test]
    fn fit_rect_preserves_aspect_ratio_inside_bounds() {
        let bounds = Rect::new(0.0, 0.0, 400.0, 400.0);
        let fitted = fit_rect(bounds, 16.0 / 9.0);

        assert!(bounds.contains(Point::new(fitted.x, fitted.y)));
        assert!(fitted.width <= bounds.width);
        assert!(fitted.height <= bounds.height);
        assert!((fitted.width / fitted.height - 16.0 / 9.0).abs() < 0.001);
    }

    #[test]
    fn memory_asset_store_completes_requests_on_poll() {
        let mut store = MemoryAssetStore::default();
        store.insert("startup.tjs", Arc::<[u8]>::from(&b"var ready = 1;"[..]));
        let id = store.request("startup.tjs", AssetKind::Text);
        assert!(store.poll().iter().any(|event| matches!(
            event,
            AssetEvent::Ready { id: event_id, path, kind: AssetKind::Text, data }
                if *event_id == id && path == "startup.tjs" && data.as_ref() == b"var ready = 1;"
        )));
    }

    #[test]
    fn virtual_clock_never_moves_backwards() {
        let mut clock = VirtualClock::new(100);
        clock.advance_millis(25);
        clock.advance_millis(-10);
        assert_eq!(clock.now_millis(), 125);
    }

    #[test]
    fn memory_save_store_is_profile_scoped_and_pollable() {
        let mut store = MemorySaveStore::default();
        let save_id = store.save("player-a", "slot-1", Arc::from(&b"save"[..]));
        assert!(
            matches!(store.poll().as_slice(), [SaveEvent::Saved { id, profile, key }]
            if *id == save_id && profile == "player-a" && key == "slot-1")
        );

        let load_id = store.load("player-a", "slot-1");
        assert!(
            matches!(store.poll().as_slice(), [SaveEvent::Loaded { id, data: Some(data), .. }]
            if *id == load_id && data.as_ref() == b"save")
        );
        let missing_id = store.load("player-b", "slot-1");
        assert!(
            matches!(store.poll().as_slice(), [SaveEvent::Loaded { id, data: None, .. }]
            if *id == missing_id)
        );
    }

    /// The provider lookup is the reference's hash find (`TransIntf.cpp:341-359`):
    /// every registered name resolves to its kernel, and the match is exact --
    /// `"Wave"` is not the registered `"wave"` and is unknown to the reference.
    #[test]
    fn transition_name_lookup_is_the_reference_exact_match() {
        for (name, method) in TRANSITION_PROVIDER_NAMES {
            assert_eq!(TransitionMethod::try_from_name(name), Ok(*method));
            assert_eq!(method.as_name(), *name);
        }

        assert_eq!(
            TransitionMethod::try_from_name("Wave"),
            Err(UnknownTransitionName::new("Wave"))
        );
        // Names only a loaded plugin registers stay unknown until the plugin
        // host publishes its provider registry (see `TRANSITION_PROVIDER_NAMES`).
        assert!(TransitionMethod::try_from_name("blurfade").is_err());
        assert!(TransitionMethod::try_from_name("glitch").is_err());
        // The lenient mapping the engine still uses keeps its old behaviour:
        // case-folded, and anything unknown silently becomes a crossfade.
        assert_eq!(TransitionMethod::from_name("Wave"), TransitionMethod::Wave);
        assert_eq!(
            TransitionMethod::from_name("blurfade"),
            TransitionMethod::Crossfade
        );
        assert_eq!(TransitionMethod::from_name(""), TransitionMethod::Crossfade);
    }

    /// The unknown-name path produces the reference's exact text: `%1` is the
    /// caller's own spelling (`TVPFormatMessage`, `MsgIntf.cpp:115-121`), and the
    /// exception is message-only -- `TVPCannotFindTransHander` carries no
    /// `tjs_error` code and no trace (`TransIntf.cpp:354`, `tjsError.h:115-146`).
    #[test]
    fn unknown_transition_name_reports_the_official_message() {
        let error = TransitionMethod::try_from_name("furu-furu").expect_err("unknown name");
        assert_eq!(error.name, "furu-furu");
        assert_eq!(error.message(), "Cannot find transition handler furu-furu");
        assert_eq!(
            error.to_string(),
            "Cannot find transition handler furu-furu"
        );

        let spaced =
            TransitionMethod::try_from_name("wave ").expect_err("trailing space is a miss");
        assert_eq!(spaced.message(), "Cannot find transition handler wave ");
    }

    /// The extrans kernels run on the millisecond clock the reference stores in
    /// `Time`/`CurTime`; `0` is the documented "not supplied" value and nothing
    /// else in the parameter set depends on the field.
    #[test]
    fn transition_duration_clock_defaults_to_not_supplied() {
        let params = TransitionParams::default();
        assert_eq!(params.duration_millis, 0.0);
        assert_eq!(
            TransitionParams {
                duration_millis: 1000.0,
                ..TransitionParams::default()
            }
            .duration_millis,
            1000.0
        );
    }
}
