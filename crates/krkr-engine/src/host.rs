use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque, btree_map::Entry},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use krkr_core::{
    AssetKind, AudioBus, AudioCommand, AudioInstanceId, AudioLoadPolicy, AudioSourceRef,
    DrawCommand, FrameTransition, ImageUpload, LayerId, LayerImage, LayerNode, LayerTree,
    LifecycleState, Point, ResourceData, StoragePort, TextInputEvent, TextureId, TransitionParams,
};
use krkr_font::FontSystem;
use krkr_kag::KagParser;
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, TjsHost, Variant},
};

use crate::{
    native::video::VideoOverlayState,
    resource_manager::{DecodedImageData, ResourceManager, ResourceTaskId, decode_image_bytes},
    scheduler::{AsyncTriggerMode, TvpScheduler},
    storage::{
        ProjectStorage, decode_text_storage, io_error as storage_io_error,
        normalize_storage_separators as storage_normalize_separators, storage_mode_offset,
    },
};

const IMAGE_CACHE_CAPACITY_BYTES: usize = 128 * 1024 * 1024;
const IMAGE_CACHE_MAX_ENTRY_BYTES: usize = 32 * 1024 * 1024;

/// Bounds the in-memory host log; long headless runs with trace categories
/// enabled would otherwise grow it without limit. When the cap is hit the
/// oldest half is dropped in one drain.
const MAX_LOG_LINES: usize = 200_000;

#[cfg(not(target_arch = "wasm32"))]
struct ReceiverPcmStream {
    receiver: std::sync::mpsc::Receiver<krkr_core::PcmAudioChunk>,
}

#[cfg(not(target_arch = "wasm32"))]
impl krkr_core::PcmStream for ReceiverPcmStream {
    fn next_chunk(&mut self) -> Option<krkr_core::PcmAudioChunk> {
        self.receiver.recv_timeout(Duration::from_secs(2)).ok()
    }
}

/// High-frequency diagnostic categories, enabled through the `KRKR_TRACE`
/// environment variable (comma-separated, case-insensitive; `all` enables
/// everything). Trace lines are tagged `[<category>]` in the host log.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceCategory {
    /// WaveSoundBuffer and KAG audio lifecycle (open/play/stop/fade).
    Audio,
    /// KAG tag dispatch (unknown-tag routing, unhandled tags).
    Kag,
    /// Pointer/keyboard routing and native control activation.
    Input,
}

impl TraceCategory {
    fn bit(self) -> u8 {
        match self {
            TraceCategory::Audio => TRACE_AUDIO,
            TraceCategory::Kag => TRACE_KAG,
            TraceCategory::Input => TRACE_INPUT,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            TraceCategory::Audio => "audio",
            TraceCategory::Kag => "kag",
            TraceCategory::Input => "input",
        }
    }
}

const TRACE_AUDIO: u8 = 1 << 0;
const TRACE_KAG: u8 = 1 << 1;
const TRACE_INPUT: u8 = 1 << 2;
const TRACE_ALL: u8 = TRACE_AUDIO | TRACE_KAG | TRACE_INPUT;

/// Parses a `KRKR_TRACE`-style category list; unknown names are ignored.
fn parse_trace_mask(value: &str) -> u8 {
    let mut mask = 0;
    for name in value.split(',').map(str::trim) {
        if name.is_empty() {
            continue;
        }
        mask |= match name.to_ascii_lowercase().as_str() {
            "all" => TRACE_ALL,
            "audio" => TRACE_AUDIO,
            "kag" => TRACE_KAG,
            "input" => TRACE_INPUT,
            _ => continue,
        };
    }
    mask
}

fn trace_mask_from_env() -> u8 {
    std::env::var("KRKR_TRACE")
        .map(|value| parse_trace_mask(&value))
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TransitionPolicy {
    #[default]
    Animated,
    Immediate,
}

/// Host-provided paths exposed through the KRKR `System` object.
///
/// These are deliberately strings rather than filesystem paths: desktop
/// supplies native paths, while Web/mobile hosts provide virtual paths or
/// application-specific save namespaces. The engine never discovers paths
/// from the process environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemPaths {
    pub exe_path: String,
    pub data_path: String,
    pub personal_path: String,
    pub app_data_path: String,
}

impl Default for SystemPaths {
    fn default() -> Self {
        Self {
            exe_path: String::new(),
            data_path: "savedata/".to_string(),
            personal_path: "/".to_string(),
            app_data_path: "/".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeTextDrawEvent {
    pub layer_id: Option<LayerId>,
    pub text: String,
    pub x: i64,
    pub y: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoOverlaySnapshot {
    pub storage: Option<String>,
    pub status: String,
    pub left: i64,
    pub top: i64,
    pub width: i64,
    pub height: i64,
    pub visible: bool,
    pub looping: bool,
    pub position_ms: i64,
    pub play_rate: f64,
    pub audio_volume: i64,
    pub audio_balance: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KagLayerSlot {
    pub page: String,
    pub layer: String,
}

impl KagLayerSlot {
    pub(crate) fn new(page: &str, layer: &str) -> Self {
        Self {
            page: normalize_kag_page(page).to_string(),
            layer: layer.to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LayerRenderTarget {
    Native(LayerId),
    Kag(KagLayerSlot),
}

#[derive(Clone, Debug)]
pub(crate) struct LayerInstance {
    pub layer_id: LayerId,
    pub window: Option<ObjectHandle>,
    pub parent: Option<ObjectHandle>,
    pub children: Vec<ObjectHandle>,
    pub children_array: Option<ObjectHandle>,
    pub render_target: LayerRenderTarget,
    properties: BTreeMap<String, Variant>,
}

impl LayerInstance {
    fn new(
        layer_id: LayerId,
        window: Option<ObjectHandle>,
        parent: Option<ObjectHandle>,
        children_array: Option<ObjectHandle>,
    ) -> Self {
        Self {
            layer_id,
            window,
            parent,
            children: Vec::new(),
            children_array,
            render_target: LayerRenderTarget::Native(layer_id),
            properties: BTreeMap::new(),
        }
    }

    fn property(&self, name: &str) -> Option<Variant> {
        match name {
            "window" => self
                .properties
                .get(name)
                .cloned()
                .or_else(|| self.window.map(Variant::Object)),
            "parent" => self
                .properties
                .get(name)
                .cloned()
                .or_else(|| self.parent.map(Variant::Object)),
            "children" => self
                .children_array
                .map(Variant::Object)
                .or_else(|| self.properties.get(name).cloned()),
            _ => self.properties.get(name).cloned(),
        }
    }

    fn set_property(&mut self, name: impl Into<String>, value: Variant) {
        self.properties.insert(name.into(), value);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WindowInstance {
    pub children: Vec<ObjectHandle>,
    pub children_array: Option<ObjectHandle>,
    pub primary_layer: Option<ObjectHandle>,
    pub focused_layer: Option<ObjectHandle>,
    pub visible: bool,
    pub closed: bool,
    pub modal: bool,
    properties: BTreeMap<String, Variant>,
}

impl WindowInstance {
    fn new(children_array: Option<ObjectHandle>) -> Self {
        Self {
            children: Vec::new(),
            children_array,
            primary_layer: None,
            focused_layer: None,
            visible: false,
            closed: false,
            modal: false,
            properties: BTreeMap::new(),
        }
    }

    fn property(&self, name: &str) -> Option<Variant> {
        match name {
            "children" => self
                .children_array
                .map(Variant::Object)
                .or_else(|| self.properties.get(name).cloned()),
            "primaryLayer" => self
                .primary_layer
                .map(Variant::Object)
                .or_else(|| self.properties.get(name).cloned()),
            "focusedLayer" => self
                .focused_layer
                .map(Variant::Object)
                .or_else(|| self.properties.get(name).cloned()),
            "visible" => Some(Variant::Integer(i64::from(self.visible))),
            "__nativeClosed" => Some(Variant::Integer(i64::from(self.closed))),
            "__nativeModal" => Some(Variant::Integer(i64::from(self.modal))),
            _ => self.properties.get(name).cloned(),
        }
    }

    fn set_property(&mut self, name: impl Into<String>, value: Variant) {
        self.properties.insert(name.into(), value);
    }
}

#[derive(Clone)]
pub struct KrkrHost {
    project_storage: Option<ProjectStorage>,
    system_paths: SystemPaths,
    resource_manager: Option<ResourceManager>,
    auto_paths: Vec<String>,
    logs: Vec<String>,
    trace_mask: u8,
    /// Invocation counts of stubbed native methods, keyed `Class.method`.
    stub_calls: BTreeMap<String, u64>,
    linked_plugins: BTreeSet<String>,
    kag_parsers: BTreeMap<ObjectHandle, KagParser>,
    kag_parser_revisions: BTreeMap<ObjectHandle, u64>,
    layer_tree: LayerTree,
    native_layers: BTreeMap<ObjectHandle, LayerInstance>,
    native_windows: BTreeMap<ObjectHandle, WindowInstance>,
    kag_layer_slots: BTreeMap<ObjectHandle, KagLayerSlot>,
    native_text_draw_events: Vec<NativeTextDrawEvent>,
    layer_image_storages: BTreeMap<LayerId, String>,
    scheduler: TvpScheduler,
    kag_layers: BTreeMap<String, LayerId>,
    pending_kag_layers: BTreeMap<String, LayerNode>,
    transition_policy: TransitionPolicy,
    active_transition: Option<ActiveTransition>,
    completed_native_transitions: Vec<NativeTransitionCompletion>,
    current_kag_page: String,
    current_kag_layer: String,
    image_cache: LayerImageCache,
    image_cache_revision: u64,
    pending_image_loads: BTreeMap<ResourceTaskId, PendingImageLoad>,
    pending_script_image_loads: BTreeMap<ResourceTaskId, (String, u64)>,
    /// Number of script-driven image decode completions observed since the
    /// last resource poll.  Script image loads suspend the TJS VM directly
    /// (unlike Layer.loadImages, which has an explicit continuation), so the
    /// engine uses this signal to resume that native call after the decoded
    /// image has been placed in the cache.
    completed_script_image_loads: usize,
    script_image_errors: BTreeMap<String, String>,
    completed_image_loads: Vec<CompletedImageLoad>,
    image_target_generations: BTreeMap<ImageLoadTarget, u64>,
    #[cfg_attr(test, allow(dead_code))]
    next_resource_generation: u64,
    font_system: FontSystem,
    next_texture_id: u64,
    next_audio_instance_id: u64,
    native_audio_buffers: BTreeMap<ObjectHandle, NativeAudioBuffer>,
    native_audio_global_volume: i64,
    pending_audio_commands: Vec<AudioCommand>,
    video_overlays: BTreeMap<ObjectHandle, VideoOverlayState>,
    text_encoding: String,
    pressed_keys: BTreeSet<i64>,
    cursor_position: Option<Point>,
    lifecycle_state: LifecycleState,
    pending_text_input: Vec<TextInputEvent>,
    clock_offset_millis: i64,
    termination_requested: bool,
    modal_windows: Vec<ObjectHandle>,
    external_resource_catalog: BTreeSet<String>,
    pending_external_resources: BTreeMap<String, AssetKind>,
    system_hooks: BTreeMap<String, SystemHookRegistration>,
}

#[derive(Clone, Debug)]
pub(crate) struct SystemHookRegistration {
    pub storage: Option<String>,
    pub target: Option<String>,
    pub call: bool,
}

impl Default for KrkrHost {
    fn default() -> Self {
        Self {
            project_storage: Some(ProjectStorage::new(None, Vec::new(), None, Vec::new())),
            system_paths: SystemPaths::default(),
            resource_manager: None,
            auto_paths: Vec::new(),
            logs: Vec::new(),
            trace_mask: trace_mask_from_env(),
            stub_calls: BTreeMap::new(),
            linked_plugins: BTreeSet::new(),
            kag_parsers: BTreeMap::new(),
            kag_parser_revisions: BTreeMap::new(),
            layer_tree: LayerTree::new(),
            native_layers: BTreeMap::new(),
            native_windows: BTreeMap::new(),
            kag_layer_slots: BTreeMap::new(),
            native_text_draw_events: Vec::new(),
            layer_image_storages: BTreeMap::new(),
            scheduler: TvpScheduler::default(),
            kag_layers: BTreeMap::new(),
            pending_kag_layers: BTreeMap::new(),
            transition_policy: TransitionPolicy::Animated,
            active_transition: None,
            completed_native_transitions: Vec::new(),
            current_kag_page: "fore".to_string(),
            current_kag_layer: "base".to_string(),
            image_cache: LayerImageCache::new(
                IMAGE_CACHE_CAPACITY_BYTES,
                IMAGE_CACHE_MAX_ENTRY_BYTES,
            ),
            image_cache_revision: 0,
            pending_image_loads: BTreeMap::new(),
            pending_script_image_loads: BTreeMap::new(),
            completed_script_image_loads: 0,
            script_image_errors: BTreeMap::new(),
            completed_image_loads: Vec::new(),
            image_target_generations: BTreeMap::new(),
            next_resource_generation: 1,
            font_system: FontSystem::new(),
            next_texture_id: 1,
            next_audio_instance_id: 1,
            native_audio_buffers: BTreeMap::new(),
            native_audio_global_volume: 100000,
            pending_audio_commands: Vec::new(),
            video_overlays: BTreeMap::new(),
            text_encoding: "UTF-8".to_string(),
            pressed_keys: BTreeSet::new(),
            cursor_position: None,
            lifecycle_state: LifecycleState::Foreground,
            pending_text_input: Vec::new(),
            clock_offset_millis: 0,
            termination_requested: false,
            modal_windows: Vec::new(),
            external_resource_catalog: BTreeSet::new(),
            pending_external_resources: BTreeMap::new(),
            system_hooks: BTreeMap::new(),
        }
    }
}

impl KrkrHost {
    pub fn with_system_paths(system_paths: SystemPaths) -> Self {
        let mut host = Self::default();
        host.system_paths = system_paths;
        host
    }

    /// Builds a host over an already materialized storage view (for example a
    /// browser package downloaded into memory). Resource workers are only
    /// started on native targets; WASM performs image/resource work on the
    /// browser thread.
    pub fn from_storage(storage: ProjectStorage, system_paths: SystemPaths) -> Result<Self> {
        let mut host = Self::default();
        host.image_cache_revision = storage.revision();
        host.project_storage = Some(storage.clone());
        host.system_paths = system_paths;
        #[cfg(not(target_arch = "wasm32"))]
        {
            host.resource_manager = Some(ResourceManager::new(storage).map_err(|error| {
                TjsError::runtime(format!("failed to start resource worker: {error}"))
            })?);
        }
        Ok(host)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn push_modal_window(&mut self, window: ObjectHandle) {
        self.modal_windows.push(window);
    }

    pub(crate) fn current_modal_window(&self) -> Option<ObjectHandle> {
        self.modal_windows.last().copied()
    }

    /// Returns the current modal window's host state for platform diagnostics.
    pub fn modal_window_snapshot(&self) -> Option<(bool, bool, bool, Option<ObjectHandle>)> {
        let handle = self.current_modal_window()?;
        let window = self.native_windows.get(&handle)?;
        Some((
            window.visible,
            window.closed,
            window.modal,
            window.primary_layer,
        ))
    }

    pub(crate) fn pop_modal_window(&mut self, window: ObjectHandle) {
        if self.modal_windows.last() == Some(&window) {
            self.modal_windows.pop();
        } else {
            self.modal_windows.retain(|entry| *entry != window);
        }
    }

    pub fn system_paths(&self) -> &SystemPaths {
        &self.system_paths
    }

    pub fn video_overlay_snapshots(&mut self) -> Vec<VideoOverlaySnapshot> {
        let now = self.now_millis();
        self.video_overlays
            .values()
            .map(|state| VideoOverlaySnapshot {
                storage: state.storage.clone(),
                status: state.status.to_string(),
                left: state.left,
                top: state.top,
                width: state.width,
                height: state.height,
                visible: state.visible,
                looping: state.looping,
                position_ms: state.elapsed_ms(now),
                play_rate: state.play_rate,
                audio_volume: state.audio_volume,
                audio_balance: state.audio_balance,
            })
            .collect()
    }

    pub fn project_storage(&self) -> Result<&ProjectStorage> {
        self.project_storage
            .as_ref()
            .ok_or_else(|| TjsError::runtime("project storage is not configured"))
    }

    /// Returns browser-memory storage writes accumulated by the engine. Native
    /// filesystem projects return an empty journal; Web hosts can persist the
    /// returned entries without coupling the engine to a browser database.
    pub fn drain_memory_storage_writes(&self) -> Vec<(String, Vec<u8>)> {
        self.project_storage
            .as_ref()
            .map(ProjectStorage::drain_memory_writes)
            .unwrap_or_default()
    }

    /// Registers logical resources known by a remote package. They may not be
    /// present in the local storage overlay yet, but reads can now yield a
    /// resumable `ResourcePending` request instead of failing permanently.
    pub fn set_external_resource_catalog<I, S>(&mut self, paths: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let paths: Vec<String> = paths.into_iter().map(Into::into).collect();
        if let Some(storage) = &self.project_storage {
            storage.add_catalog_paths(paths.iter().cloned());
        }
        self.external_resource_catalog = paths.iter().cloned().collect();
        // KRKR resolves relative storage names through configured auto paths.
        // A remote manifest has no filesystem directories to search, so expose
        // an unambiguous basename alias as well (e.g. `Config.tjs` for
        // `main/Config.tjs`). Ambiguous basenames remain path-qualified.
        let mut basenames = BTreeMap::<String, Option<String>>::new();
        let mut stems = BTreeMap::<String, Option<String>>::new();
        for path in paths {
            let basename = path
                .rsplit('/')
                .next()
                .unwrap_or(&path)
                .to_ascii_lowercase();
            match basenames.get_mut(&basename) {
                // The Web host supplies both the manifest key and the
                // physical entry path. They are often identical, and that
                // duplicate must not make an otherwise unique basename look
                // ambiguous (e.g. `main/config.tjs` -> `Config.tjs`).
                Some(value) if value.as_deref() == Some(path.as_str()) => {}
                Some(value) => *value = None,
                None => {
                    basenames.insert(basename, Some(path.clone()));
                }
            }
            if let Some((stem, extension)) =
                path.rsplit('/').next().unwrap_or(&path).rsplit_once('.')
                && matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "tlg" | "png" | "jpg" | "jpeg" | "bmp" | "webp"
                )
            {
                let key = stem.to_ascii_lowercase();
                match stems.get_mut(&key) {
                    Some(value) if value.as_deref() == Some(path.as_str()) => {}
                    Some(value)
                        if value.as_ref().is_some_and(|candidate| {
                            candidate
                                .get(..6)
                                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("uipsd/"))
                        }) => {}
                    Some(value)
                        if path
                            .get(..6)
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("uipsd/")) =>
                    {
                        *value = Some(path.clone())
                    }
                    Some(value) => *value = None,
                    None => {
                        stems.insert(key, Some(path.clone()));
                    }
                }
            }
        }
        for path in basenames.into_values().flatten() {
            let basename = path.rsplit('/').next().unwrap_or(&path).to_string();
            self.external_resource_catalog.insert(basename);
        }
        for path in stems.into_values().flatten() {
            let basename = path.rsplit('/').next().unwrap_or(&path);
            if let Some((stem, _)) = basename.rsplit_once('.') {
                self.external_resource_catalog.insert(stem.to_string());
            }
        }
    }

    pub fn take_external_resource_requests(&mut self) -> Vec<(String, AssetKind)> {
        std::mem::take(&mut self.pending_external_resources)
            .into_iter()
            .collect()
    }

    pub fn has_external_resource_request(&self, path: &str) -> bool {
        self.pending_external_resources.contains_key(path)
    }

    pub fn has_pending_external_resources(&self) -> bool {
        !self.pending_external_resources.is_empty()
    }

    pub(crate) fn register_system_hook(
        &mut self,
        name: impl Into<String>,
        hook: SystemHookRegistration,
    ) {
        self.system_hooks.insert(name.into(), hook);
    }

    pub(crate) fn system_hook(&self, name: &str) -> Option<SystemHookRegistration> {
        self.system_hooks.get(name).cloned()
    }

    pub fn provide_external_resource(&mut self, path: &str, bytes: Vec<u8>) -> Result<()> {
        self.project_storage()?
            .insert_memory(path.to_string(), bytes);
        if let Some(key) = self
            .pending_external_resources
            .keys()
            .find(|key| key.eq_ignore_ascii_case(path))
            .cloned()
        {
            self.pending_external_resources.remove(&key);
        }
        Ok(())
    }

    fn request_external_resource(&mut self, path: &str, kind: AssetKind) -> TjsError {
        self.pending_external_resources
            .entry(path.to_string())
            .or_insert(kind);
        TjsError::resource_pending(path.to_string())
    }

    fn is_external_resource(&self, path: &str) -> bool {
        self.external_resource_catalog
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(path))
    }

    pub fn resource_provider(&self) -> Option<Arc<dyn StoragePort>> {
        self.project_storage
            .as_ref()
            .map(|storage| Arc::new(storage.package_mount()) as Arc<dyn StoragePort>)
    }

    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    pub fn drain_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    pub fn linked_plugins(&self) -> impl Iterator<Item = &str> {
        self.linked_plugins.iter().map(String::as_str)
    }

    pub fn termination_requested(&self) -> bool {
        self.termination_requested
    }

    pub(crate) fn request_termination(&mut self) {
        self.termination_requested = true;
    }

    pub fn text_encoding(&self) -> &str {
        &self.text_encoding
    }

    pub fn set_text_encoding(&mut self, encoding: impl Into<String>) {
        self.text_encoding = encoding.into();
    }

    pub fn transition_policy(&self) -> TransitionPolicy {
        self.transition_policy
    }

    pub fn set_transition_policy(&mut self, policy: TransitionPolicy) {
        self.transition_policy = policy;
    }

    pub(crate) fn set_key_state(&mut self, key: i64, pressed: bool) {
        if pressed {
            self.pressed_keys.insert(key);
        } else {
            self.pressed_keys.remove(&key);
        }
    }

    pub(crate) fn key_state(&self, key: i64) -> bool {
        self.pressed_keys.contains(&key)
    }

    pub(crate) fn set_cursor_position(&mut self, position: Point) {
        self.cursor_position = Some(position);
    }

    pub(crate) fn cursor_position(&self) -> Option<Point> {
        self.cursor_position
    }

    pub fn lifecycle_state(&self) -> LifecycleState {
        self.lifecycle_state
    }

    pub(crate) fn set_lifecycle_state(&mut self, state: LifecycleState) {
        self.lifecycle_state = state;
        self.logs.push(format!("lifecycle state: {state:?}"));
    }

    pub(crate) fn push_text_input(&mut self, event: TextInputEvent) {
        self.pending_text_input.push(event);
    }

    /// Drains text input delivered by the host. A platform shell can expose
    /// this to an IME-aware UI without making the engine depend on DOM/IME
    /// types.
    pub fn take_text_input(&mut self) -> Vec<TextInputEvent> {
        std::mem::take(&mut self.pending_text_input)
    }

    pub fn add_auto_path(&mut self, path: impl Into<String>) {
        let path = storage_normalize_separators(&path.into());
        if !self.auto_paths.iter().any(|item| item == &path) {
            self.auto_paths.push(path);
            if let Some(storage) = &self.project_storage {
                storage.add_auto_path(self.auto_paths.last().expect("auto path was pushed"));
            }
            self.invalidate_resource_state();
        }
    }

    pub fn remove_auto_path(&mut self, path: &str) -> bool {
        let before = self.auto_paths.len();
        self.auto_paths.retain(|item| item != path);
        let removed = before != self.auto_paths.len();
        if removed {
            if let Some(storage) = &self.project_storage {
                storage.remove_auto_path(path);
            }
            self.invalidate_resource_state();
        }
        removed
    }

    pub fn clear_archive_cache(&self) -> Result<()> {
        if let Some(storage) = &self.project_storage {
            storage.clear_archive_cache()?;
        }
        Ok(())
    }

    pub fn storage_exists(&self, name: &str) -> bool {
        self.project_storage
            .as_ref()
            .is_some_and(|storage| storage.storage_exists(name))
            || self.is_external_resource(name)
    }

    /// Returns whether a logical directory exists in the mounted project or
    /// in the deferred Web manifest catalogue.
    pub fn storage_is_directory(&self, name: &str) -> bool {
        self.project_storage
            .as_ref()
            .is_some_and(|storage| storage.is_directory(name))
    }

    /// Lists immediate logical children using KRKR/fstat semantics. Static
    /// Web packages use the manifest catalogue; native projects merge the
    /// filesystem and XP3 provider views in `ProjectStorage`.
    pub fn storage_dirlist(&self, name: &str) -> Result<Vec<String>> {
        self.project_storage
            .as_ref()
            .ok_or_else(|| TjsError::runtime("project storage is not configured"))?
            .list_directory(name)
            .map_err(storage_io_error)
    }

    pub fn placed_path(&self, name: &str) -> Option<PathBuf> {
        self.project_storage
            .as_ref()
            .and_then(|storage| storage.placed_path(name))
    }

    pub(crate) fn read_text_storage(&self, name: &str) -> Result<String> {
        if let Some(manager) = self.resource_manager.as_ref() {
            return manager
                .load_text_blocking(name.to_string(), self.text_encoding.clone())
                .map_err(|error| {
                    TjsError::runtime(format!("failed to read text storage `{name}`: {error}"))
                });
        }
        self.project_storage()?
            .read_text_storage(name, &self.text_encoding)
    }

    pub fn read_binary_storage(&self, name: &str) -> Result<Vec<u8>> {
        let data = self.read_resource_storage(name)?;
        data.as_bytes()
            .map(|bytes| bytes.into_owned())
            .map_err(storage_io_error)
    }

    pub(crate) fn read_text_storage_for_tjs(&mut self, name: &str) -> Result<String> {
        match self.read_text_storage(name) {
            Ok(text) => Ok(text),
            Err(_) if self.is_external_resource(name) => {
                Err(self.request_external_resource(name, AssetKind::Text))
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn read_binary_storage_for_tjs(&mut self, name: &str) -> Result<Vec<u8>> {
        match self.read_binary_storage(name) {
            Ok(bytes) => Ok(bytes),
            Err(_) if self.is_external_resource(name) => {
                Err(self.request_external_resource(name, AssetKind::Binary))
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn read_resource_storage(&self, name: &str) -> Result<ResourceData> {
        if let Some(manager) = self.resource_manager.as_ref() {
            return manager
                .load_bytes_blocking(name.to_string())
                .map_err(|error| {
                    TjsError::runtime(format!("failed to read binary storage `{name}`: {error}"))
                });
        }
        self.project_storage()?.read_binary_storage(name)
    }

    pub fn write_text_storage(&mut self, name: &str, mode: &str, text: &str) -> Result<()> {
        let result = self.project_storage()?.write_text_storage(name, mode, text);
        if result.is_ok() {
            self.invalidate_resource_state();
        }
        result
    }

    pub fn write_binary_storage(&mut self, name: &str, mode: &str, bytes: &[u8]) -> Result<()> {
        let result = self
            .project_storage()?
            .write_binary_storage(name, mode, bytes);
        if result.is_ok() {
            self.invalidate_resource_state();
        }
        result
    }

    pub(crate) fn register_plugin(&mut self, name: &str) {
        self.linked_plugins.insert(name.to_string());
    }

    pub(crate) fn insert_kag_parser(&mut self, handle: ObjectHandle, parser: KagParser) {
        self.kag_parser_revisions.entry(handle).or_insert(0);
        self.kag_parsers.insert(handle, parser);
    }

    pub(crate) fn kag_parser(&self, handle: ObjectHandle) -> Option<&KagParser> {
        self.kag_parsers.get(&handle)
    }

    pub(crate) fn take_kag_parser(&mut self, handle: ObjectHandle) -> Option<KagParser> {
        self.kag_parsers.remove(&handle)
    }

    pub(crate) fn mark_kag_parser_changed(&mut self, handle: ObjectHandle) {
        let revision = self.kag_parser_revisions.entry(handle).or_insert(0);
        *revision = revision.saturating_add(1);
    }

    pub(crate) fn kag_parser_revision(&self, handle: ObjectHandle) -> u64 {
        self.kag_parser_revisions.get(&handle).copied().unwrap_or(0)
    }

    pub(crate) fn link_plugin(&mut self, name: &str) {
        self.linked_plugins.insert(name.to_string());
        self.logs
            .push(format!("plugin `{name}` linked through Rust registry"));
    }

    pub(crate) fn unlink_plugin(&mut self, name: &str) -> bool {
        self.linked_plugins.remove(name)
    }

    pub(crate) fn now_millis(&mut self) -> i64 {
        #[cfg(target_arch = "wasm32")]
        {
            // Browser WASM has no std::time wall clock. The engine advances
            // this virtual clock once per frame from the host-provided delta.
            return self.clock_offset_millis;
        }
        #[cfg(not(target_arch = "wasm32"))]
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0)
            .saturating_add(self.clock_offset_millis)
    }

    /// Installs an absolute host clock sample. Native hosts keep their wall
    /// clock as the base and store only the offset; Web/virtual hosts use the
    /// supplied value directly. RuntimeSession calls this once per frame so
    /// all scheduling code observes the same timestamp.
    pub(crate) fn set_clock_millis(&mut self, timestamp: i64) {
        #[cfg(target_arch = "wasm32")]
        {
            self.clock_offset_millis = timestamp.max(0);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let wall = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as i64)
                .unwrap_or(0);
            self.clock_offset_millis = timestamp.saturating_sub(wall);
        }
    }

    /// Advances the engine clock without sleeping. This is intended for
    /// deterministic headless integration probes; normal platform runtimes
    /// leave the offset at zero and continue to use wall-clock time.
    pub fn advance_clock(&mut self, duration: Duration) {
        let millis = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
        self.clock_offset_millis = self.clock_offset_millis.saturating_add(millis);
    }

    pub fn log(&mut self, message: &str) {
        if self.logs.len() >= MAX_LOG_LINES {
            self.logs.drain(..MAX_LOG_LINES / 2);
        }
        self.logs.push(message.to_string());
    }

    pub fn trace_enabled(&self, category: TraceCategory) -> bool {
        self.trace_mask & category.bit() != 0
    }

    /// Appends a category-tagged diagnostic line when the category is
    /// enabled (see [`TraceCategory`]). Used for high-frequency lifecycle
    /// diagnostics that would flood the log if always on.
    pub fn trace(&mut self, category: TraceCategory, message: &str) {
        if self.trace_enabled(category) {
            self.log(&format!("[{}] {message}", category.as_str()));
        }
    }

    /// Replaces the trace mask from a `KRKR_TRACE`-style category list.
    pub fn set_trace_categories(&mut self, categories: &str) {
        self.trace_mask = parse_trace_mask(categories);
    }

    /// Records one invocation of a stubbed native method. The first call to
    /// each stub is logged as a warning — a game actively calling an
    /// unimplemented method is the strongest compatibility signal there
    /// is; later calls only bump the counter (see [`Self::stub_call_counts`]).
    pub(crate) fn record_stub_call(&mut self, class_name: &str, method: &str, args_summary: &str) {
        let count = self
            .stub_calls
            .entry(format!("{class_name}.{method}"))
            .or_insert(0);
        *count += 1;
        if *count == 1 {
            self.log(&format!(
                "WARN stub method called: {class_name}.{method}({args_summary})"
            ));
        }
    }

    /// Invocation counts of stubbed native methods, keyed `Class.method`.
    pub fn stub_call_counts(&self) -> &BTreeMap<String, u64> {
        &self.stub_calls
    }

    pub fn layer_tree(&self) -> &LayerTree {
        &self.layer_tree
    }

    pub fn kag_layer_slot_for_render_layer(&self, layer_id: LayerId) -> Option<(String, String)> {
        let mut current = Some(layer_id);
        while let Some(id) = current {
            if let Some((_, slot)) = self
                .native_layers
                .iter()
                .find(|(_, instance)| instance.layer_id == id)
                .and_then(|(handle, _)| self.kag_layer_slots.get(handle).map(|slot| (handle, slot)))
            {
                return Some((slot.page.clone(), slot.layer.clone()));
            }
            if let Some((layer, _)) = self
                .kag_layers
                .iter()
                .find(|(_, candidate_id)| **candidate_id == id)
            {
                return Some(("fore".to_string(), layer.clone()));
            }
            current = self.layer_tree.layer(id).and_then(|layer| layer.parent);
        }
        None
    }

    pub fn layer_image_storage(&self, layer_id: LayerId) -> Option<&str> {
        self.layer_image_storages.get(&layer_id).map(String::as_str)
    }

    pub fn take_native_text_draw_events(&mut self) -> Vec<NativeTextDrawEvent> {
        std::mem::take(&mut self.native_text_draw_events)
    }

    pub(crate) fn record_native_text_draw(
        &mut self,
        target: &LayerRenderTarget,
        text: String,
        x: i64,
        y: i64,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let layer_id = match target {
            LayerRenderTarget::Native(layer_id) => Some(*layer_id),
            LayerRenderTarget::Kag(slot) => self.kag_layers.get(&slot.layer).copied(),
        };
        self.native_text_draw_events.push(NativeTextDrawEvent {
            layer_id,
            text,
            x,
            y,
        });
    }

    pub(crate) fn record_layer_image_storage(&mut self, target: &LayerRenderTarget, storage: &str) {
        let Some(layer_id) = self.render_layer_id_for_target(target) else {
            return;
        };
        self.layer_image_storages
            .insert(layer_id, storage.to_string());
    }

    pub(crate) fn clear_layer_image_storage_for_target(&mut self, target: &LayerRenderTarget) {
        let Some(layer_id) = self.render_layer_id_for_target(target) else {
            return;
        };
        self.layer_image_storages.remove(&layer_id);
    }

    pub(crate) fn clear_layer_image_storage(&mut self, layer_id: LayerId) {
        self.layer_image_storages.remove(&layer_id);
    }

    pub(crate) fn clear_kag_layer_image_storage(&mut self, page: &str, layer: &str) {
        let target = LayerRenderTarget::Kag(KagLayerSlot::new(page, layer));
        self.clear_layer_image_storage_for_target(&target);
    }

    fn render_layer_id_for_target(&mut self, target: &LayerRenderTarget) -> Option<LayerId> {
        match target {
            LayerRenderTarget::Native(layer_id) => Some(*layer_id),
            LayerRenderTarget::Kag(slot) => Some(self.ensure_kag_layer(&slot.page, &slot.layer)),
        }
    }

    pub(crate) fn layer_tree_mut(&mut self) -> &mut LayerTree {
        &mut self.layer_tree
    }

    pub(crate) fn register_native_window(
        &mut self,
        handle: ObjectHandle,
        children_array: Option<ObjectHandle>,
    ) {
        self.native_windows
            .entry(handle)
            .or_insert_with(|| WindowInstance::new(children_array));
    }

    pub(crate) fn native_window_property(
        &self,
        handle: ObjectHandle,
        name: &str,
    ) -> Option<Variant> {
        self.native_windows
            .get(&handle)
            .and_then(|window| window.property(name))
    }

    pub(crate) fn set_native_window_property(
        &mut self,
        handle: ObjectHandle,
        name: impl Into<String>,
        value: Variant,
    ) {
        let name = name.into();
        let window = self
            .native_windows
            .entry(handle)
            .or_insert_with(|| WindowInstance::new(None));
        match name.as_str() {
            "visible" => {
                window.visible = value.is_truthy();
            }
            "__nativeClosed" => {
                window.closed = value.is_truthy();
            }
            "__nativeModal" => {
                window.modal = value.is_truthy();
            }
            "primaryLayer" => {
                window.primary_layer = match &value {
                    Variant::Object(handle) => Some(*handle),
                    _ => None,
                };
            }
            "focusedLayer" => {
                window.focused_layer = match &value {
                    Variant::Object(handle) => Some(*handle),
                    _ => None,
                };
            }
            "children" => {
                window.children_array = match &value {
                    Variant::Object(handle) => Some(*handle),
                    _ => None,
                };
            }
            _ => {}
        }
        window.set_property(name, value);
        self.apply_window_visibility_to_layers(handle);
    }

    pub(crate) fn native_window_closed(&self, handle: ObjectHandle) -> bool {
        self.native_windows
            .get(&handle)
            .map(|window| window.closed)
            // A Layer.window property may temporarily hold a script object
            // (for example while a transition is being configured).  Such an
            // object is not a closed native window and must not hide the
            // layer. Modal waits handle missing native peers separately.
            .unwrap_or(false)
    }

    pub(crate) fn native_window_primary_layer(&self, handle: ObjectHandle) -> Option<ObjectHandle> {
        self.native_windows
            .get(&handle)
            .and_then(|window| window.primary_layer)
    }

    pub(crate) fn native_window_focused_layer(&self, handle: ObjectHandle) -> Option<ObjectHandle> {
        self.native_windows
            .get(&handle)
            .and_then(|window| window.focused_layer)
    }

    pub(crate) fn add_native_window_child(&mut self, window: ObjectHandle, child: ObjectHandle) {
        let window_instance = self
            .native_windows
            .entry(window)
            .or_insert_with(|| WindowInstance::new(None));
        window_instance.children.retain(|entry| *entry != child);
        window_instance.children.push(child);
        if self.native_layers.contains_key(&child) {
            if window_instance.primary_layer.is_none() {
                window_instance.primary_layer = Some(child);
                window_instance
                    .properties
                    .insert("primaryLayer".to_string(), Variant::Object(child));
            }
            if window_instance.focused_layer.is_none() {
                window_instance
                    .properties
                    .entry("focusedLayer".to_string())
                    .or_insert(Variant::Null);
            }
            self.set_native_layer_window(child, Some(window), Variant::Object(window));
        }
    }

    pub(crate) fn remove_native_window_child(&mut self, window: ObjectHandle, child: ObjectHandle) {
        let Some(window_instance) = self.native_windows.get_mut(&window) else {
            return;
        };
        window_instance.children.retain(|entry| *entry != child);
        if window_instance.primary_layer == Some(child) {
            window_instance.primary_layer = None;
            window_instance
                .properties
                .insert("primaryLayer".to_string(), Variant::Void);
        }
        if window_instance.focused_layer == Some(child) {
            window_instance.focused_layer = None;
            window_instance
                .properties
                .insert("focusedLayer".to_string(), Variant::Null);
        }
    }

    pub(crate) fn register_native_layer(
        &mut self,
        handle: ObjectHandle,
        name: impl Into<String>,
        window: Option<ObjectHandle>,
        parent: Option<ObjectHandle>,
        children_array: Option<ObjectHandle>,
        primary: bool,
    ) -> LayerId {
        if let Some(instance) = self.native_layers.get(&handle) {
            let layer_id = instance.layer_id;
            let old_parent = instance.parent;

            // A script subclass may call an intermediate `super.Layer()`
            // before its base class forwards the real `(owner, parent)` pair.
            // KRKR attaches the native instance when that latter constructor
            // runs.  Do not keep the temporary, ownerless root created by the
            // first call: it causes child controls (notably quick-menu
            // buttons) to escape their hidden parent layer.
            if window.is_some() || parent.is_some() {
                if let Some(old_parent) = old_parent
                    && Some(old_parent) != parent
                {
                    self.remove_native_layer_child(old_parent, handle);
                }
                if let Some(new_parent) = parent
                    && old_parent != Some(new_parent)
                {
                    self.add_native_layer_child(new_parent, handle);
                }
                if let Some(instance) = self.native_layers.get_mut(&handle) {
                    if window.is_some() {
                        instance.window = window;
                    }
                    instance.parent = parent;
                    instance.children_array = children_array.or(instance.children_array);
                }
                let render_parent = self
                    .native_layer_render_parent(handle, parent)
                    .filter(|parent_id| *parent_id != layer_id);
                // A few KRKR constructors pass their own layer as the
                // temporary parent while the native object is being wired.
                // Never materialize that as a tree edge: it makes the layer
                // disappear from root traversal (and therefore from draw
                // lists) until a later reparent happens.
                self.layer_tree.set_parent(layer_id, render_parent);
                if parent.is_some() {
                    let z_order = self.next_sibling_z_order(render_parent);
                    if let Some(layer) = self.layer_tree.layer_mut(layer_id) {
                        layer.z_order = z_order;
                    }
                }
            }
            return layer_id;
        }

        let id = self.layer_tree.create_layer(name, None, 0);
        if let Some(layer) = self.layer_tree.layer_mut(id)
            && primary
        {
            layer.visible = true;
            layer.opacity = 255;
            layer.layer_type = 1;
        }
        let mut instance = LayerInstance::new(id, window, parent, children_array);
        instance.set_property("isPrimary", Variant::Integer(i64::from(primary)));
        self.native_layers.insert(handle, instance);
        let render_parent = self
            .native_layer_render_parent(handle, parent)
            .filter(|parent_id| *parent_id != id);
        let z_order = if primary {
            0
        } else {
            self.next_sibling_z_order(render_parent)
        };
        if let Some(layer) = self.layer_tree.layer_mut(id) {
            layer.parent = render_parent;
            layer.z_order = z_order;
        }
        if let Some(parent) = parent {
            self.add_native_layer_child(parent, handle);
        }
        id
    }

    pub(crate) fn native_layer(&self, handle: ObjectHandle) -> Option<LayerId> {
        self.native_layers
            .get(&handle)
            .map(|instance| instance.layer_id)
    }

    pub(crate) fn native_object_for_layer(&self, layer_id: LayerId) -> Option<ObjectHandle> {
        self.native_layers
            .iter()
            .find_map(|(handle, instance)| (instance.layer_id == layer_id).then_some(*handle))
    }

    pub(crate) fn native_layer_property(
        &self,
        handle: ObjectHandle,
        name: &str,
    ) -> Option<Variant> {
        self.native_layers
            .get(&handle)
            .and_then(|instance| instance.property(name))
    }

    pub(crate) fn set_native_layer_property(
        &mut self,
        handle: ObjectHandle,
        name: impl Into<String>,
        value: Variant,
    ) {
        let Some(instance) = self.native_layers.get_mut(&handle) else {
            return;
        };
        instance.set_property(name, value);
    }

    pub(crate) fn native_layer_parent(&self, handle: ObjectHandle) -> Option<ObjectHandle> {
        self.native_layers
            .get(&handle)
            .and_then(|instance| instance.parent)
    }

    pub(crate) fn native_layer_window(&self, handle: ObjectHandle) -> Option<ObjectHandle> {
        self.native_layers
            .get(&handle)
            .and_then(|instance| instance.window)
    }

    pub(crate) fn native_layer_children(&self, handle: ObjectHandle) -> Vec<ObjectHandle> {
        self.native_layers
            .get(&handle)
            .map(|instance| instance.children.clone())
            .unwrap_or_default()
    }

    pub(crate) fn native_layer_roots(&self) -> Vec<ObjectHandle> {
        self.native_layers
            .iter()
            .filter_map(|(handle, instance)| instance.parent.is_none().then_some(*handle))
            .collect()
    }

    pub(crate) fn set_native_layer_parent(
        &mut self,
        handle: ObjectHandle,
        parent: Option<ObjectHandle>,
        stored_value: Variant,
    ) -> bool {
        let Some(layer_id) = self.native_layer(handle) else {
            return false;
        };
        let render_parent = self.native_layer_render_parent(handle, parent);
        if !self.layer_tree.set_parent(layer_id, render_parent) {
            return false;
        }

        let old_parent = self.native_layer_parent(handle);
        if old_parent == parent {
            if let Some(instance) = self.native_layers.get_mut(&handle) {
                instance.set_property("parent", stored_value);
            }
            return true;
        }

        if let Some(old_parent) = old_parent {
            self.remove_native_layer_child(old_parent, handle);
        }
        if let Some(new_parent) = parent {
            self.add_native_layer_child(new_parent, handle);
            let z_order = self.next_sibling_z_order(render_parent);
            if let Some(layer) = self.layer_tree.layer_mut(layer_id) {
                layer.z_order = z_order;
            }
        }
        if let Some(instance) = self.native_layers.get_mut(&handle) {
            instance.parent = parent;
            instance.set_property("parent", stored_value);
        }
        self.apply_layer_instance_to_render(handle);
        true
    }

    pub(crate) fn set_native_layer_window(
        &mut self,
        handle: ObjectHandle,
        window: Option<ObjectHandle>,
        stored_value: Variant,
    ) {
        let Some(instance) = self.native_layers.get_mut(&handle) else {
            return;
        };
        instance.window = window;
        instance.set_property("window", stored_value);
        self.apply_layer_instance_to_render(handle);
    }

    fn add_native_layer_child(&mut self, parent: ObjectHandle, child: ObjectHandle) {
        if let Some(parent) = self.native_layers.get_mut(&parent)
            && !parent.children.contains(&child)
        {
            parent.children.push(child);
        }
    }

    fn remove_native_layer_child(&mut self, parent: ObjectHandle, child: ObjectHandle) {
        if let Some(parent) = self.native_layers.get_mut(&parent) {
            parent.children.retain(|entry| *entry != child);
        }
    }

    pub(crate) fn kag_layer_slot(&self, handle: ObjectHandle) -> Option<&KagLayerSlot> {
        self.kag_layer_slots.get(&handle)
    }

    pub(crate) fn layer_render_target(&self, handle: ObjectHandle) -> Option<LayerRenderTarget> {
        self.native_layers
            .get(&handle)
            .map(|instance| instance.render_target.clone())
    }

    pub(crate) fn replace_kag_layer_slots(&mut self, slots: BTreeMap<ObjectHandle, KagLayerSlot>) {
        if self.kag_layer_slots == slots {
            return;
        }
        self.kag_layer_slots = slots;
        let handles = self.native_layers.keys().copied().collect::<Vec<_>>();
        for handle in handles {
            let Some(layer_id) = self.native_layer(handle) else {
                continue;
            };
            let target = match self.kag_layer_slots.get(&handle).cloned() {
                Some(slot) if slot.page == "back" => LayerRenderTarget::Kag(slot),
                _ => LayerRenderTarget::Native(layer_id),
            };
            if let Some(instance) = self.native_layers.get_mut(&handle) {
                instance.render_target = target;
            }
            self.apply_layer_instance_to_render(handle);
        }
    }

    pub(crate) fn apply_layer_instance_to_render(&mut self, handle: ObjectHandle) {
        let Some(instance) = self.native_layers.get(&handle).cloned() else {
            return;
        };
        let window_closed = instance
            .window
            .is_some_and(|window| self.native_window_closed(window));
        match instance.render_target.clone() {
            LayerRenderTarget::Native(layer_id) => {
                let render_parent = self
                    .native_layer_render_parent(handle, instance.parent)
                    .filter(|parent_id| *parent_id != layer_id);
                self.layer_tree.set_parent(layer_id, render_parent);
                if let Some(layer) = self.layer_tree.layer_mut(layer_id) {
                    apply_layer_properties_to_node(layer, &instance.properties, window_closed);
                    layer.renderable = true;
                }
            }
            LayerRenderTarget::Kag(slot) => {
                if let Some(layer) = self.layer_tree.layer_mut(instance.layer_id) {
                    layer.renderable = false;
                }
                self.mutate_kag_layer(&slot.page, &slot.layer, |layer| {
                    apply_layer_properties_to_node(layer, &instance.properties, window_closed);
                });
            }
        }
    }

    /// Returns the physical parent used by the current fore-page render
    /// projection.  A page exchange can make the active fore base a TJS child
    /// of the now-staged back page; that ancestor is deliberately not
    /// renderable.  Project just that virtual page root to the render root so
    /// its live children remain visible.  Ordinary fore bases retain their
    /// real parent, which is essential for sibling popup ordering.
    fn native_layer_render_parent(
        &self,
        handle: ObjectHandle,
        parent: Option<ObjectHandle>,
    ) -> Option<LayerId> {
        let is_fore_base = self
            .kag_layer_slots
            .get(&handle)
            .is_some_and(|slot| slot.page == "fore" && slot.layer == "base");
        let parent_is_back_page = parent.is_some_and(|parent| {
            self.kag_layer_slots
                .get(&parent)
                .is_some_and(|slot| slot.page == "back")
        });
        if is_fore_base && parent_is_back_page {
            None
        } else if let Some(parent) = parent
            && self
                .kag_layer_slots
                .get(&parent)
                .is_some_and(|slot| slot.page == "back" && slot.layer == "base")
            && self
                .native_layer(handle)
                .and_then(|layer_id| self.layer_tree.layer(layer_id))
                .is_some_and(|layer| layer.renderable)
        {
            // `syspage ... page=back` builds UI below the staging base.  On
            // exchange, KAG projects that live UI subtree into fore while the
            // back base itself stays non-renderable.  Keep the script parent
            // untouched, but attach its render root to the corresponding
            // fore base so draw and hit-test traversal agree.
            self.kag_layer_slots.iter().find_map(|(handle, slot)| {
                (slot.page == "fore" && slot.layer == "base")
                    .then(|| self.native_layer(*handle))
                    .flatten()
            })
        } else {
            parent.and_then(|parent| self.native_layer(parent))
        }
    }

    fn apply_window_visibility_to_layers(&mut self, window: ObjectHandle) {
        let handles = self
            .native_layers
            .iter()
            .filter_map(|(handle, instance)| (instance.window == Some(window)).then_some(*handle))
            .collect::<Vec<_>>();
        for handle in handles {
            self.apply_layer_instance_to_render(handle);
        }
    }

    pub(crate) fn invalidate_native_object(&mut self, handle: ObjectHandle) {
        self.cleanup_invalidated_handle(handle);
        self.modal_windows.retain(|window| *window != handle);

        if let Some(window) = self.native_windows.remove(&handle) {
            for child in window.children {
                self.invalidate_native_object(child);
            }
            return;
        }

        let Some(layer_id) = self.native_layer(handle) else {
            return;
        };

        let mut removed_layer_ids = BTreeSet::new();
        self.collect_layer_subtree_ids(layer_id, &mut removed_layer_ids);
        for layer_id in &removed_layer_ids {
            self.layer_tree.remove_layer(*layer_id);
        }

        let removed_handles = self
            .native_layers
            .iter()
            .filter_map(|(handle, instance)| {
                removed_layer_ids
                    .contains(&instance.layer_id)
                    .then_some(*handle)
            })
            .collect::<Vec<_>>();
        for handle in removed_handles {
            if let Some(instance) = self.native_layers.remove(&handle) {
                if let Some(parent) = instance.parent {
                    self.remove_native_layer_child(parent, handle);
                }
                if let Some(window) = instance.window {
                    self.remove_native_window_child(window, handle);
                }
            }
            if let Some(slot) = self.kag_layer_slots.remove(&handle)
                && slot.page == "back"
            {
                self.pending_kag_layers.remove(&slot.layer);
            }
            for window in self.native_windows.values_mut() {
                window.children.retain(|child| *child != handle);
                if window.primary_layer == Some(handle) {
                    window.primary_layer = None;
                    window
                        .properties
                        .insert("primaryLayer".to_string(), Variant::Void);
                }
                if window.focused_layer == Some(handle) {
                    window.focused_layer = None;
                    window
                        .properties
                        .insert("focusedLayer".to_string(), Variant::Null);
                }
            }
            self.cleanup_invalidated_handle(handle);
        }
    }

    fn cleanup_invalidated_handle(&mut self, handle: ObjectHandle) {
        self.scheduler.invalidate_object(handle);
        self.pending_image_loads
            .retain(|_, load| load.request.owner != Some(handle));
        self.kag_parsers.remove(&handle);
        self.kag_parser_revisions.remove(&handle);
        if let Some(buffer) = self.native_audio_buffers.remove(&handle) {
            self.pending_audio_commands.push(AudioCommand::Stop {
                id: buffer.id,
                fade_seconds: 0.0,
            });
        }
    }

    pub(crate) fn register_timer(&mut self, handle: ObjectHandle) {
        self.scheduler.register_timer(handle);
    }

    pub(crate) fn register_async_trigger(&mut self, handle: ObjectHandle) {
        self.scheduler.cancel_async(handle);
    }

    pub(crate) fn trigger_async_with_mode(
        &mut self,
        handle: ObjectHandle,
        mode: AsyncTriggerMode,
        cached: bool,
    ) {
        self.scheduler.trigger_async(handle, mode, cached);
    }

    pub(crate) fn cancel_async(&mut self, handle: ObjectHandle) {
        self.scheduler.cancel_async(handle);
    }

    pub(crate) fn schedule_audio_fade_completion(&mut self, handle: ObjectHandle, millis: i64) {
        let due = self.now_millis().saturating_add(millis.max(0));
        self.scheduler.schedule_audio_fade_completion(handle, due);
    }

    pub(crate) fn cancel_audio_fade_completion(&mut self, handle: ObjectHandle) {
        self.scheduler.cancel_audio_fade_completion(handle);
    }

    pub(crate) fn request_layer_paint(&mut self, handle: ObjectHandle) -> bool {
        self.scheduler.post_window_update(handle)
    }

    pub(crate) fn add_continuous_handler(&mut self, handler: Variant) {
        self.scheduler.add_continuous_handler(handler);
    }

    pub(crate) fn remove_continuous_handler(&mut self, handler: &Variant) -> bool {
        self.scheduler.remove_continuous_handler(handler)
    }

    pub(crate) fn scheduler(&self) -> &TvpScheduler {
        &self.scheduler
    }

    #[cfg(test)]
    pub(crate) fn has_pending_window_update(&self, handle: ObjectHandle) -> bool {
        self.scheduler.has_window_update(handle)
    }

    #[cfg(test)]
    pub(crate) fn has_pending_image_load_for_owner(&self, handle: ObjectHandle) -> bool {
        self.pending_image_loads
            .values()
            .any(|load| load.request.owner == Some(handle))
    }

    pub(crate) fn scheduler_mut(&mut self) -> &mut TvpScheduler {
        &mut self.scheduler
    }

    pub(crate) fn scheduler_diagnostics(&self) -> (usize, usize, usize, usize, usize) {
        (
            self.scheduler.continuous_handler_count(),
            self.scheduler.script_event_count(),
            self.scheduler.idle_event_count(),
            self.scheduler.timer_count(),
            self.scheduler.window_update_count(),
        )
    }

    pub(crate) fn ensure_kag_layer(&mut self, page: &str, layer: &str) -> LayerId {
        let _ = normalize_kag_page(page);
        let key = layer.to_string();
        match self.kag_layers.entry(key) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let id = self.layer_tree.create_layer(
                    format!("kag:{layer}"),
                    None,
                    kag_layer_z_order(layer),
                );
                if let Some(node) = self.layer_tree.layer_mut(id) {
                    node.renderable = true;
                }
                entry.insert(id);
                id
            }
        }
    }

    pub(crate) fn kag_layer(&self, page: &str, layer: &str) -> Option<&LayerNode> {
        if normalize_kag_page(page) == "back"
            && let Some(node) = self.pending_kag_layers.get(layer)
        {
            return Some(node);
        }

        self.kag_layers
            .get(layer)
            .and_then(|layer_id| self.layer_tree.layer(*layer_id))
    }

    pub(crate) fn current_kag_page(&self) -> &str {
        &self.current_kag_page
    }

    pub(crate) fn current_kag_layer(&self) -> &str {
        &self.current_kag_layer
    }

    pub(crate) fn set_current_kag_layer(
        &mut self,
        page: impl Into<String>,
        layer: impl Into<String>,
    ) {
        self.current_kag_page = normalize_kag_page(&page.into()).to_string();
        self.current_kag_layer = layer.into();
    }

    pub(crate) fn load_image_storage(&mut self, name: &str) -> Result<LayerImage> {
        self.logs
            .push(format!("image load decode started `{name}`"));
        self.sync_image_cache_revision();
        if let Some(image) = self.image_cache.get(name) {
            return Ok(image.clone());
        }

        let bytes = self.read_binary_storage_for_tjs(name)?;
        let decoded = decode_image_bytes(&bytes, name).map_err(TjsError::runtime)?;
        let texture_id = self.next_texture_id;
        self.next_texture_id = self.next_texture_id.saturating_add(1);
        let image = LayerImage::new(texture_id, decoded.width, decoded.height, decoded.rgba);
        self.image_cache.insert(name.to_string(), image.clone());
        Ok(image)
    }

    pub(crate) fn load_image_storage_for_script(&mut self, name: &str) -> Result<LayerImage> {
        self.logs
            .push(format!("script image load requested `{name}`"));
        self.sync_image_cache_revision();
        if let Some(image) = self.image_cache.get(name) {
            return Ok(image.clone());
        }

        #[cfg(test)]
        {
            self.load_image_storage(name)
        }

        #[cfg(not(test))]
        {
            let Some(manager) = self.resource_manager.as_ref() else {
                return self.load_image_storage(name);
            };
            if let Some(error) = self.script_image_errors.get(name) {
                return Err(TjsError::runtime(format!(
                    "failed to decode image `{name}`: {error}"
                )));
            }
            let revision = self.storage_revision();
            if self
                .pending_script_image_loads
                .values()
                .any(|(storage, _)| storage.eq_ignore_ascii_case(name))
            {
                return Err(TjsError::resource_pending(name.to_string()));
            }
            let id = manager.request_image_decode(name.to_string(), revision);
            self.pending_script_image_loads
                .insert(id, (name.to_string(), revision));
            self.logs.push(format!(
                "script image decode queued `{name}` (request {})",
                id.0
            ));
            // TJS/KAG resumes this native call after the worker completion is
            // applied to the image cache; no frame is blocked on decode.
            Err(TjsError::resource_pending(name.to_string()))
        }
    }

    pub(crate) fn request_image_load(
        &mut self,
        request: ImageLoadRequest,
    ) -> Result<ImageLoadState> {
        self.sync_image_cache_revision();
        if let Some(image) = self.image_cache.get(&request.storage) {
            return Ok(ImageLoadState::Ready(Box::new(CompletedImageLoad {
                request,
                image,
            })));
        }

        // Browser/web packages keep non-bootstrap images outside the initial
        // ProjectStorage overlay. Let the normal external-resource scheduler
        // fetch the image before asking the decode worker to read it.
        let local_storage_exists = self
            .project_storage
            .as_ref()
            .is_some_and(|storage| storage.storage_exists(&request.storage));
        if !local_storage_exists && self.is_external_resource(&request.storage) {
            self.logs.push(format!(
                "image load waiting for external resource `{}`",
                request.storage
            ));
            return Err(self.request_external_resource(&request.storage, AssetKind::Image));
        }

        #[cfg(test)]
        {
            let image = self.load_image_storage(&request.storage)?;
            Ok(ImageLoadState::Ready(Box::new(CompletedImageLoad {
                request,
                image,
            })))
        }

        #[cfg(not(test))]
        {
            let Some(manager) = self.resource_manager.as_ref() else {
                let image = self.load_image_storage(&request.storage)?;
                return Ok(ImageLoadState::Ready(Box::new(CompletedImageLoad {
                    request,
                    image,
                })));
            };
            let revision = self.storage_revision();
            let generation = self.next_resource_generation;
            self.next_resource_generation = self.next_resource_generation.saturating_add(1);
            self.pending_image_loads
                .retain(|_, load| load.request.target != request.target);
            self.image_target_generations
                .insert(request.target.clone(), generation);
            let id = manager.request_image_decode(request.storage.clone(), revision);
            self.pending_image_loads.insert(
                id,
                PendingImageLoad {
                    request,
                    generation,
                    revision,
                },
            );
            Ok(ImageLoadState::Pending)
        }
    }

    pub(crate) fn clear_graphic_cache(&mut self) {
        self.image_cache.clear();
        if let Some(manager) = self.resource_manager.as_ref()
            && let Err(error) = manager.clear_decoded_image_cache_blocking()
        {
            self.logs
                .push(format!("failed to clear decoded image cache: {error}"));
        }
    }

    pub(crate) fn touch_images(&mut self, storages: &[String], limit: i64, timeout_ms: u64) {
        self.sync_image_cache_revision();
        if storages.is_empty() {
            return;
        }

        let start = Instant::now();
        let timeout = (timeout_ms > 0).then(|| Duration::from_millis(timeout_ms));
        let limit_bytes = graphic_cache_limit_bytes(limit);
        let mut touched = 0usize;
        let mut bytes = 0usize;
        let mut timed_out = false;
        let mut limit_exceeded = false;

        for storage in storages {
            if timeout.is_some_and(|timeout| start.elapsed() >= timeout) {
                timed_out = true;
                break;
            }
            if bytes >= limit_bytes {
                limit_exceeded = true;
                break;
            }
            if storage.is_empty() {
                continue;
            }

            match self.touch_image_for_cache(storage) {
                Ok(image_bytes) => {
                    touched = touched.saturating_add(1);
                    bytes = bytes.saturating_add(image_bytes);
                }
                Err(error) => {
                    self.logs
                        .push(format!("failed to touch image `{storage}`: {error}"));
                }
            }
        }

        let reason = if timed_out {
            " timed out"
        } else if limit_exceeded {
            " limit exceeded"
        } else {
            ""
        };
        self.logs.push(format!(
            "touched {touched} image(s), {} bytes in {}ms{reason}",
            bytes,
            start.elapsed().as_millis()
        ));
    }

    fn touch_image_for_cache(&mut self, name: &str) -> Result<usize> {
        if let Some(image) = self.image_cache.get(name) {
            return Ok(image.upload.rgba.len());
        }

        #[cfg(test)]
        {
            let image = self.load_image_storage(name)?;
            Ok(image.upload.rgba.len())
        }

        #[cfg(not(test))]
        {
            // Cache warming is a hint, not a reason to block the VM. Reuse
            // the same queued decode path as Layer.loadImages; the completion
            // will populate the bounded decoded-image cache on a later frame.
            let image = self.load_image_storage_for_script(name)?;
            Ok(image.upload.rgba.len())
        }
    }

    pub(crate) fn take_completed_image_loads(&mut self) -> (Vec<CompletedImageLoad>, usize) {
        self.poll_resource_completions();
        (
            std::mem::take(&mut self.completed_image_loads),
            std::mem::take(&mut self.completed_script_image_loads),
        )
    }

    pub fn has_pending_resource_loads(&self) -> bool {
        !self.pending_image_loads.is_empty() || !self.pending_script_image_loads.is_empty()
    }

    fn poll_resource_completions(&mut self) {
        let Some(manager) = self.resource_manager.as_ref() else {
            return;
        };
        let completions = manager.drain_completions();
        for completion in completions {
            if let Some((storage, expected_revision)) =
                self.pending_script_image_loads.remove(&completion.id)
            {
                if expected_revision != completion.revision
                    || completion.revision != self.storage_revision()
                {
                    self.logs.push(format!(
                        "discarded stale script image `{storage}` after storage revision changed"
                    ));
                    continue;
                }
                match completion.result {
                    Ok(decoded) => {
                        let image = self.layer_image_from_decoded(decoded);
                        self.logs.push(format!(
                            "script image decoded `{storage}` ({}x{}, {} bytes)",
                            image.upload.width,
                            image.upload.height,
                            image.upload.rgba.len()
                        ));
                        self.script_image_errors.remove(&storage);
                        self.image_cache.insert(storage, image);
                        self.completed_script_image_loads =
                            self.completed_script_image_loads.saturating_add(1);
                    }
                    Err(error) => {
                        self.script_image_errors
                            .insert(storage.clone(), error.clone());
                        self.logs
                            .push(format!("script image decode failed `{storage}`: {error}"));
                        self.completed_script_image_loads =
                            self.completed_script_image_loads.saturating_add(1);
                    }
                }
                continue;
            }
            self.complete_image_load(
                completion.id,
                completion.revision,
                &completion.storage,
                completion.result,
            );
        }
    }

    fn complete_image_load(
        &mut self,
        id: ResourceTaskId,
        revision: u64,
        storage: &str,
        result: std::result::Result<DecodedImageData, String>,
    ) {
        let Some(pending) = self.pending_image_loads.remove(&id) else {
            return;
        };
        if pending.revision != revision
            || revision != self.storage_revision()
            || self
                .image_target_generations
                .get(&pending.request.target)
                .copied()
                != Some(pending.generation)
        {
            return;
        }
        self.image_target_generations
            .remove(&pending.request.target);
        match result {
            Ok(decoded) => {
                self.logs.push(format!(
                    "image decoded `{storage}` ({}x{}, {} bytes)",
                    decoded.width,
                    decoded.height,
                    decoded.rgba.len()
                ));
                let image = self.layer_image_from_decoded(decoded);
                self.image_cache.insert(storage.to_string(), image.clone());
                self.completed_image_loads.push(CompletedImageLoad {
                    request: pending.request,
                    image,
                });
            }
            Err(error) => {
                self.logs
                    .push(format!("failed to decode image `{storage}`: {error}"));
            }
        }
    }

    fn layer_image_from_decoded(&mut self, decoded: DecodedImageData) -> LayerImage {
        let texture_id = self.next_texture_id;
        self.next_texture_id = self.next_texture_id.saturating_add(1);
        LayerImage::new(texture_id, decoded.width, decoded.height, decoded.rgba)
    }

    fn storage_revision(&self) -> u64 {
        self.project_storage
            .as_ref()
            .map(ProjectStorage::revision)
            .unwrap_or(0)
    }

    fn sync_image_cache_revision(&mut self) {
        let revision = self.storage_revision();
        if self.image_cache_revision != revision {
            self.cancel_pending_resource_tasks();
            self.image_cache.clear();
            self.completed_image_loads.clear();
            self.pending_image_loads.clear();
            self.image_target_generations.clear();
            self.image_cache_revision = revision;
        }
    }

    fn invalidate_resource_state(&mut self) {
        self.cancel_pending_resource_tasks();
        self.image_cache_revision = self.storage_revision();
        self.image_cache.clear();
        self.pending_image_loads.clear();
        self.completed_image_loads.clear();
        self.image_target_generations.clear();
    }

    fn cancel_pending_resource_tasks(&self) {
        let Some(manager) = self.resource_manager.as_ref() else {
            return;
        };
        for id in self
            .pending_image_loads
            .keys()
            .chain(self.pending_script_image_loads.keys())
            .copied()
        {
            manager.cancel(id);
        }
    }

    pub(crate) fn register_native_audio_buffer(&mut self, handle: ObjectHandle) -> AudioInstanceId {
        let id = AudioInstanceId(self.next_audio_instance_id);
        self.next_audio_instance_id = self.next_audio_instance_id.saturating_add(1);
        self.native_audio_buffers
            .insert(handle, NativeAudioBuffer::new(id));
        id
    }

    pub(crate) fn native_audio_buffer(&self, handle: ObjectHandle) -> Option<&NativeAudioBuffer> {
        self.native_audio_buffers.get(&handle)
    }

    pub(crate) fn native_audio_global_volume(&self) -> i64 {
        self.native_audio_global_volume
    }

    pub(crate) fn set_native_audio_global_volume(&mut self, volume: i64) {
        self.native_audio_global_volume = clamp_krkr_volume(volume);
        self.queue_all_native_audio_volume_updates();
    }

    pub(crate) fn set_native_audio_volume(&mut self, handle: ObjectHandle, volume: i64) {
        self.set_native_audio_volume_with_fade(handle, volume, 0.0);
    }

    pub(crate) fn set_native_audio_volume_with_fade(
        &mut self,
        handle: ObjectHandle,
        volume: i64,
        fade_seconds: f32,
    ) {
        let global_volume = self.native_audio_global_volume;
        let Some(buffer) = self.native_audio_buffers.get_mut(&handle) else {
            return;
        };
        buffer.volume = clamp_krkr_volume(volume);
        if buffer.playing {
            let id = buffer.id;
            let volume = buffer.effective_volume(global_volume);
            self.pending_audio_commands.push(AudioCommand::SetVolume {
                id,
                volume,
                fade_seconds,
            });
        }
    }

    pub(crate) fn set_native_audio_volume2(&mut self, handle: ObjectHandle, volume: i64) {
        let global_volume = self.native_audio_global_volume;
        let Some(buffer) = self.native_audio_buffers.get_mut(&handle) else {
            return;
        };
        buffer.volume2 = clamp_krkr_volume(volume);
        if buffer.playing {
            let id = buffer.id;
            let volume = buffer.effective_volume(global_volume);
            self.pending_audio_commands.push(AudioCommand::SetVolume {
                id,
                volume,
                fade_seconds: 0.0,
            });
        }
    }

    pub(crate) fn set_native_audio_looping(&mut self, handle: ObjectHandle, looping: bool) {
        if let Some(buffer) = self.native_audio_buffers.get_mut(&handle) {
            buffer.looping = looping;
        }
    }

    pub(crate) fn set_native_audio_pan(&mut self, handle: ObjectHandle, pan: i64) {
        if let Some(buffer) = self.native_audio_buffers.get_mut(&handle) {
            buffer.pan = pan.clamp(-100000, 100000);
        }
    }

    pub(crate) fn mark_native_audio_stopped(&mut self, handle: ObjectHandle) {
        if let Some(buffer) = self.native_audio_buffers.get_mut(&handle) {
            buffer.playing = false;
        }
    }

    pub(crate) fn mark_native_audio_instance_stopped(
        &mut self,
        id: AudioInstanceId,
    ) -> Option<ObjectHandle> {
        self.native_audio_buffers
            .iter_mut()
            .find_map(|(handle, buffer)| {
                (buffer.id == id).then(|| {
                    buffer.playing = false;
                    *handle
                })
            })
    }

    pub(crate) fn open_native_audio_storage(
        &mut self,
        handle: ObjectHandle,
        storage: impl Into<String>,
    ) -> Result<()> {
        let storage = storage.into();
        if !self.native_audio_buffers.contains_key(&handle) {
            let id = self.allocate_audio_instance_id();
            self.native_audio_buffers
                .insert(handle, NativeAudioBuffer::new(id));
        }
        let buffer = self
            .native_audio_buffers
            .get_mut(&handle)
            .expect("native audio buffer was inserted");
        buffer.storage = Some(storage.clone());
        self.pending_audio_commands.push(AudioCommand::Preload {
            source: AudioSourceRef::new(storage),
            load_policy: AudioLoadPolicy::Auto,
        });
        Ok(())
    }

    pub(crate) fn queue_native_audio_play(
        &mut self,
        handle: ObjectHandle,
        bus: AudioBus,
        load_policy: AudioLoadPolicy,
    ) -> Result<()> {
        let buffer = self
            .native_audio_buffers
            .get_mut(&handle)
            .ok_or_else(|| TjsError::runtime("WaveSoundBuffer is not initialized"))?;
        let storage = buffer
            .storage
            .clone()
            .ok_or_else(|| TjsError::runtime("WaveSoundBuffer has no opened storage"))?;
        let id = buffer.id;
        let looping = buffer.looping;
        let volume = buffer.effective_volume(self.native_audio_global_volume);
        buffer.playing = true;
        self.pending_audio_commands.push(AudioCommand::Play {
            id,
            bus,
            source: AudioSourceRef::new(storage),
            load_policy,
            looping,
            volume,
        });
        Ok(())
    }

    pub(crate) fn queue_audio_command(&mut self, command: AudioCommand) {
        self.pending_audio_commands.push(command);
    }

    pub(crate) fn video_overlay_state(&self, handle: ObjectHandle) -> Option<&VideoOverlayState> {
        self.video_overlays.get(&handle)
    }

    pub(crate) fn video_overlay_state_mut(
        &mut self,
        handle: ObjectHandle,
    ) -> &mut VideoOverlayState {
        self.video_overlays.entry(handle).or_default()
    }

    pub(crate) fn video_overlay_handles(&self) -> Vec<ObjectHandle> {
        self.video_overlays.keys().copied().collect()
    }

    pub(crate) fn video_overlays_mut(&mut self) -> &mut BTreeMap<ObjectHandle, VideoOverlayState> {
        &mut self.video_overlays
    }

    pub(crate) fn remove_video_overlay(&mut self, handle: ObjectHandle) {
        self.video_overlays.remove(&handle);
    }

    pub(crate) fn allocate_video_texture_id(&mut self) -> TextureId {
        let texture_id = self.next_texture_id;
        self.next_texture_id = self.next_texture_id.saturating_add(1);
        texture_id
    }

    pub fn take_audio_commands(&mut self) -> Vec<AudioCommand> {
        std::mem::take(&mut self.pending_audio_commands)
    }

    pub(crate) fn queue_kag_audio_play(
        &mut self,
        storage: impl Into<String>,
        bus: AudioBus,
        load_policy: AudioLoadPolicy,
        looping: bool,
        volume: f32,
    ) -> Result<AudioInstanceId> {
        let storage = storage.into();
        let id = self.allocate_audio_instance_id();
        self.pending_audio_commands.push(AudioCommand::Play {
            id,
            bus,
            source: AudioSourceRef::new(storage),
            load_policy,
            looping,
            volume,
        });
        Ok(id)
    }

    fn allocate_audio_instance_id(&mut self) -> AudioInstanceId {
        let id = AudioInstanceId(self.next_audio_instance_id);
        self.next_audio_instance_id = self.next_audio_instance_id.saturating_add(1);
        id
    }

    /// Queues playback of a live PCM stream (movie soundtrack decoded by
    /// krkr-video) and returns its audio instance id.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn queue_pcm_stream_play(
        &mut self,
        bus: AudioBus,
        spec: krkr_core::PcmAudioSpec,
        total_frames: u64,
        rx: std::sync::mpsc::Receiver<krkr_core::PcmAudioChunk>,
        volume: f32,
    ) -> AudioInstanceId {
        let id = self.allocate_audio_instance_id();
        self.pending_audio_commands
            .push(AudioCommand::PlayPcmStream {
                id,
                bus,
                source: krkr_core::PcmStreamSource::new(
                    spec,
                    total_frames,
                    Box::new(ReceiverPcmStream { receiver: rx }),
                ),
                volume,
            });
        id
    }

    fn queue_all_native_audio_volume_updates(&mut self) {
        let global_volume = self.native_audio_global_volume;
        let updates = self
            .native_audio_buffers
            .values()
            .filter(|buffer| buffer.playing)
            .map(|buffer| (buffer.id, buffer.effective_volume(global_volume)))
            .collect::<Vec<_>>();
        for (id, volume) in updates {
            self.pending_audio_commands.push(AudioCommand::SetVolume {
                id,
                volume,
                fade_seconds: 0.0,
            });
        }
    }

    pub(crate) fn create_layer_image(
        &mut self,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> LayerImage {
        let texture_id = self.next_texture_id;
        self.next_texture_id = self.next_texture_id.saturating_add(1);
        LayerImage::new(texture_id, width, height, Arc::<[u8]>::from(rgba))
    }

    pub(crate) fn font_system(&self) -> &FontSystem {
        &self.font_system
    }

    pub fn font_system_mut(&mut self) -> &mut FontSystem {
        &mut self.font_system
    }

    pub(crate) fn mutate_kag_layer<R>(
        &mut self,
        page: &str,
        layer: &str,
        mutate: impl FnOnce(&mut LayerNode) -> R,
    ) -> R {
        if normalize_kag_page(page) == "back" {
            let node = self.pending_kag_layer_mut(layer);
            mutate(node)
        } else {
            let layer_id = self.ensure_kag_layer("fore", layer);
            let node = self
                .layer_tree
                .layer_mut(layer_id)
                .expect("created KAG layer must exist");
            mutate(node)
        }
    }

    pub(crate) fn apply_immediate_transition(&mut self) {
        self.complete_active_transition();
        self.apply_pending_kag_layers();
        self.active_transition = None;
    }

    pub(crate) fn begin_kag_transition(
        &mut self,
        duration: Duration,
        params: TransitionParams,
        rule_image_upload: Option<ImageUpload>,
    ) {
        self.complete_active_transition();
        if self.pending_kag_layers.is_empty() {
            self.active_transition = None;
            return;
        }

        if duration.is_zero() || self.transition_policy == TransitionPolicy::Immediate {
            self.apply_immediate_transition();
            return;
        }

        let (frozen_draw_commands, frozen_image_uploads) = self.layer_tree.draw_model();
        self.apply_pending_kag_layers();
        self.active_transition = Some(ActiveTransition {
            params,
            rule_texture_id: rule_image_upload.as_ref().map(|upload| upload.texture_id),
            rule_image_upload,
            elapsed: Duration::ZERO,
            duration,
            frozen_draw_commands,
            frozen_image_uploads,
            suppressed_live_images: BTreeSet::new(),
            live_layer_overrides: BTreeMap::new(),
            native_completion: None,
        });
    }

    pub(crate) fn begin_native_transition(
        &mut self,
        duration: Duration,
        params: TransitionParams,
        rule_image_upload: Option<ImageUpload>,
        frozen_model: (Vec<DrawCommand>, Vec<ImageUpload>),
        suppressed_live_images: BTreeSet<LayerId>,
        live_layer_overrides: BTreeMap<LayerId, LayerNode>,
        completion: NativeTransitionCompletion,
    ) {
        self.complete_active_transition();
        if duration.is_zero() || self.transition_policy == TransitionPolicy::Immediate {
            self.completed_native_transitions.push(completion);
            self.active_transition = None;
            return;
        }

        self.active_transition = Some(ActiveTransition {
            params,
            rule_texture_id: rule_image_upload.as_ref().map(|upload| upload.texture_id),
            rule_image_upload,
            elapsed: Duration::ZERO,
            duration,
            frozen_draw_commands: frozen_model.0,
            frozen_image_uploads: frozen_model.1,
            suppressed_live_images,
            live_layer_overrides,
            native_completion: Some(completion),
        });
    }

    pub(crate) fn advance_transition(&mut self, delta: Duration) {
        let Some(transition) = &mut self.active_transition else {
            return;
        };
        transition.elapsed = transition.elapsed.saturating_add(delta);
        if transition.elapsed >= transition.duration {
            if let Some(completion) = transition.native_completion.take() {
                self.completed_native_transitions.push(completion);
            }
            self.active_transition = None;
        }
    }

    pub(crate) fn complete_active_transition(&mut self) {
        let Some(mut transition) = self.active_transition.take() else {
            return;
        };
        if let Some(completion) = transition.native_completion.take() {
            self.completed_native_transitions.push(completion);
        }
    }

    pub(crate) fn complete_native_transition_for(&mut self, dest: ObjectHandle) {
        let should_complete = self
            .active_transition
            .as_ref()
            .and_then(|transition| transition.native_completion.as_ref())
            .is_some_and(|completion| completion.dest == dest);
        if should_complete {
            self.complete_active_transition();
        }
    }

    pub(crate) fn has_active_transition(&self) -> bool {
        self.active_transition.is_some()
    }

    pub(crate) fn frame_transition(&self) -> Option<FrameTransition> {
        let transition = self.active_transition.as_ref()?;
        let progress = if transition.duration.is_zero() {
            1.0
        } else {
            transition.elapsed.as_secs_f32() / transition.duration.as_secs_f32()
        };
        Some(FrameTransition {
            method: transition.params.method.as_name().to_string(),
            progress: progress.clamp(0.0, 1.0),
            params: transition.params.clone(),
            rule_texture_id: transition.rule_texture_id,
            rule_image_upload: transition.rule_image_upload.clone(),
            frozen_draw_commands: transition.frozen_draw_commands.clone(),
            frozen_image_uploads: transition.frozen_image_uploads.clone(),
        })
    }

    pub(crate) fn suppressed_transition_live_images(&self) -> BTreeSet<LayerId> {
        self.active_transition
            .as_ref()
            .map(|transition| transition.suppressed_live_images.clone())
            .unwrap_or_default()
    }

    pub(crate) fn reapply_transition_live_layer_overrides(&mut self) {
        let Some(overrides) = self
            .active_transition
            .as_ref()
            .map(|transition| transition.live_layer_overrides.clone())
        else {
            return;
        };
        for (layer_id, source) in overrides {
            if let Some(dest) = self.layer_tree.layer_mut(layer_id) {
                copy_layer_node_render_content(dest, &source);
                dest.renderable = source.renderable;
            }
        }
    }

    pub(crate) fn take_completed_native_transitions(&mut self) -> Vec<NativeTransitionCompletion> {
        std::mem::take(&mut self.completed_native_transitions)
    }

    pub(crate) fn backlay_kag_layers(&mut self, layer: Option<&str>) {
        match layer {
            Some(layer) => self.copy_fore_kag_layer_to_pending(layer),
            None => {
                if self.kag_layers.is_empty() {
                    self.ensure_kag_layer("fore", "base");
                }
                let layers = self.kag_layers.keys().cloned().collect::<Vec<_>>();
                for layer in layers {
                    self.copy_fore_kag_layer_to_pending(&layer);
                }
            }
        }
    }

    pub(crate) fn pending_kag_layer_names(&self) -> Vec<String> {
        self.pending_kag_layers.keys().cloned().collect()
    }

    fn pending_kag_layer_mut(&mut self, layer: &str) -> &mut LayerNode {
        let layer_id = self.ensure_kag_layer("fore", layer);
        let base = self
            .layer_tree
            .layer(layer_id)
            .cloned()
            .expect("created KAG layer must exist");
        self.pending_kag_layers
            .entry(layer.to_string())
            .or_insert(base)
    }

    fn copy_fore_kag_layer_to_pending(&mut self, layer: &str) {
        let layer_id = self.ensure_kag_layer("fore", layer);
        if let Some(base) = self.layer_tree.layer(layer_id).cloned() {
            self.pending_kag_layers.insert(layer.to_string(), base);
        }
    }

    fn apply_pending_kag_layers(&mut self) {
        let pending_layers = std::mem::take(&mut self.pending_kag_layers);
        for (layer, source) in pending_layers {
            let target_id = self.ensure_kag_layer("fore", &layer);
            if let Some(target) = self.layer_tree.layer_mut(target_id) {
                let id = target.id;
                let name = target.name.clone();
                let z_order = target.z_order;
                *target = source;
                target.id = id;
                target.name = name;
                target.z_order = z_order;
                target.renderable = true;
            }
        }
    }

    fn next_sibling_z_order(&self, parent: Option<LayerId>) -> i32 {
        self.layer_tree()
            .layers()
            .filter(|layer| layer.parent == parent)
            .map(|layer| layer.z_order)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    fn collect_layer_subtree_ids(&self, root: LayerId, output: &mut BTreeSet<LayerId>) {
        if !output.insert(root) {
            return;
        }
        let children = self
            .layer_tree()
            .layers()
            .filter_map(|layer| (layer.parent == Some(root)).then_some(layer.id))
            .collect::<Vec<_>>();
        for child in children {
            self.collect_layer_subtree_ids(child, output);
        }
    }
}

fn normalize_kag_page(page: &str) -> &str {
    match page {
        "back" | "background" => "back",
        _ => "fore",
    }
}

fn apply_layer_properties_to_node(
    layer: &mut LayerNode,
    properties: &BTreeMap<String, Variant>,
    window_closed: bool,
) {
    layer.left = layer_property_i64(properties, "left", layer.left.round() as i64) as f32;
    layer.top = layer_property_i64(properties, "top", layer.top.round() as i64) as f32;
    layer.width = layer_property_i64(properties, "width", layer.width.round() as i64).max(0) as f32;
    layer.height =
        layer_property_i64(properties, "height", layer.height.round() as i64).max(0) as f32;
    layer.image_left =
        layer_property_i64(properties, "imageLeft", layer.image_left.round() as i64) as f32;
    layer.image_top =
        layer_property_i64(properties, "imageTop", layer.image_top.round() as i64) as f32;
    layer.image_width =
        layer_property_i64(properties, "imageWidth", layer.image_width.round() as i64).max(0)
            as f32;
    layer.image_height =
        layer_property_i64(properties, "imageHeight", layer.image_height.round() as i64).max(0)
            as f32;
    layer.visible =
        layer_property_i64(properties, "visible", i64::from(layer.visible)) != 0 && !window_closed;
    layer.enabled = layer_property_i64(properties, "enabled", i64::from(layer.enabled)) != 0;
    layer.node_enabled =
        layer_property_i64(properties, "nodeEnabled", i64::from(layer.node_enabled)) != 0;
    layer.opacity =
        layer_property_i64(properties, "opacity", i64::from(layer.opacity)).clamp(0, 255) as u8;
    layer.layer_type = layer_property_i64(properties, "type", i64::from(layer.layer_type)) as i32;
    layer.face = layer_property_i64(properties, "face", i64::from(layer.face)) as i32;
    layer.hit_type = layer_property_i64(properties, "hitType", i64::from(layer.hit_type)) as i32;
    layer.hit_threshold =
        layer_property_i64(properties, "hitThreshold", i64::from(layer.hit_threshold))
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    if let Some(z_order) = properties
        .get("absolute")
        .or_else(|| properties.get("order"))
        .and_then(|value| value.to_integer().ok())
    {
        layer.z_order = z_order.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    }
}

fn layer_property_i64(properties: &BTreeMap<String, Variant>, name: &str, fallback: i64) -> i64 {
    properties
        .get(name)
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(fallback)
}

fn copy_layer_node_render_content(dest: &mut LayerNode, source: &LayerNode) {
    dest.left = source.left;
    dest.top = source.top;
    dest.width = source.width;
    dest.height = source.height;
    dest.image_left = source.image_left;
    dest.image_top = source.image_top;
    dest.image_width = source.image_width;
    dest.image_height = source.image_height;
    dest.visible = source.visible;
    dest.enabled = source.enabled;
    dest.node_enabled = source.node_enabled;
    dest.opacity = source.opacity;
    dest.layer_type = source.layer_type;
    dest.face = source.face;
    dest.image = source.image.clone();
}

fn kag_layer_z_order(layer: &str) -> i32 {
    // Proprietary title-screen layers are roots beside the native system UI.
    // Their background/effect must be visited before that UI; the logo is
    // intentionally above it so the artwork can overlap the title panel.
    match layer {
        "__title_image" => return -100,
        "__title_effect" => return -90,
        "__title_logo" => return 100,
        _ => {}
    }
    if layer == "base" || layer == "background" {
        return 0;
    }
    if let Some(index) = layer
        .strip_prefix("message")
        .and_then(|value| value.parse::<i32>().ok())
    {
        return 10_000 + index;
    }
    layer.parse::<i32>().map_or(1_000, |index| 1_000 + index)
}

fn graphic_cache_limit_bytes(limit: i64) -> usize {
    if limit >= 0 {
        let limit = limit as usize;
        if limit == 0 || limit > IMAGE_CACHE_CAPACITY_BYTES {
            IMAGE_CACHE_CAPACITY_BYTES
        } else {
            limit
        }
    } else {
        let remaining = limit.unsigned_abs().min(usize::MAX as u64) as usize;
        IMAGE_CACHE_CAPACITY_BYTES.saturating_sub(remaining)
    }
}

#[derive(Clone)]
pub(crate) struct NativeAudioBuffer {
    pub id: AudioInstanceId,
    pub storage: Option<String>,
    pub looping: bool,
    pub volume: i64,
    pub volume2: i64,
    pub pan: i64,
    pub playing: bool,
}

impl NativeAudioBuffer {
    fn new(id: AudioInstanceId) -> Self {
        Self {
            id,
            storage: None,
            looping: false,
            volume: 100000,
            volume2: 100000,
            pan: 0,
            playing: false,
        }
    }

    fn effective_volume(&self, global_volume: i64) -> f32 {
        krkr_volume_product_to_linear(self.volume, self.volume2, global_volume)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ImageLoadTarget {
    Kag { page: String, layer: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImageLoadRequest {
    pub owner: Option<ObjectHandle>,
    pub target: ImageLoadTarget,
    pub storage: String,
    pub visible: bool,
    pub left: Option<i64>,
    pub top: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub opacity: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingImageLoad {
    request: ImageLoadRequest,
    generation: u64,
    revision: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedImageLoad {
    pub request: ImageLoadRequest,
    pub image: LayerImage,
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) enum ImageLoadState {
    Ready(Box<CompletedImageLoad>),
    Pending,
}

#[derive(Clone)]
struct LayerImageCacheEntry {
    image: LayerImage,
    bytes: usize,
}

#[derive(Clone)]
struct LayerImageCache {
    entries: HashMap<String, LayerImageCacheEntry>,
    lru: VecDeque<String>,
    bytes: usize,
    capacity_bytes: usize,
    max_entry_bytes: usize,
}

impl LayerImageCache {
    fn new(capacity_bytes: usize, max_entry_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            bytes: 0,
            capacity_bytes,
            max_entry_bytes,
        }
    }

    fn get(&mut self, key: &str) -> Option<LayerImage> {
        let image = self.entries.get(key)?.image.clone();
        self.touch(key.to_string());
        Some(image)
    }

    fn insert(&mut self, key: String, image: LayerImage) {
        let bytes = image.upload.rgba.len();
        if bytes > self.max_entry_bytes || bytes > self.capacity_bytes {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries
            .insert(key.clone(), LayerImageCacheEntry { image, bytes });
        self.touch(key);
        self.evict_to_capacity();
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.bytes = 0;
    }

    fn touch(&mut self, key: String) {
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

fn clamp_krkr_volume(volume: i64) -> i64 {
    volume.clamp(0, 100000)
}

fn krkr_volume_product_to_linear(volume: i64, volume2: i64, global_volume: i64) -> f32 {
    let volume = clamp_krkr_volume(volume) as f32 / 100000.0;
    let volume2 = clamp_krkr_volume(volume2) as f32 / 100000.0;
    let global_volume = clamp_krkr_volume(global_volume) as f32 / 100000.0;
    (volume * volume2 * global_volume).clamp(0.0, 1.0)
}

#[derive(Clone)]
struct ActiveTransition {
    params: TransitionParams,
    rule_texture_id: Option<TextureId>,
    rule_image_upload: Option<ImageUpload>,
    elapsed: Duration,
    duration: Duration,
    frozen_draw_commands: Vec<DrawCommand>,
    frozen_image_uploads: Vec<ImageUpload>,
    suppressed_live_images: BTreeSet<LayerId>,
    live_layer_overrides: BTreeMap<LayerId, LayerNode>,
    native_completion: Option<NativeTransitionCompletion>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeTransitionCompletion {
    pub dest: ObjectHandle,
    pub source: Option<ObjectHandle>,
    pub paired_comp: bool,
}

impl TjsHost for KrkrHost {
    fn read_text(&mut self, name: &str, mode: &str) -> Result<String> {
        if storage_mode_offset(mode).is_some() {
            let bytes = self.read_binary(name, mode)?;
            return decode_text_storage(name, &bytes, None, &self.text_encoding);
        }
        self.read_text_storage_for_tjs(name)
    }

    fn read_binary(&mut self, name: &str, mode: &str) -> Result<Vec<u8>> {
        let bytes = self.read_binary_storage_for_tjs(name)?;
        if let Some(offset) = storage_mode_offset(mode) {
            let offset = offset as usize;
            if offset >= bytes.len() {
                return Ok(Vec::new());
            }
            return Ok(bytes[offset..].to_vec());
        }
        Ok(bytes)
    }

    fn write_text(&mut self, name: &str, mode: &str, text: &str) -> Result<()> {
        self.write_text_storage(name, mode, text)
    }

    fn write_binary(&mut self, name: &str, mode: &str, bytes: &[u8]) -> Result<()> {
        self.write_binary_storage(name, mode, bytes)
    }

    fn now_millis(&mut self) -> i64 {
        KrkrHost::now_millis(self)
    }

    fn log(&mut self, message: &str) {
        KrkrHost::log(self, message);
    }

    fn invalidate_object(&mut self, handle: ObjectHandle) {
        self.invalidate_native_object(handle);
    }
}

impl krkr_core::Clock for KrkrHost {
    fn now_millis(&mut self) -> i64 {
        KrkrHost::now_millis(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn advance_clock_offsets_timer_time_without_sleeping() {
        let mut host = KrkrHost::default();
        let before = host.now_millis();
        host.advance_clock(Duration::from_millis(250));
        assert!(host.now_millis() >= before.saturating_add(250));
    }

    #[test]
    fn trace_categories_gate_trace_lines() {
        let mut host = KrkrHost::default();
        host.set_trace_categories("audio, kag");
        assert!(host.trace_enabled(TraceCategory::Audio));
        assert!(host.trace_enabled(TraceCategory::Kag));
        host.trace(TraceCategory::Audio, "open bgm002.ogg");
        host.trace(TraceCategory::Kag, "tag `playbgm`");
        assert_eq!(
            host.logs(),
            &[
                "[audio] open bgm002.ogg".to_string(),
                "[kag] tag `playbgm`".to_string()
            ]
        );
    }

    #[test]
    fn trace_all_enables_every_category_case_insensitively() {
        let mut host = KrkrHost::default();
        host.set_trace_categories("ALL");
        assert!(host.trace_enabled(TraceCategory::Audio));
        assert!(host.trace_enabled(TraceCategory::Kag));
    }

    #[test]
    fn external_catalog_keeps_unique_basename_when_manifest_lists_key_twice() {
        let mut host = KrkrHost::default();
        // Web manifests pass both the logical key and physical URL path. If
        // they are the same file, this must remain a unique auto-path alias.
        host.set_external_resource_catalog(["main/config.tjs", "main/config.tjs"]);
        assert!(host.storage_exists("Config.tjs"));

        // A genuinely ambiguous basename must still be rejected so that a
        // typo cannot silently select an arbitrary resource.
        host.set_external_resource_catalog([
            "main/config.tjs",
            "locale/config.tjs",
            "main/config.tjs",
            "locale/config.tjs",
        ]);
        assert!(!host.storage_exists("Config.tjs"));
    }

    #[test]
    fn first_stub_call_warns_and_later_calls_only_count() {
        let mut host = KrkrHost::default();
        host.record_stub_call("VideoOverlay", "open", "\"op.wmv\"");
        host.record_stub_call("VideoOverlay", "open", "\"ed.wmv\"");
        host.record_stub_call("System", "shellExecute", "");
        assert_eq!(host.stub_call_counts().get("VideoOverlay.open"), Some(&2));
        assert_eq!(host.stub_call_counts().get("System.shellExecute"), Some(&1));
        let warnings = host
            .logs()
            .iter()
            .filter(|line| line.starts_with("WARN stub method called:"))
            .count();
        assert_eq!(warnings, 2);
        assert!(
            host.logs()
                .iter()
                .any(|line| line.contains("VideoOverlay.open(\"op.wmv\")"))
        );
    }

    #[test]
    fn log_cap_drops_oldest_half() {
        let mut host = KrkrHost::default();
        for index in 0..MAX_LOG_LINES {
            host.log(&format!("line {index}"));
        }
        host.log("overflow");
        assert_eq!(host.logs().len(), MAX_LOG_LINES / 2 + 1);
        assert_eq!(host.logs()[0], format!("line {}", MAX_LOG_LINES / 2));
        assert_eq!(host.logs().last().unwrap(), "overflow");
    }

    #[test]
    fn storage_mode_offset_reads_and_writes_struct_tail() {
        let root = temp_root("offset");
        fs::create_dir_all(&root).expect("create root");
        let storage = ProjectStorage::for_root(&root).expect("storage");
        let mut host = KrkrHost::from_storage(storage, SystemPaths::default()).expect("host");

        host.write_binary("savedata/bookmark.bmp", "", b"thumbnail")
            .expect("write thumbnail");
        host.write_text("savedata/bookmark.bmp", "o9", "%[\"answer\" => 42]")
            .expect("write struct tail");

        let bytes = host
            .read_binary("savedata/bookmark.bmp", "")
            .expect("read all");
        assert!(bytes.starts_with(b"thumbnail"));
        assert_eq!(bytes[9..11], [0xff, 0xfe]);
        assert_eq!(
            host.read_text("savedata/bookmark.bmp", "o9")
                .expect("read tail"),
            "%[\"answer\" => 42]"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn temp_root(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "Kirakira-engine-host-{prefix}-{}-{nanos}",
            std::process::id()
        ))
    }
}
