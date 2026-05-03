use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque, btree_map::Entry},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use krkr_core::{
    AudioBus, AudioCommand, AudioInstanceId, AudioLoadPolicy, AudioSourceRef, DrawCommand,
    FrameTransition, ImageUpload, LayerId, LayerImage, LayerNode, LayerTree, ResourceData,
    ResourceProvider,
};
use krkr_font::FontSystem;
use krkr_kag::{KagParser, ParserSnapshot};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, TjsHost, Variant},
};

use crate::{
    resource_manager::{DecodedImageData, ResourceManager, ResourceTaskId},
    storage::{
        ProjectStorage, decode_text_storage, io_error as storage_io_error,
        normalize_storage_separators as storage_normalize_separators, storage_mode_offset,
    },
};

const IMAGE_CACHE_CAPACITY_BYTES: usize = 128 * 1024 * 1024;
const IMAGE_CACHE_MAX_ENTRY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub struct KrkrHost {
    project_root: Option<PathBuf>,
    project_storage: Option<ProjectStorage>,
    resource_manager: Option<ResourceManager>,
    auto_paths: Vec<String>,
    logs: Vec<String>,
    linked_plugins: BTreeSet<String>,
    kag_parsers: BTreeMap<ObjectHandle, KagParser>,
    kag_snapshots: BTreeMap<i64, ParserSnapshot>,
    next_kag_snapshot_id: i64,
    layer_tree: LayerTree,
    native_layers: BTreeMap<ObjectHandle, LayerId>,
    timers: BTreeMap<ObjectHandle, TimerState>,
    pending_async_triggers: BTreeSet<ObjectHandle>,
    pending_layer_paints: BTreeSet<ObjectHandle>,
    continuous_handlers: Vec<Variant>,
    kag_layers: BTreeMap<String, LayerId>,
    pending_kag_layers: BTreeMap<String, LayerNode>,
    active_transition: Option<ActiveTransition>,
    completed_native_transitions: Vec<NativeTransitionCompletion>,
    current_kag_page: String,
    current_kag_layer: String,
    image_cache: LayerImageCache,
    image_cache_revision: u64,
    pending_image_loads: BTreeMap<ResourceTaskId, PendingImageLoad>,
    completed_image_loads: Vec<CompletedImageLoad>,
    image_target_generations: BTreeMap<ImageLoadTarget, u64>,
    #[cfg_attr(test, allow(dead_code))]
    next_resource_generation: u64,
    font_system: FontSystem,
    next_texture_id: u64,
    next_audio_instance_id: u64,
    native_audio_buffers: BTreeMap<ObjectHandle, NativeAudioBuffer>,
    native_audio_global_volume: i64,
    pending_audio_fade_completions: BTreeMap<ObjectHandle, i64>,
    pending_audio_commands: Vec<AudioCommand>,
    text_encoding: String,
    pressed_keys: BTreeSet<i64>,
    termination_requested: bool,
    modal_windows: Vec<ObjectHandle>,
}

impl Default for KrkrHost {
    fn default() -> Self {
        Self {
            project_root: None,
            project_storage: Some(ProjectStorage::new(None, Vec::new(), None, Vec::new())),
            resource_manager: None,
            auto_paths: Vec::new(),
            logs: Vec::new(),
            linked_plugins: BTreeSet::new(),
            kag_parsers: BTreeMap::new(),
            kag_snapshots: BTreeMap::new(),
            next_kag_snapshot_id: 1,
            layer_tree: LayerTree::new(),
            native_layers: BTreeMap::new(),
            timers: BTreeMap::new(),
            pending_async_triggers: BTreeSet::new(),
            pending_layer_paints: BTreeSet::new(),
            continuous_handlers: Vec::new(),
            kag_layers: BTreeMap::new(),
            pending_kag_layers: BTreeMap::new(),
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
            completed_image_loads: Vec::new(),
            image_target_generations: BTreeMap::new(),
            next_resource_generation: 1,
            font_system: FontSystem::new(),
            next_texture_id: 1,
            next_audio_instance_id: 1,
            native_audio_buffers: BTreeMap::new(),
            native_audio_global_volume: 100000,
            pending_audio_fade_completions: BTreeMap::new(),
            pending_audio_commands: Vec::new(),
            text_encoding: "UTF-8".to_string(),
            pressed_keys: BTreeSet::new(),
            termination_requested: false,
            modal_windows: Vec::new(),
        }
    }
}

impl KrkrHost {
    pub fn for_project(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let project_storage = ProjectStorage::for_root(&root)?;
        let resource_manager = ResourceManager::new(project_storage.clone()).map_err(|error| {
            TjsError::runtime(format!("failed to start resource worker: {error}"))
        })?;
        let image_cache_revision = project_storage.revision();
        Ok(Self {
            project_root: Some(root),
            project_storage: Some(project_storage),
            resource_manager: Some(resource_manager),
            auto_paths: Vec::new(),
            logs: Vec::new(),
            linked_plugins: BTreeSet::new(),
            kag_parsers: BTreeMap::new(),
            kag_snapshots: BTreeMap::new(),
            next_kag_snapshot_id: 1,
            layer_tree: LayerTree::new(),
            native_layers: BTreeMap::new(),
            timers: BTreeMap::new(),
            pending_async_triggers: BTreeSet::new(),
            pending_layer_paints: BTreeSet::new(),
            continuous_handlers: Vec::new(),
            kag_layers: BTreeMap::new(),
            pending_kag_layers: BTreeMap::new(),
            active_transition: None,
            completed_native_transitions: Vec::new(),
            current_kag_page: "fore".to_string(),
            current_kag_layer: "base".to_string(),
            image_cache: LayerImageCache::new(
                IMAGE_CACHE_CAPACITY_BYTES,
                IMAGE_CACHE_MAX_ENTRY_BYTES,
            ),
            image_cache_revision,
            pending_image_loads: BTreeMap::new(),
            completed_image_loads: Vec::new(),
            image_target_generations: BTreeMap::new(),
            next_resource_generation: 1,
            font_system: FontSystem::new(),
            next_texture_id: 1,
            next_audio_instance_id: 1,
            native_audio_buffers: BTreeMap::new(),
            native_audio_global_volume: 100000,
            pending_audio_fade_completions: BTreeMap::new(),
            pending_audio_commands: Vec::new(),
            text_encoding: "UTF-8".to_string(),
            pressed_keys: BTreeSet::new(),
            termination_requested: false,
            modal_windows: Vec::new(),
        })
    }

    pub(crate) fn push_modal_window(&mut self, window: ObjectHandle) {
        self.modal_windows.push(window);
    }

    pub(crate) fn current_modal_window(&self) -> Option<ObjectHandle> {
        self.modal_windows.last().copied()
    }

    pub(crate) fn pop_modal_window(&mut self, window: ObjectHandle) {
        if self.modal_windows.last() == Some(&window) {
            self.modal_windows.pop();
        } else {
            self.modal_windows.retain(|entry| *entry != window);
        }
    }

    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }

    pub fn data_path(&self) -> Option<PathBuf> {
        self.project_root.as_ref().map(|root| root.join("savedata"))
    }

    pub fn project_storage(&self) -> Result<&ProjectStorage> {
        self.project_storage
            .as_ref()
            .ok_or_else(|| TjsError::runtime("project storage is not configured"))
    }

    pub fn resource_provider(&self) -> Option<Arc<dyn ResourceProvider>> {
        self.project_storage
            .as_ref()
            .map(|storage| Arc::new(storage.clone()) as Arc<dyn ResourceProvider>)
    }

    pub fn logs(&self) -> &[String] {
        &self.logs
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

    pub(crate) fn read_binary_storage(&self, name: &str) -> Result<Vec<u8>> {
        let data = self.read_resource_storage(name)?;
        data.as_bytes()
            .map(|bytes| bytes.into_owned())
            .map_err(storage_io_error)
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

    fn write_text_storage(&mut self, name: &str, mode: &str, text: &str) -> Result<()> {
        let result = self.project_storage()?.write_text_storage(name, mode, text);
        if result.is_ok() {
            self.invalidate_resource_state();
        }
        result
    }

    fn write_binary_storage(&mut self, name: &str, mode: &str, bytes: &[u8]) -> Result<()> {
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
        self.kag_parsers.insert(handle, parser);
    }

    pub(crate) fn kag_parser(&self, handle: ObjectHandle) -> Option<&KagParser> {
        self.kag_parsers.get(&handle)
    }

    pub(crate) fn take_kag_parser(&mut self, handle: ObjectHandle) -> Option<KagParser> {
        self.kag_parsers.remove(&handle)
    }

    pub(crate) fn store_kag_snapshot(&mut self, snapshot: ParserSnapshot) -> i64 {
        let id = self.next_kag_snapshot_id;
        self.next_kag_snapshot_id += 1;
        self.kag_snapshots.insert(id, snapshot);
        id
    }

    pub(crate) fn kag_snapshot(&self, id: i64) -> Option<&ParserSnapshot> {
        self.kag_snapshots.get(&id)
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
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0)
    }

    pub(crate) fn log(&mut self, message: &str) {
        self.logs.push(message.to_string());
    }

    pub fn layer_tree(&self) -> &LayerTree {
        &self.layer_tree
    }

    pub(crate) fn layer_tree_mut(&mut self) -> &mut LayerTree {
        &mut self.layer_tree
    }

    pub(crate) fn register_native_layer(
        &mut self,
        handle: ObjectHandle,
        name: impl Into<String>,
        parent: Option<LayerId>,
        primary: bool,
    ) -> LayerId {
        if let Some(id) = self.native_layers.get(&handle) {
            return *id;
        }

        let z_order = if primary {
            0
        } else {
            self.next_sibling_z_order(parent)
        };
        let id = self.layer_tree.create_layer(name, parent, z_order);
        if let Some(layer) = self.layer_tree.layer_mut(id)
            && primary
        {
            layer.visible = true;
            layer.opacity = 255;
            layer.layer_type = 1;
        }
        self.native_layers.insert(handle, id);
        id
    }

    pub(crate) fn native_layer(&self, handle: ObjectHandle) -> Option<LayerId> {
        self.native_layers.get(&handle).copied()
    }

    pub(crate) fn native_layer_entries(&self) -> Vec<(ObjectHandle, LayerId)> {
        self.native_layers
            .iter()
            .map(|(handle, layer_id)| (*handle, *layer_id))
            .collect()
    }

    pub(crate) fn native_object_for_layer(&self, layer_id: LayerId) -> Option<ObjectHandle> {
        self.native_layers
            .iter()
            .find_map(|(handle, id)| (*id == layer_id).then_some(*handle))
    }

    pub(crate) fn invalidate_native_object(&mut self, handle: ObjectHandle) {
        self.timers.remove(&handle);
        self.pending_async_triggers.remove(&handle);
        self.pending_layer_paints.remove(&handle);
        self.pending_audio_fade_completions.remove(&handle);
        self.pending_image_loads
            .retain(|_, load| load.request.owner != Some(handle));
        self.kag_parsers.remove(&handle);
        if let Some(buffer) = self.native_audio_buffers.remove(&handle) {
            self.pending_audio_commands.push(AudioCommand::Stop {
                id: buffer.id,
                fade_seconds: 0.0,
            });
        }

        let Some(layer_id) = self.native_layers.get(&handle).copied() else {
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
            .filter_map(|(handle, layer_id)| {
                removed_layer_ids.contains(layer_id).then_some(*handle)
            })
            .collect::<Vec<_>>();
        for handle in removed_handles {
            self.native_layers.remove(&handle);
            self.timers.remove(&handle);
            self.pending_async_triggers.remove(&handle);
            self.pending_layer_paints.remove(&handle);
            self.pending_audio_fade_completions.remove(&handle);
            self.pending_image_loads
                .retain(|_, load| load.request.owner != Some(handle));
            self.kag_parsers.remove(&handle);
            if let Some(buffer) = self.native_audio_buffers.remove(&handle) {
                self.pending_audio_commands.push(AudioCommand::Stop {
                    id: buffer.id,
                    fade_seconds: 0.0,
                });
            }
        }
    }

    pub(crate) fn register_timer(&mut self, handle: ObjectHandle) {
        self.timers.entry(handle).or_insert(TimerState {
            next_fire_millis: None,
        });
    }

    pub(crate) fn timer_handles(&self) -> Vec<ObjectHandle> {
        self.timers.keys().copied().collect()
    }

    pub(crate) fn timer_next_fire_millis(&self, handle: ObjectHandle) -> Option<i64> {
        self.timers
            .get(&handle)
            .and_then(|timer| timer.next_fire_millis)
    }

    pub(crate) fn set_timer_next_fire_millis(
        &mut self,
        handle: ObjectHandle,
        next_fire_millis: Option<i64>,
    ) {
        self.timers
            .entry(handle)
            .or_insert(TimerState {
                next_fire_millis: None,
            })
            .next_fire_millis = next_fire_millis;
    }

    pub(crate) fn register_async_trigger(&mut self, handle: ObjectHandle) {
        self.pending_async_triggers.remove(&handle);
    }

    pub(crate) fn trigger_async(&mut self, handle: ObjectHandle) {
        self.pending_async_triggers.insert(handle);
    }

    pub(crate) fn cancel_async(&mut self, handle: ObjectHandle) {
        self.pending_async_triggers.remove(&handle);
    }

    pub(crate) fn take_pending_async_triggers(&mut self) -> Vec<ObjectHandle> {
        std::mem::take(&mut self.pending_async_triggers)
            .into_iter()
            .collect()
    }

    pub(crate) fn schedule_audio_fade_completion(&mut self, handle: ObjectHandle, millis: i64) {
        let due = self.now_millis().saturating_add(millis.max(0));
        self.pending_audio_fade_completions.insert(handle, due);
    }

    pub(crate) fn cancel_audio_fade_completion(&mut self, handle: ObjectHandle) {
        self.pending_audio_fade_completions.remove(&handle);
    }

    pub(crate) fn take_due_audio_fade_completions(&mut self) -> Vec<ObjectHandle> {
        let now = self.now_millis();
        let due = self
            .pending_audio_fade_completions
            .iter()
            .filter_map(|(handle, due)| (*due <= now).then_some(*handle))
            .collect::<Vec<_>>();
        for handle in &due {
            self.pending_audio_fade_completions.remove(handle);
        }
        due
    }

    pub(crate) fn request_layer_paint(&mut self, handle: ObjectHandle) {
        self.pending_layer_paints.insert(handle);
    }

    pub(crate) fn take_pending_layer_paints(&mut self) -> Vec<ObjectHandle> {
        std::mem::take(&mut self.pending_layer_paints)
            .into_iter()
            .collect()
    }

    pub(crate) fn add_continuous_handler(&mut self, handler: Variant) {
        if !matches!(handler, Variant::Void)
            && !self.continuous_handlers.iter().any(|item| item == &handler)
        {
            self.continuous_handlers.push(handler);
        }
    }

    pub(crate) fn remove_continuous_handler(&mut self, handler: &Variant) -> bool {
        let before = self.continuous_handlers.len();
        self.continuous_handlers.retain(|item| item != handler);
        before != self.continuous_handlers.len()
    }

    pub(crate) fn continuous_handlers(&self) -> Vec<Variant> {
        self.continuous_handlers.clone()
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
        self.sync_image_cache_revision();
        if let Some(image) = self.image_cache.get(name) {
            return Ok(image.clone());
        }

        let data = self.project_storage()?.read_binary_storage(name)?;
        let bytes = data.as_bytes().map_err(storage_io_error)?;
        let decoded = image::load_from_memory(&bytes)
            .map_err(|error| {
                TjsError::runtime(format!("failed to decode image `{name}`: {error}"))
            })?
            .to_rgba8();
        let width = decoded.width();
        let height = decoded.height();
        let rgba = Arc::<[u8]>::from(decoded.into_raw());
        let texture_id = self.next_texture_id;
        self.next_texture_id = self.next_texture_id.saturating_add(1);
        let image = LayerImage::new(texture_id, width, height, rgba);
        self.image_cache.insert(name.to_string(), image.clone());
        Ok(image)
    }

    pub(crate) fn load_image_storage_for_script(&mut self, name: &str) -> Result<LayerImage> {
        self.sync_image_cache_revision();
        if let Some(image) = self.image_cache.get(name) {
            return Ok(image.clone());
        }

        #[cfg(test)]
        {
            return self.load_image_storage(name);
        }

        #[cfg(not(test))]
        {
            let Some(manager) = self.resource_manager.as_ref() else {
                return self.load_image_storage(name);
            };
            let revision = self.storage_revision();
            let decoded = manager
                .decode_image_blocking(name.to_string(), revision)
                .map_err(|error| {
                    TjsError::runtime(format!("failed to decode image `{name}`: {error}"))
                })?;
            if revision != self.storage_revision() {
                return Err(TjsError::runtime(format!(
                    "discarded stale image `{name}` after storage revision changed"
                )));
            }
            let image = self.layer_image_from_decoded(decoded);
            self.image_cache.insert(name.to_string(), image.clone());
            Ok(image)
        }
    }

    pub(crate) fn request_image_load(
        &mut self,
        request: ImageLoadRequest,
    ) -> Result<ImageLoadState> {
        self.sync_image_cache_revision();
        if let Some(image) = self.image_cache.get(&request.storage) {
            return Ok(ImageLoadState::Ready(CompletedImageLoad { request, image }));
        }

        #[cfg(test)]
        {
            let image = self.load_image_storage(&request.storage)?;
            return Ok(ImageLoadState::Ready(CompletedImageLoad { request, image }));
        }

        #[cfg(not(test))]
        {
            let Some(manager) = self.resource_manager.as_ref() else {
                let image = self.load_image_storage(&request.storage)?;
                return Ok(ImageLoadState::Ready(CompletedImageLoad { request, image }));
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
            return Ok(image.upload.rgba.len());
        }

        #[cfg(not(test))]
        {
            if let Some(manager) = self.resource_manager.as_ref() {
                let revision = self.storage_revision();
                let decoded = manager
                    .decode_image_blocking(name.to_string(), revision)
                    .map_err(|error| {
                        TjsError::runtime(format!("failed to decode image `{name}`: {error}"))
                    })?;
                if revision != self.storage_revision() {
                    return Err(TjsError::runtime(format!(
                        "discarded stale image `{name}` after storage revision changed"
                    )));
                }
                return Ok(decoded.rgba.len());
            }

            let image = self.load_image_storage(name)?;
            Ok(image.upload.rgba.len())
        }
    }

    pub(crate) fn take_completed_image_loads(&mut self) -> Vec<CompletedImageLoad> {
        self.poll_resource_completions();
        std::mem::take(&mut self.completed_image_loads)
    }

    pub(crate) fn has_pending_resource_loads(&self) -> bool {
        !self.pending_image_loads.is_empty()
    }

    fn poll_resource_completions(&mut self) {
        let Some(manager) = self.resource_manager.as_ref() else {
            return;
        };
        let completions = manager.drain_completions();
        for completion in completions {
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
            self.image_cache.clear();
            self.completed_image_loads.clear();
            self.pending_image_loads.clear();
            self.image_target_generations.clear();
            self.image_cache_revision = revision;
        }
    }

    fn invalidate_resource_state(&mut self) {
        self.image_cache_revision = self.storage_revision();
        self.image_cache.clear();
        self.pending_image_loads.clear();
        self.completed_image_loads.clear();
        self.image_target_generations.clear();
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

    pub(crate) fn font_system_mut(&mut self) -> &mut FontSystem {
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

    pub(crate) fn begin_kag_transition(&mut self, method: &str, duration: Duration) {
        self.complete_active_transition();
        if self.pending_kag_layers.is_empty() {
            self.active_transition = None;
            return;
        }

        if duration.is_zero() {
            self.apply_immediate_transition();
            return;
        }

        let (frozen_draw_commands, frozen_image_uploads) = self.layer_tree.draw_model();
        self.apply_pending_kag_layers();
        self.active_transition = Some(ActiveTransition {
            method: normalize_transition_method(method).to_string(),
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
        method: &str,
        duration: Duration,
        frozen_model: (Vec<DrawCommand>, Vec<ImageUpload>),
        suppressed_live_images: BTreeSet<LayerId>,
        live_layer_overrides: BTreeMap<LayerId, LayerNode>,
        completion: NativeTransitionCompletion,
    ) {
        self.complete_active_transition();
        if duration.is_zero() {
            self.completed_native_transitions.push(completion);
            self.active_transition = None;
            return;
        }

        self.active_transition = Some(ActiveTransition {
            method: normalize_transition_method(method).to_string(),
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
            method: transition.method.clone(),
            progress: progress.clamp(0.0, 1.0),
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

fn normalize_transition_method(method: &str) -> &str {
    match method {
        "crossfade" | "" => "crossfade",
        _ => "crossfade",
    }
}

fn kag_layer_z_order(layer: &str) -> i32 {
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
struct TimerState {
    next_fire_millis: Option<i64>,
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
    Ready(CompletedImageLoad),
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
    method: String,
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
        self.read_text_storage(name)
    }

    fn read_binary(&mut self, name: &str, mode: &str) -> Result<Vec<u8>> {
        let bytes = self.read_binary_storage(name)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn storage_mode_offset_reads_and_writes_struct_tail() {
        let root = temp_root("offset");
        fs::create_dir_all(&root).expect("create root");
        let mut host = KrkrHost::for_project(&root).expect("host");

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
