use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use encoding_rs::{Encoding, GBK, SHIFT_JIS, UTF_8};
use krkr_core::{
    AudioBus, AudioCommand, AudioInstanceId, AudioSourceKind, DrawCommand, FrameTransition,
    ImageUpload, LayerId, LayerImage, LayerNode, LayerTree, ResourceProvider,
};
use krkr_font::FontSystem;
use krkr_kag::{KagParser, ParserSnapshot};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, TjsHost, Variant},
};
use krkr_xp3::Xp3ResourceProvider;

#[derive(Clone)]
pub struct KrkrHost {
    project_root: Option<PathBuf>,
    fs_layers: Vec<ProjectLayer>,
    xp3_provider: Option<Xp3ResourceProvider>,
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
    image_cache: BTreeMap<String, LayerImage>,
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
}

impl Default for KrkrHost {
    fn default() -> Self {
        Self {
            project_root: None,
            fs_layers: Vec::new(),
            xp3_provider: None,
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
            image_cache: BTreeMap::new(),
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
        }
    }
}

impl KrkrHost {
    pub fn for_project(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let fs_layers = project_layers(&root);
        let xp3_provider = open_project_archives(&root)?;
        Ok(Self {
            project_root: Some(root),
            fs_layers,
            xp3_provider,
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
            image_cache: BTreeMap::new(),
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
        })
    }

    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
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
        let path = normalize_storage_separators(&path.into());
        if !self.auto_paths.iter().any(|item| item == &path) {
            self.auto_paths.push(path);
        }
    }

    pub fn remove_auto_path(&mut self, path: &str) -> bool {
        let before = self.auto_paths.len();
        self.auto_paths.retain(|item| item != path);
        before != self.auto_paths.len()
    }

    pub fn clear_archive_cache(&self) -> Result<()> {
        Ok(())
    }

    pub fn storage_exists(&self, name: &str) -> bool {
        self.storage_bytes(name).is_ok()
    }

    pub fn placed_path(&self, name: &str) -> Option<PathBuf> {
        self.find_fs_path(name).ok().flatten()
    }

    pub(crate) fn read_text_storage(&self, name: &str) -> Result<String> {
        if let Some(storage) = self.find_fs_storage(name)? {
            let bytes = fs::read(&storage.path).map_err(io_error)?;
            return decode_text_storage(name, &bytes, storage.encoding_hint, &self.text_encoding);
        }

        let bytes = self.storage_bytes(name)?;
        decode_text_storage(name, &bytes, None, &self.text_encoding)
    }

    pub(crate) fn read_binary_storage(&self, name: &str) -> Result<Vec<u8>> {
        self.storage_bytes(name)
    }

    fn write_text_storage(&self, name: &str, text: &str) -> Result<()> {
        self.write_binary_storage(name, text.as_bytes())
    }

    fn write_binary_storage(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let root = self
            .project_root
            .as_ref()
            .ok_or_else(|| TjsError::runtime("project root is not set"))?;
        let path = storage_write_path(root, name)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        fs::write(&path, bytes).map_err(io_error)
    }

    fn storage_bytes(&self, name: &str) -> Result<Vec<u8>> {
        if let Some(storage) = self.find_fs_storage(name)? {
            return fs::read(&storage.path).map_err(io_error);
        }

        if let Some(provider) = &self.xp3_provider {
            for candidate in self.storage_candidates(name)? {
                if provider.exists(&candidate) {
                    let mut stream = provider.open(&candidate).map_err(io_error)?;
                    let mut bytes = Vec::new();
                    stream.read_to_end(&mut bytes).map_err(io_error)?;
                    return Ok(bytes);
                }
            }
        }

        Err(TjsError::runtime(format!("storage `{name}` not found")))
    }

    fn find_fs_path(&self, name: &str) -> Result<Option<PathBuf>> {
        Ok(self.find_fs_storage(name)?.map(|storage| storage.path))
    }

    fn find_fs_storage(&self, name: &str) -> Result<Option<LocatedStorage>> {
        if let Some(storage) = self.find_absolute_storage(name) {
            return Ok(Some(storage));
        }

        if self.project_root.is_none() {
            return Ok(None);
        };
        for candidate in self.fs_candidates(name)? {
            for layer in self.fs_layers.iter().rev() {
                let path = layer.root.join(&candidate);
                if path.is_file() {
                    return Ok(Some(LocatedStorage {
                        path,
                        encoding_hint: layer.encoding_hint,
                    }));
                }
                if let Some(path) =
                    resolve_case_insensitive_path(&layer.root, &candidate).map_err(io_error)?
                    && path.is_file()
                {
                    return Ok(Some(LocatedStorage {
                        path,
                        encoding_hint: layer.encoding_hint,
                    }));
                }
            }
        }
        Ok(None)
    }

    fn find_absolute_storage(&self, name: &str) -> Option<LocatedStorage> {
        let path = Path::new(name);
        if !path.is_absolute() || !is_safe_absolute_storage_path(path) {
            return None;
        }
        let path = path.to_path_buf();
        if !path.is_file() {
            return None;
        }
        Some(LocatedStorage {
            encoding_hint: self
                .fs_layers
                .iter()
                .find(|layer| path.starts_with(&layer.root))
                .and_then(|layer| layer.encoding_hint)
                .or_else(|| infer_encoding_from_path(&path)),
            path,
        })
    }

    fn fs_candidates(&self, name: &str) -> Result<Vec<PathBuf>> {
        self.storage_candidates(name)?
            .into_iter()
            .map(|candidate| clean_relative_path(&candidate))
            .collect()
    }

    fn storage_candidates(&self, name: &str) -> Result<Vec<String>> {
        let names = storage_lookup_names(name)?;
        let mut candidates = Vec::with_capacity(names.len() * (self.auto_paths.len() + 1));
        for name in names {
            let clean = clean_relative_path(&name)?;
            push_unique_storage_candidate(&mut candidates, &clean);
            for auto_path in self.auto_paths.iter().rev() {
                for candidate in auto_path_candidates(auto_path, &clean) {
                    push_unique_storage_candidate(&mut candidates, &candidate);
                }
            }
        }
        Ok(candidates)
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
        if let Some(image) = self.image_cache.get(name) {
            return Ok(image.clone());
        }

        let bytes = self.read_binary_storage(name)?;
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
        let bytes = self.read_binary_storage(&storage)?;
        if !self.native_audio_buffers.contains_key(&handle) {
            let id = self.allocate_audio_instance_id();
            self.native_audio_buffers
                .insert(handle, NativeAudioBuffer::new(id));
        }
        let buffer = self
            .native_audio_buffers
            .get_mut(&handle)
            .expect("native audio buffer was inserted");
        buffer.storage = Some(storage);
        buffer.bytes = Some(bytes);
        Ok(())
    }

    pub(crate) fn queue_native_audio_play(
        &mut self,
        handle: ObjectHandle,
        bus: AudioBus,
        kind: AudioSourceKind,
    ) -> Result<()> {
        let buffer = self
            .native_audio_buffers
            .get_mut(&handle)
            .ok_or_else(|| TjsError::runtime("WaveSoundBuffer is not initialized"))?;
        let storage = buffer
            .storage
            .clone()
            .ok_or_else(|| TjsError::runtime("WaveSoundBuffer has no opened storage"))?;
        let bytes = buffer
            .bytes
            .clone()
            .ok_or_else(|| TjsError::runtime("WaveSoundBuffer has no decoded source bytes"))?;
        let id = buffer.id;
        let looping = buffer.looping;
        let volume = buffer.effective_volume(self.native_audio_global_volume);
        buffer.playing = true;
        self.pending_audio_commands.push(AudioCommand::Play {
            id,
            bus,
            kind,
            storage,
            bytes,
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
        kind: AudioSourceKind,
        looping: bool,
        volume: f32,
    ) -> Result<AudioInstanceId> {
        let storage = storage.into();
        let bytes = self.read_binary_storage(&storage)?;
        let id = self.allocate_audio_instance_id();
        self.pending_audio_commands.push(AudioCommand::Play {
            id,
            bus,
            kind,
            storage,
            bytes,
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

#[derive(Clone)]
struct ProjectLayer {
    root: PathBuf,
    encoding_hint: Option<&'static Encoding>,
}

#[derive(Clone)]
struct TimerState {
    next_fire_millis: Option<i64>,
}

#[derive(Clone)]
pub(crate) struct NativeAudioBuffer {
    pub id: AudioInstanceId,
    pub storage: Option<String>,
    pub bytes: Option<Vec<u8>>,
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
            bytes: None,
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

#[derive(Clone)]
struct LocatedStorage {
    path: PathBuf,
    encoding_hint: Option<&'static Encoding>,
}

fn project_layers(root: &Path) -> Vec<ProjectLayer> {
    let mut layers = Vec::new();
    push_project_layer(
        &mut layers,
        root.to_path_buf(),
        infer_encoding_from_layer_name(root),
    );
    for name in ["data", "sys", "patch", "patch2", "patch3", "special"] {
        let layer = root.join(name);
        if layer.is_dir() {
            push_project_layer(
                &mut layers,
                layer,
                infer_encoding_from_layer_name(Path::new(name)),
            );
        }
    }
    layers
}

fn push_project_layer(
    layers: &mut Vec<ProjectLayer>,
    root: PathBuf,
    encoding_hint: Option<&'static Encoding>,
) {
    if layers.iter().any(|layer| layer.root == root) {
        return;
    }
    layers.push(ProjectLayer {
        root,
        encoding_hint,
    });
}

fn infer_encoding_from_layer_name(path: &Path) -> Option<&'static Encoding> {
    match path.file_name().and_then(|name| name.to_str()) {
        Some("patch") | Some("patch2") | Some("patch3") | Some("special") => Some(GBK),
        Some("data") | Some("sys") => Some(SHIFT_JIS),
        _ => None,
    }
}

fn infer_encoding_from_path(path: &Path) -> Option<&'static Encoding> {
    path.components().find_map(|component| match component {
        Component::Normal(part) => infer_encoding_from_layer_name(Path::new(part)),
        _ => None,
    })
}

fn auto_path_candidates(auto_path: &str, clean: &Path) -> Vec<PathBuf> {
    let Some(auto_path) = normalize_auto_path(auto_path) else {
        return Vec::new();
    };
    let Ok(auto_relative) = clean_relative_path(&auto_path) else {
        return Vec::new();
    };
    vec![auto_relative.join(clean)]
}

fn push_unique_storage_candidate(candidates: &mut Vec<String>, path: &Path) {
    let candidate = path_to_storage_name(path);
    if !candidates.iter().any(|item| item == &candidate) {
        candidates.push(candidate);
    }
}

fn path_to_storage_name(path: &Path) -> String {
    normalize_storage_separators(&path.to_string_lossy())
}

fn resolve_case_insensitive_path(root: &Path, relative: &Path) -> io::Result<Option<PathBuf>> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Ok(None);
        };
        let exact = current.join(part);
        if exact.exists() {
            current = exact;
            continue;
        }

        let Some(part) = part.to_str() else {
            return Ok(None);
        };
        if !current.is_dir() {
            return Ok(None);
        }
        let mut matched = None;
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(part)
            {
                matched = Some(entry.path());
                break;
            }
        }
        let Some(path) = matched else {
            return Ok(None);
        };
        current = path;
    }
    Ok(Some(current))
}

fn storage_lookup_names(name: &str) -> Result<Vec<String>> {
    let name = normalize_storage_separators(name);
    clean_relative_path(&name)?;
    let path = Path::new(&name);
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(is_known_storage_extension)
    {
        return Ok(vec![name]);
    }

    let mut names = Vec::with_capacity(12);
    names.push(name.clone());
    for extension in [
        "png", "jpg", "jpeg", "bmp", "webp", "ks", "tjs", "asd", "ogg", "wav", "tcw", "mpg", "mpeg",
    ] {
        names.push(format!("{name}.{extension}"));
    }
    Ok(names)
}

fn is_known_storage_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "bmp"
            | "webp"
            | "ks"
            | "tjs"
            | "asd"
            | "ogg"
            | "wav"
            | "tcw"
            | "mpg"
            | "mpeg"
    )
}

fn normalize_auto_path(path: &str) -> Option<String> {
    let path = normalize_storage_separators(path);
    if let Some((_, inner_path)) = path.split_once('>') {
        return Some(inner_path.trim_start_matches('/').to_string());
    }
    if path.is_empty() {
        return Some(String::new());
    }
    let path = Path::new(&path);
    if path.is_absolute() {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .filter(|name| !name.ends_with(".xp3"))
    } else if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xp3"))
    {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
    } else {
        Some(path.to_string_lossy().into_owned())
    }
}

fn decode_text_storage(
    _name: &str,
    bytes: &[u8],
    encoding_hint: Option<&'static Encoding>,
    configured_encoding: &str,
) -> Result<String> {
    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
        return Ok(text);
    }

    let mut encodings = Vec::new();
    if let Some(encoding) = encoding_hint {
        encodings.push(encoding);
    }
    if let Some(encoding) = Encoding::for_label(configured_encoding.as_bytes()) {
        encodings.push(encoding);
    }
    encodings.push(SHIFT_JIS);
    encodings.push(GBK);
    encodings.push(UTF_8);

    for encoding in encodings {
        let (text, _, had_errors) = encoding.decode(bytes);
        if !had_errors {
            return Ok(text.into_owned());
        }
    }

    let encoding = encoding_hint.unwrap_or_else(|| {
        Encoding::for_label(configured_encoding.as_bytes()).unwrap_or(SHIFT_JIS)
    });
    let (text, _, _) = encoding.decode(bytes);
    Ok(text.into_owned())
}

fn normalize_storage_separators(path: &str) -> String {
    path.replace('\\', "/")
}

fn is_safe_absolute_storage_path(path: &Path) -> bool {
    path.components()
        .all(|component| !matches!(component, Component::ParentDir | Component::Prefix(_)))
}

fn storage_write_path(root: &Path, name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        if is_safe_absolute_storage_path(path) {
            return Ok(path.to_path_buf());
        }
        return Err(TjsError::runtime(format!(
            "storage path must be safe: {}",
            path.display()
        )));
    }
    Ok(root.join(clean_relative_path(name)?))
}

impl TjsHost for KrkrHost {
    fn read_text(&mut self, name: &str, _mode: &str) -> Result<String> {
        self.read_text_storage(name)
    }

    fn read_binary(&mut self, name: &str, _mode: &str) -> Result<Vec<u8>> {
        self.read_binary_storage(name)
    }

    fn write_text(&mut self, name: &str, _mode: &str, text: &str) -> Result<()> {
        self.write_text_storage(name, text)
    }

    fn write_binary(&mut self, name: &str, _mode: &str, bytes: &[u8]) -> Result<()> {
        self.write_binary_storage(name, bytes)
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

fn open_project_archives(root: &Path) -> Result<Option<Xp3ResourceProvider>> {
    let archives = project_archive_paths(root);
    if archives.is_empty() {
        return Ok(None);
    }
    Xp3ResourceProvider::open_archives(archives)
        .map(Some)
        .map_err(|error| TjsError::runtime(format!("failed to open XP3 archives: {error}")))
}

fn project_archive_paths(root: &Path) -> Vec<PathBuf> {
    let mut archives = xp3_files_in_directory(&root.join("sys"));
    archives.extend(xp3_files_in_directory(root));
    archives
}

fn xp3_files_in_directory(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut archives = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("xp3"))
        })
        .collect::<Vec<_>>();
    archives.sort();
    archives
}

fn clean_relative_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(TjsError::runtime(format!(
            "storage path must be relative: {}",
            path.display()
        )));
    }

    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(TjsError::runtime(format!(
                    "storage path must stay inside project root: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(clean)
}

fn io_error(error: io::Error) -> TjsError {
    TjsError::runtime(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_candidates_apply_auto_paths_to_xp3_lookups() {
        let mut host = KrkrHost::default();
        host.add_auto_path("bgimage/");
        host.add_auto_path("/tmp/game/sys/bgimage.xp3>");

        let candidates = host.storage_candidates("白").expect("candidates");

        assert!(candidates.iter().any(|candidate| candidate == "白.jpg"));
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate == "bgimage/白.jpg")
        );
    }

    #[test]
    fn normalize_auto_path_uses_archive_inner_prefix() {
        assert_eq!(
            normalize_auto_path("/tmp/game/sys/bgimage.xp3>"),
            Some(String::new())
        );
        assert_eq!(
            normalize_auto_path("/tmp/game/sys/bgimage.xp3>patch/"),
            Some("patch/".to_string())
        );
        assert_eq!(
            normalize_auto_path("/tmp/game/bgimage/"),
            Some("bgimage".to_string())
        );
    }

    #[test]
    fn project_archive_paths_include_sys_archives_before_root_archives() {
        let root = temp_root("archives");
        fs::create_dir_all(root.join("sys")).expect("create sys");
        fs::write(root.join("data.xp3"), []).expect("write data archive");
        fs::write(root.join("patch.xp3"), []).expect("write patch archive");
        fs::write(root.join("sys/bgimage.xp3"), []).expect("write bg archive");
        fs::write(root.join("sys/fgimage.XP3"), []).expect("write fg archive");
        fs::write(root.join("sys/readme.txt"), []).expect("write readme");

        let paths = project_archive_paths(&root)
            .into_iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .expect("strip root")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                "sys/bgimage.xp3",
                "sys/fgimage.XP3",
                "data.xp3",
                "patch.xp3"
            ]
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn filesystem_storage_lookup_falls_back_to_case_insensitive_match() {
        let root = temp_root("case");
        fs::create_dir_all(root.join("patch")).expect("create patch");
        fs::write(root.join("patch/sc_title_bt_Gallery.png"), b"gallery")
            .expect("write mixed-case resource");

        let host = KrkrHost::for_project(&root).expect("host");

        assert_eq!(
            host.read_binary_storage("sc_title_bt_GALLERY.png")
                .expect("read mixed-case resource"),
            b"gallery"
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
