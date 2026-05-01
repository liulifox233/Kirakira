use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use encoding_rs::{Encoding, GBK, SHIFT_JIS, UTF_8};
use krkr_core::{LayerId, LayerImage, LayerTree, ResourceProvider};
use krkr_kag::{KagParser, ParserSnapshot};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, TjsHost},
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
    kag_layers: BTreeMap<String, LayerId>,
    current_kag_page: String,
    current_kag_layer: String,
    image_cache: BTreeMap<String, LayerImage>,
    next_texture_id: u64,
    text_encoding: String,
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
            kag_layers: BTreeMap::new(),
            current_kag_page: "fore".to_string(),
            current_kag_layer: "base".to_string(),
            image_cache: BTreeMap::new(),
            next_texture_id: 1,
            text_encoding: "UTF-8".to_string(),
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
            kag_layers: BTreeMap::new(),
            current_kag_page: "fore".to_string(),
            current_kag_layer: "base".to_string(),
            image_cache: BTreeMap::new(),
            next_texture_id: 1,
            text_encoding: "UTF-8".to_string(),
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

    pub fn text_encoding(&self) -> &str {
        &self.text_encoding
    }

    pub fn set_text_encoding(&mut self, encoding: impl Into<String>) {
        self.text_encoding = encoding.into();
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

    fn read_binary_storage(&self, name: &str) -> Result<Vec<u8>> {
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
            for candidate in storage_lookup_names(name)? {
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
        let names = storage_lookup_names(name)?;
        let mut candidates = Vec::with_capacity(names.len() * (self.auto_paths.len() + 1));
        for name in names {
            let clean = clean_relative_path(&name)?;
            candidates.push(clean.clone());
            for auto_path in self.auto_paths.iter().rev() {
                candidates.extend(auto_path_candidates(auto_path, &clean));
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
        self.kag_parsers.remove(&handle);

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
            self.kag_parsers.remove(&handle);
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

    pub(crate) fn request_layer_paint(&mut self, handle: ObjectHandle) {
        self.pending_layer_paints.insert(handle);
    }

    pub(crate) fn take_pending_layer_paints(&mut self) -> Vec<ObjectHandle> {
        std::mem::take(&mut self.pending_layer_paints)
            .into_iter()
            .collect()
    }

    pub(crate) fn ensure_kag_layer(&mut self, page: &str, layer: &str) -> LayerId {
        let page = normalize_kag_page(page);
        let key = format!("{page}:{layer}");
        match self.kag_layers.entry(key) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let id = self.layer_tree.create_layer(
                    format!("{page}:{layer}"),
                    None,
                    kag_layer_z_order(layer),
                );
                if let Some(node) = self.layer_tree.layer_mut(id) {
                    node.renderable = page == "fore";
                }
                entry.insert(id);
                id
            }
        }
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

    pub(crate) fn apply_immediate_transition(&mut self) {
        let back_layers = self
            .kag_layers
            .iter()
            .filter_map(|(key, source_id)| {
                let layer = key.strip_prefix("back:")?;
                Some((layer.to_string(), *source_id))
            })
            .collect::<Vec<_>>();

        for (layer, source_id) in back_layers {
            let target_id = self.ensure_kag_layer("fore", &layer);
            let Some(source) = self.layer_tree.layer(source_id).cloned() else {
                continue;
            };
            if let Some(target) = self.layer_tree.layer_mut(target_id) {
                let z_order = target.z_order;
                let id = target.id;
                let name = target.name.clone();
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

fn storage_lookup_names(name: &str) -> Result<Vec<String>> {
    let name = normalize_storage_separators(name);
    clean_relative_path(&name)?;
    let path = Path::new(&name);
    if path.extension().is_some() {
        return Ok(vec![name]);
    }

    let mut names = Vec::with_capacity(12);
    names.push(name.clone());
    for extension in [
        "png", "jpg", "jpeg", "bmp", "webp", "ks", "tjs", "asd", "ogg", "wav", "mpg", "mpeg",
    ] {
        names.push(format!("{name}.{extension}"));
    }
    Ok(names)
}

fn normalize_auto_path(path: &str) -> Option<String> {
    let mut path = normalize_storage_separators(path);
    if path.ends_with('>') {
        path.pop();
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
    } else if path.extension().is_some_and(|ext| ext == "xp3") {
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
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(None);
    };
    let mut archives = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "xp3"))
        .collect::<Vec<_>>();
    archives.sort();
    if archives.is_empty() {
        return Ok(None);
    }
    Xp3ResourceProvider::open_archives(archives)
        .map(Some)
        .map_err(|error| TjsError::runtime(format!("failed to open XP3 archives: {error}")))
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
