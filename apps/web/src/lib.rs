//! Minimal browser shell for the platform-neutral runtime.
//!
//! The shell deliberately owns DOM/canvas concerns.  Rendering and engine
//! state stay in Rust crates so a future WebGPU/WebGL2 adapter can be swapped
//! without changing KAG/TJS code.

use krkr_core::{
    AssetEvent, AssetKind, AssetRequestId, AudioCommand, AudioError, AudioEvent, AudioSink,
};

#[cfg(target_arch = "wasm32")]
use krkr_core::{AssetScheduler, SaveEvent, SaveRequestId, SaveStore};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

#[derive(Default)]
pub struct WebInputQueue {
    events: Vec<krkr_core::EngineEvent>,
}

impl WebInputQueue {
    pub fn push(&mut self, event: krkr_core::EngineEvent) {
        self.events.push(event);
    }

    pub fn drain(&mut self) -> Vec<krkr_core::EngineEvent> {
        std::mem::take(&mut self.events)
    }
}

/// Browser entry point.  This only validates/returns the canvas element; the
/// host application is responsible for creating a wgpu surface and driving
/// `krkr_engine::RuntimeSession` once the runtime is wired in.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn attach_canvas(canvas_id: &str) -> Result<(), wasm_bindgen::JsValue> {
    let window =
        web_sys::window().ok_or_else(|| wasm_bindgen::JsValue::from_str("window unavailable"))?;
    let document = window
        .document()
        .ok_or_else(|| wasm_bindgen::JsValue::from_str("document unavailable"))?;
    let canvas = document
        .get_element_by_id(canvas_id)
        .ok_or_else(|| wasm_bindgen::JsValue::from_str("canvas element not found"))?;
    canvas
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| wasm_bindgen::JsValue::from_str("element is not a canvas"))?;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn attach_canvas(_canvas_id: &str) -> Result<(), String> {
    Err("krkr-web canvas attachment is only available on wasm32".to_string())
}

/// Marker used by JS glue to describe the static package protocol.
pub const PACKAGE_MANIFEST_NAME: &str = "manifest.json";

// Keep the browser-facing asset vocabulary discoverable from this crate while
// the asynchronous fetch implementation is added. These aliases also make
// generated TypeScript bindings stable across the migration.
pub type WebAssetEvent = AssetEvent;
pub type WebAssetKind = AssetKind;
pub type WebAssetRequestId = AssetRequestId;

#[cfg(target_arch = "wasm32")]
use krkr_engine::{EngineInput, KrkrEngine, RuntimeSession};
#[cfg(target_arch = "wasm32")]
use krkr_render::{PhysicalSize, Renderer};

#[cfg(target_arch = "wasm32")]
use std::time::Duration;

#[cfg(target_arch = "wasm32")]
struct WebClock;

#[cfg(target_arch = "wasm32")]
fn web_log(message: impl AsRef<str>) {
    web_sys::console::info_1(&wasm_bindgen::JsValue::from_str(&format!(
        "[kirakira] {}",
        message.as_ref()
    )));
}

#[cfg(target_arch = "wasm32")]
fn web_warn(message: impl AsRef<str>) {
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
        "[kirakira] {}",
        message.as_ref()
    )));
}

#[cfg(target_arch = "wasm32")]
fn persistent_storage_prefix(game: &str) -> String {
    format!("kirakira.save.v1.{}.", encode_hex(game.as_bytes()))
}

#[cfg(target_arch = "wasm32")]
fn profile_storage_key(game: &str, profile: &str, key: &str) -> String {
    format!(
        "{}{}:{}",
        persistent_storage_prefix(game),
        encode_hex(profile.as_bytes()),
        encode_hex(key.as_bytes())
    )
}

/// Small synchronous browser adapter for the asynchronous SaveStore
/// protocol. `localStorage` is used as a portable baseline; applications can
/// replace it with IndexedDB without changing the engine/session API.
#[cfg(target_arch = "wasm32")]
struct WebSaveStore {
    game: String,
    next_id: u64,
    events: Vec<SaveEvent>,
}

#[cfg(target_arch = "wasm32")]
impl WebSaveStore {
    fn new(game: impl Into<String>) -> Self {
        Self {
            game: game.into(),
            next_id: 0,
            events: Vec::new(),
        }
    }

    fn next_id(&mut self) -> SaveRequestId {
        self.next_id = self.next_id.saturating_add(1);
        SaveRequestId(self.next_id)
    }
}

#[cfg(target_arch = "wasm32")]
impl SaveStore for WebSaveStore {
    fn load(&mut self, profile: &str, key: &str) -> SaveRequestId {
        let id = self.next_id();
        let data = web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .and_then(|storage| {
                storage
                    .get_item(&profile_storage_key(&self.game, profile, key))
                    .ok()
                    .flatten()
            })
            .and_then(|value| decode_hex(&value))
            .map(std::sync::Arc::from);
        self.events.push(SaveEvent::Loaded {
            id,
            profile: profile.to_string(),
            key: key.to_string(),
            data,
        });
        id
    }

    fn save(&mut self, profile: &str, key: &str, data: std::sync::Arc<[u8]>) -> SaveRequestId {
        let id = self.next_id();
        let result = web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .ok_or_else(|| "localStorage is unavailable".to_string())
            .and_then(|storage| {
                storage
                    .set_item(
                        &profile_storage_key(&self.game, profile, key),
                        &encode_hex(&data),
                    )
                    .map_err(|_| "localStorage write failed".to_string())
            });
        match result {
            Ok(()) => self.events.push(SaveEvent::Saved {
                id,
                profile: profile.to_string(),
                key: key.to_string(),
            }),
            Err(message) => self.events.push(SaveEvent::Failed {
                id,
                profile: profile.to_string(),
                key: key.to_string(),
                message,
            }),
        }
        id
    }

    fn poll(&mut self) -> Vec<SaveEvent> {
        std::mem::take(&mut self.events)
    }
}

#[cfg(target_arch = "wasm32")]
fn load_web_persistent_files(game: &str) -> Vec<(String, Vec<u8>)> {
    let Some(storage) = web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    else {
        return Vec::new();
    };
    let prefix = persistent_storage_prefix(game);
    let mut files = Vec::new();
    let Ok(length) = storage.length() else {
        return files;
    };
    for index in 0..length {
        let Ok(Some(key)) = storage.key(index) else {
            continue;
        };
        let Some(path_hex) = key.strip_prefix(&prefix) else {
            continue;
        };
        let Some(path) = decode_hex(path_hex).and_then(|bytes| String::from_utf8(bytes).ok())
        else {
            continue;
        };
        let Ok(Some(value)) = storage.get_item(&key) else {
            continue;
        };
        let Some(bytes) = decode_hex(&value) else {
            continue;
        };
        files.push((path, bytes));
    }
    files
}

#[cfg(target_arch = "wasm32")]
fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(target_arch = "wasm32")]
fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Some(bytes)
}

#[cfg(target_arch = "wasm32")]
fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(target_arch = "wasm32")]
impl krkr_core::Clock for WebClock {
    fn now_millis(&mut self) -> i64 {
        web_sys::window()
            .and_then(|window| window.performance())
            .map(|performance| performance.now().max(0.0) as i64)
            .unwrap_or(0)
    }
}

/// A browser-owned runtime session. The shell drives `tick` from
/// `requestAnimationFrame`; all engine side effects remain behind the same
/// RuntimeSession boundary used by desktop hosts.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub struct WebRuntime {
    session: RuntimeSession,
    last_timestamp_ms: Option<f64>,
    viewport_width: f32,
    viewport_height: f32,
    device_pixel_ratio: f64,
    safe_area: krkr_core::SafeAreaInsets,
    orientation: krkr_core::Orientation,
    pending_events: Vec<krkr_core::EngineEvent>,
    pending_text: Vec<krkr_core::TextInputEvent>,
    package_base_url: Option<String>,
    package_manifest: Option<WebManifest>,
    package_cache: Option<Rc<RefCell<ByteCache>>>,
    pending_scenario: Option<String>,
    renderer: Option<Renderer>,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
impl WebRuntime {
    #[wasm_bindgen::prelude::wasm_bindgen(constructor)]
    pub fn new(width: f32, height: f32) -> Result<WebRuntime, wasm_bindgen::JsValue> {
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&info.to_string()));
        }));
        web_log("engine creating");
        let engine = KrkrEngine::new(krkr_engine::EngineConfig::default())
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
        let mut engine = engine;
        krkr_plugins::register_reference_plugins(&mut engine)
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
        web_log("engine ready");
        Ok(Self {
            session: RuntimeSession::new(
                engine,
                Box::new(FetchAssetStore::default()),
                Box::new(WebAudioSink::default()),
                Box::new(WebClock),
            ),
            last_timestamp_ms: None,
            viewport_width: width.max(1.0),
            viewport_height: height.max(1.0),
            device_pixel_ratio: 1.0,
            safe_area: krkr_core::SafeAreaInsets::default(),
            orientation: krkr_core::Orientation::default(),
            pending_events: Vec::new(),
            pending_text: Vec::new(),
            package_base_url: None,
            package_manifest: None,
            package_cache: None,
            pending_scenario: None,
            renderer: None,
        })
    }

    /// Installs the shared `krkr-render` WebGPU backend on the browser canvas.
    /// Returns `false` when the browser declines WebGPU so the shell can use
    /// its deterministic Canvas2D fallback.
    pub async fn init_renderer(
        &mut self,
        canvas_id: String,
    ) -> Result<bool, wasm_bindgen::JsValue> {
        let Some(window) = web_sys::window() else {
            return Ok(false);
        };
        let Some(document) = window.document() else {
            return Ok(false);
        };
        let Some(element) = document.get_element_by_id(&canvas_id) else {
            return Err(wasm_bindgen::JsValue::from_str("canvas element not found"));
        };
        let canvas = element
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|_| wasm_bindgen::JsValue::from_str("element is not a canvas"))?;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = match instance.create_surface(wgpu::SurfaceTarget::Canvas(canvas)) {
            Ok(surface) => surface,
            Err(error) => {
                web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "WebGPU surface unavailable: {error}"
                )));
                return Ok(false);
            }
        };
        let renderer = match Renderer::new_with_surface(
            instance,
            surface,
            self.physical_viewport_size(),
            self.device_pixel_ratio,
        )
        .await
        {
            Ok(renderer) => renderer,
            Err(error) => {
                web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "WebGPU adapter unavailable: {error}"
                )));
                return Ok(false);
            }
        };
        self.renderer = Some(renderer);
        Ok(true)
    }

    /// Downloads a semantic-path v1 static game package and switches the
    /// running session to it. Only manifest bootstrap entries are fetched up
    /// front; all other files are requested lazily by `WebResourceStore`.
    pub async fn load_package(&mut self, base_url: String) -> Result<(), wasm_bindgen::JsValue> {
        web_log(format!("package manifest fetch started: {base_url}"));
        let manifest_bytes = fetch_bytes(&package_url(&base_url, PACKAGE_MANIFEST_NAME))
            .await
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error))?;
        web_log(format!(
            "package manifest fetched: {} bytes",
            manifest_bytes.len()
        ));
        let manifest =
            WebManifest::from_json(&String::from_utf8_lossy(&manifest_bytes)).map_err(|error| {
                wasm_bindgen::JsValue::from_str(&format!("invalid Web manifest: {error}"))
            })?;
        if manifest.format != krkr_assets::WEB_PACKAGE_FORMAT {
            return Err(wasm_bindgen::JsValue::from_str(&format!(
                "unsupported Web manifest format {}; expected {}",
                manifest.format,
                krkr_assets::WEB_PACKAGE_FORMAT
            )));
        }
        if let Some(entry) = manifest.entry.as_deref() {
            let Some(asset) = manifest.entry(entry) else {
                return Err(wasm_bindgen::JsValue::from_str(&format!(
                    "manifest entry is missing from entries: {entry}"
                )));
            };
            if asset.kind != "script" {
                return Err(wasm_bindgen::JsValue::from_str(&format!(
                    "manifest entry is not a script: {entry}"
                )));
            }
        }
        web_log(format!(
            "manifest entry: {}",
            manifest.entry.as_deref().unwrap_or("<none>")
        ));
        web_log(format!(
            "detected Web manifest v1: {} entries, {} bootstrap files",
            manifest.entries.len(),
            manifest.bootstrap.len()
        ));
        self.load_web_package(base_url, manifest).await
    }

    /// Loads a KAG scenario after the package bootstrap. Keeping this
    /// separate from `load_package` lets a static publication choose its
    /// entry scenario without putting a package path in the URL.
    pub fn load_scenario(&mut self, storage: String) -> Result<bool, wasm_bindgen::JsValue> {
        let storage = storage.trim();
        if storage.is_empty() {
            return Ok(true);
        }
        web_log(format!("scenario load started: {storage}"));
        // `startup.tjs` is the game's real bootstrap.  On the browser it may
        // still be suspended while one of its lazy scripts is being fetched.
        // Do not inject a host-selected scenario into that suspended VM: the
        // desktop host would finish startup first, and game scripts (such as
        // game scripts may create and drive their own KAG parser during that
        // continuation.
        if self.session.engine().is_script_suspended()
            || self
                .session
                .engine()
                .host()
                .has_pending_external_resources()
            || self.session.engine().host().has_pending_resource_loads()
        {
            self.pending_scenario = Some(storage.to_string());
            web_log(format!(
                "scenario deferred until startup settles: {storage}"
            ));
            return Ok(false);
        }
        match self.session.engine_mut().load_kag_scenario(storage) {
            Ok(()) => {
                self.pending_scenario = None;
                self.last_timestamp_ms = None;
                web_log(format!("scenario ready: {storage}"));
                Ok(true)
            }
            Err(error) if error.to_string().contains("resource is pending") => {
                self.pending_scenario = Some(storage.to_string());
                web_log(format!("scenario waiting for resource: {storage}"));
                Ok(false)
            }
            Err(error) => Err(wasm_bindgen::JsValue::from_str(&error.to_string())),
        }
    }

    /// Returns the entry scenario selected by the static publication.
    ///
    /// There is intentionally no fallback based on names such as `first.ks`
    /// or `title.ks`: those are often dispatchers with game-specific first-run
    /// behavior. The publisher must choose the scenario explicitly.
    pub fn entry_scenario(&self) -> String {
        self.package_manifest
            .as_ref()
            .and_then(|manifest| manifest.entry.clone())
            .unwrap_or_default()
    }

    /// Returns the stable package identity used to namespace browser saves.
    pub fn package_game(&self) -> String {
        self.package_manifest
            .as_ref()
            .map(|manifest| manifest.game.clone())
            .unwrap_or_default()
    }

    /// Requests a profile save through the host-owned SaveStore. The request
    /// is pollable from the next `tick` result and is independent from the
    /// read-only game package.
    pub fn load_save(
        &mut self,
        profile: String,
        key: String,
    ) -> Result<u64, wasm_bindgen::JsValue> {
        self.session
            .request_save_load(&profile, &key)
            .map(|id| id.0)
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("save store is not configured"))
    }

    pub fn save_save(
        &mut self,
        profile: String,
        key: String,
        data: js_sys::Uint8Array,
    ) -> Result<u64, wasm_bindgen::JsValue> {
        self.session
            .request_save(&profile, &key, std::sync::Arc::from(data.to_vec()))
            .map(|id| id.0)
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("save store is not configured"))
    }

    async fn load_web_package(
        &mut self,
        base_url: String,
        manifest: WebManifest,
    ) -> Result<(), wasm_bindgen::JsValue> {
        // A package switch is a new runtime session. Do not deliver input or
        // deferred scenario commands queued for the previous game into the
        // replacement engine.
        self.pending_events.clear();
        self.pending_text.clear();
        self.pending_scenario = None;
        self.last_timestamp_ms = None;
        let manifest_entry_count = manifest.entries.len();
        let mut files = Vec::new();
        web_log(format!(
            "Web v1 bootstrap started: {} files",
            manifest.bootstrap.len()
        ));
        for path in &manifest.bootstrap {
            let entry = manifest.entry(path).ok_or_else(|| {
                wasm_bindgen::JsValue::from_str(&format!("bootstrap entry is missing: {path}"))
            })?;
            web_log(format!(
                "bootstrap fetch started: {} ({} bytes)",
                entry.path, entry.size
            ));
            let bytes = fetch_web_manifest_entry(&base_url, entry)
                .await
                .map_err(|error| wasm_bindgen::JsValue::from_str(&error))?;
            web_log(format!(
                "bootstrap ready: {} ({} bytes)",
                entry.path,
                bytes.len()
            ));
            files.push((entry.path.clone(), bytes.clone()));
            if path != &entry.path {
                files.push((path.clone(), bytes.clone()));
            }
            // KRKR's startup scripts frequently use a basename resolved via
            // an auto path (for example `Config.tjs` for `main/config.tjs`).
            // Materialize that unambiguous alias in the bootstrap overlay so
            // initialization remains synchronous and does not refetch an
            // already bootstrapped object.
            if let Some(name) = path.rsplit('/').next()
                && name != path
                && manifest.entry(name).is_some()
            {
                files.push((name.to_string(), bytes));
            }
        }
        let persisted = load_web_persistent_files(&manifest.game);
        if !persisted.is_empty() {
            web_log(format!(
                "restoring {} persisted storage files for {}",
                persisted.len(),
                manifest.game
            ));
            files.extend(persisted);
        }
        let catalog_paths = manifest
            .entries
            .iter()
            .flat_map(|(key, entry)| [key.clone(), entry.path.clone()]);
        let storage =
            krkr_assets::ProjectStorage::from_memory_with_catalog(files.clone(), catalog_paths);
        let viewport_width = self.viewport_width.round().max(1.0) as i64;
        let viewport_height = self.viewport_height.round().max(1.0) as i64;
        let mut engine = KrkrEngine::new(krkr_engine::EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            system_paths: krkr_engine::SystemPaths {
                // These are logical virtual paths, never host filesystem
                // paths. Save writes are routed through WebSaveStore.
                exe_path: "./".to_string(),
                data_path: "savedata/".to_string(),
                personal_path: "savedata/".to_string(),
                app_data_path: "savedata/".to_string(),
            },
            system_metrics: krkr_engine::SystemMetrics {
                screen_width: viewport_width,
                screen_height: viewport_height,
                desktop_left: 0,
                desktop_top: 0,
                desktop_width: viewport_width,
                desktop_height: viewport_height,
            },
            // The browser shell owns presentation through HTMLVideoElement;
            // injecting the Web capability profile keeps backend selection
            // outside krkr-engine even though decoder creation is bypassed.
            video_factory: std::sync::Arc::new(krkr_video::PlatformVideoFactory),
            ..krkr_engine::EngineConfig::default()
        })
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
        krkr_plugins::register_reference_plugins(&mut engine)
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
        engine.set_external_resource_catalog(
            manifest
                .entries
                .iter()
                .flat_map(|(key, entry)| [key.clone(), entry.path.clone()]),
        );
        let package_base_url = base_url.clone();
        let manifest_bootstrap_count = manifest.bootstrap.len();
        let resource_store = WebResourceStore::new(base_url, manifest.clone());
        // Bootstrap bytes are already in the startup overlay. Prime the
        // scheduler cache too, so media bridges and aliases reuse them rather
        // than issuing a second request for the same semantic asset.
        for (path, bytes) in &files {
            if let Some(entry) = manifest.entry(path) {
                resource_store.cache.borrow_mut().insert(
                    normalize_web_path(&entry.path),
                    std::sync::Arc::from(bytes.clone()),
                );
            }
        }
        let package_cache = Rc::clone(&resource_store.cache);
        self.package_manifest = Some(manifest.clone());
        self.session = RuntimeSession::new(
            engine,
            Box::new(resource_store),
            Box::new(WebAudioSink::default()),
            Box::new(WebClock),
        );
        if let Err(error) = self.session.start_project()
            && !error.to_string().contains("resource is pending")
        {
            return Err(wasm_bindgen::JsValue::from_str(&error.to_string()));
        }
        self.session
            .set_save_store(Box::new(WebSaveStore::new(manifest.game.clone())));
        self.package_base_url = Some(package_base_url);
        self.package_cache = Some(package_cache);
        self.last_timestamp_ms = None;
        web_log(format!(
            "Web v1 package ready: {manifest_entry_count} entries, {} bootstrap files",
            manifest_bootstrap_count
        ));
        Ok(())
    }

    pub fn load_video(&self, storage: String) -> js_sys::Promise {
        self.load_storage(storage)
    }

    /// Fetches a packaged resource by its engine storage name. The browser
    /// audio/video bridges use the same semantic URL as the asset scheduler.
    pub fn load_storage(&self, storage: String) -> js_sys::Promise {
        web_log(format!("direct resource load requested: {storage}"));
        let normalized_storage = normalize_web_path(&storage);
        let request = self.package_base_url.as_ref().and_then(|base| {
            self.package_manifest
                .as_ref()
                .and_then(|manifest| manifest.entry(&storage).cloned())
                .map(|entry| {
                    let cache_key = normalize_web_path(&entry.path);
                    (base.clone(), entry, cache_key)
                })
        });
        let cached = self.package_cache.as_ref().and_then(|cache| {
            let key = request
                .as_ref()
                .map(|(_, _, cache_key)| cache_key.as_str())
                .unwrap_or(&normalized_storage);
            cache
                .borrow_mut()
                .get_case_insensitive(key)
                .map(|bytes| bytes.to_vec())
        });
        let cache = self.package_cache.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            if let Some(bytes) = cached {
                web_log(format!(
                    "direct resource cache hit: {storage} ({} bytes)",
                    bytes.len()
                ));
                return Ok(js_sys::Uint8Array::from(bytes.as_slice()).into());
            }
            let Some((base, request, cache_key)) = request else {
                return Err(wasm_bindgen::JsValue::from_str(
                    "resource is not available in the loaded package",
                ));
            };
            let bytes = fetch_web_manifest_entry(&base, &request)
                .await
                .map_err(|error| wasm_bindgen::JsValue::from_str(&error))?;
            if let Some(cache) = cache {
                cache
                    .borrow_mut()
                    .insert(cache_key, std::sync::Arc::from(bytes.clone()));
            }
            Ok(js_sys::Uint8Array::from(bytes.as_slice()).into())
        })
    }

    pub fn notify_video_ended(&mut self, id: String) -> Result<(), wasm_bindgen::JsValue> {
        let id = id.parse::<u64>().map_err(|_| {
            wasm_bindgen::JsValue::from_str(&format!("invalid video overlay id: {id}"))
        })?;
        self.session
            .engine_mut()
            .notify_video_ended(id)
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
    }

    /// Delivers a WebAudio buffer-source completion to the engine.  Browser
    /// audio is driven by JavaScript, so without this explicit bridge KAG
    /// `wait`/conductor callbacks would remain suspended forever.
    pub fn notify_audio_stopped(&mut self, id: u64) -> Result<(), wasm_bindgen::JsValue> {
        self.session
            .engine_mut()
            .notify_audio_stopped(krkr_core::AudioInstanceId(id))
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
    }

    /// Returns the current engine hit-test candidates at a CSS/logical point.
    /// This is intentionally diagnostic-only and is consumed by the browser
    /// play tool when explaining why a control did (or did not) receive a
    /// click.
    pub fn inspect_pointer(
        &mut self,
        x: f32,
        y: f32,
    ) -> Result<js_sys::Array, wasm_bindgen::JsValue> {
        let candidates = self
            .session
            .engine_mut()
            .inspect_pointer(krkr_core::Point::new(x, y))
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
        let result = js_sys::Array::new();
        for candidate in candidates {
            result.push(&wasm_bindgen::JsValue::from_str(&candidate));
        }
        Ok(result)
    }

    /// Evaluates a TJS expression for host-side diagnostics.  The browser
    /// play tool uses this only in debug sessions to inspect hook/link state;
    /// it does not participate in normal game execution.
    pub fn debug_eval(&mut self, expression: String) -> Result<String, wasm_bindgen::JsValue> {
        self.session
            .engine_mut()
            .execute_expression("web-debug", &expression)
            .map(|value| format!("{value:?}"))
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
    }

    fn physical_viewport_size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(
            (self.viewport_width as f64 * self.device_pixel_ratio).round() as u32,
            (self.viewport_height as f64 * self.device_pixel_ratio).round() as u32,
        )
    }

    pub fn resize(&mut self, width: f32, height: f32) {
        self.viewport_width = width.max(1.0);
        self.viewport_height = height.max(1.0);
        let width = self.viewport_width.round() as i64;
        let height = self.viewport_height.round() as i64;
        self.session
            .engine_mut()
            .set_system_metrics(krkr_engine::SystemMetrics {
                screen_width: width,
                screen_height: height,
                desktop_left: 0,
                desktop_top: 0,
                desktop_width: width,
                desktop_height: height,
            });
        let physical_size = self.physical_viewport_size();
        if let Some(renderer) = &mut self.renderer {
            renderer.resize(physical_size, self.device_pixel_ratio);
        }
    }

    /// Updates the browser backing-store scale independently from the logical
    /// game viewport. This keeps WebGPU pixels aligned with HiDPI Canvas CSS
    /// coordinates instead of stretching a low-resolution surface.
    pub fn set_device_pixel_ratio(&mut self, ratio: f64) {
        self.device_pixel_ratio = ratio.max(1.0);
        let physical_size = self.physical_viewport_size();
        if let Some(renderer) = &mut self.renderer {
            renderer.resize(physical_size, self.device_pixel_ratio);
        }
    }

    pub fn set_safe_area(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        self.safe_area = krkr_core::SafeAreaInsets {
            left: left.max(0.0),
            top: top.max(0.0),
            right: right.max(0.0),
            bottom: bottom.max(0.0),
        };
    }

    pub fn set_orientation(&mut self, orientation: String) {
        self.orientation = if orientation.eq_ignore_ascii_case("portrait") {
            krkr_core::Orientation::Portrait
        } else {
            krkr_core::Orientation::Landscape
        };
    }

    pub fn suspend_renderer(&mut self) {
        if let Some(renderer) = &mut self.renderer {
            renderer.suspend();
        }
    }

    pub fn resume_renderer(&mut self) {
        let physical_size = self.physical_viewport_size();
        if let Some(renderer) = &mut self.renderer {
            renderer.resume(physical_size, self.device_pixel_ratio);
        }
    }

    pub fn drain_logs(&mut self) -> js_sys::Array {
        let logs = self.session.engine_mut().host_mut().drain_logs();
        let result = js_sys::Array::new();
        for log in logs {
            result.push(&wasm_bindgen::JsValue::from_str(&log));
        }
        result
    }

    /// Hands browser-persistent storage writes to the JavaScript shell. The
    /// engine remains synchronous; the shell decides whether to use
    /// localStorage, IndexedDB, or another origin-scoped adapter.
    pub fn drain_storage_writes(&mut self) -> js_sys::Array {
        let writes = self.session.engine().host().drain_memory_storage_writes();
        let result = js_sys::Array::new();
        for (path, bytes) in writes {
            let item = js_sys::Object::new();
            let _ = set_str(&item, "path", &path);
            let data = js_sys::Uint8Array::from(bytes.as_slice());
            let _ = js_sys::Reflect::set(&item, &wasm_bindgen::JsValue::from_str("bytes"), &data);
            result.push(&item);
        }
        result
    }

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        self.pending_events
            .push(krkr_core::EngineEvent::CursorMoved {
                position: krkr_core::Point::new(x, y),
            });
    }

    pub fn pointer_down(&mut self, x: f32, y: f32) {
        self.pointer_move(x, y);
        self.pending_events
            .push(krkr_core::EngineEvent::PointerInput {
                button: krkr_core::PointerButton::Primary,
                state: krkr_core::ButtonState::Pressed,
            });
    }

    pub fn pointer_up(&mut self, x: f32, y: f32) {
        self.pointer_move(x, y);
        self.pending_events
            .push(krkr_core::EngineEvent::PointerInput {
                button: krkr_core::PointerButton::Primary,
                state: krkr_core::ButtonState::Released,
            });
    }

    pub fn touch_event(&mut self, id: u64, x: f32, y: f32, phase: String) {
        let phase = match phase.as_str() {
            "start" | "started" => krkr_core::TouchPhase::Started,
            "move" | "moved" => krkr_core::TouchPhase::Moved,
            "end" | "ended" => krkr_core::TouchPhase::Ended,
            _ => krkr_core::TouchPhase::Cancelled,
        };
        self.pending_events
            .push(krkr_core::EngineEvent::TouchInput {
                id,
                position: krkr_core::Point::new(x, y),
                phase,
            });
    }

    pub fn text_input(&mut self, text: String, composing: bool) {
        self.pending_text
            .push(krkr_core::TextInputEvent { text, composing });
    }

    pub fn lifecycle(&mut self, state: String) -> Result<(), wasm_bindgen::JsValue> {
        let state = match state.as_str() {
            "foreground" => krkr_core::LifecycleState::Foreground,
            "background" => krkr_core::LifecycleState::Background,
            "surface-suspended" => krkr_core::LifecycleState::SurfaceSuspended,
            "surface-resumed" => krkr_core::LifecycleState::SurfaceResumed,
            "memory-pressure" => krkr_core::LifecycleState::MemoryPressure,
            "audio-interrupted" => krkr_core::LifecycleState::AudioInterrupted,
            "audio-resumed" => krkr_core::LifecycleState::AudioResumed,
            _ => {
                return Err(wasm_bindgen::JsValue::from_str("unknown lifecycle state"));
            }
        };
        self.pending_events
            .push(krkr_core::EngineEvent::Lifecycle { state });
        Ok(())
    }

    pub fn key_down(&mut self, key: String) {
        self.pending_events
            .push(krkr_core::EngineEvent::KeyboardInput {
                key: web_key(&key),
                state: krkr_core::ButtonState::Pressed,
                repeat: false,
            });
    }

    pub fn key_up(&mut self, key: String) {
        self.pending_events
            .push(krkr_core::EngineEvent::KeyboardInput {
                key: web_key(&key),
                state: krkr_core::ButtonState::Released,
                repeat: false,
            });
    }

    pub fn execute_script(&mut self, source: String) -> Result<(), wasm_bindgen::JsValue> {
        self.session
            .engine_mut()
            .execute_script("web-host", &source)
            .map(|_| ())
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
    }

    /// Flushes the game's system variables through the same shutdown hook used
    /// by the desktop host.  The browser shell calls this from `pagehide` so
    /// `sf.notFirst`, language selection and audio/window settings survive a
    /// refresh without putting browser storage concerns into the engine.
    pub fn persist_runtime_state(&mut self) -> Result<(), wasm_bindgen::JsValue> {
        self.session
            .engine_mut()
            .persist_runtime_state()
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
    }

    /// Advances one engine frame and returns a compact JS diagnostic/render
    /// model. A renderer adapter can consume the same draw list directly once
    /// the WebGPU surface is installed.
    pub fn tick(&mut self, timestamp_ms: f64) -> Result<js_sys::Object, wasm_bindgen::JsValue> {
        let delta_ms = self
            .last_timestamp_ms
            .map(|last| (timestamp_ms - last).clamp(0.0, 250.0))
            .unwrap_or(16.6667);
        self.last_timestamp_ms = Some(timestamp_ms);
        let frame = match self.session.update(
            EngineInput::new(
                krkr_core::FrameInput::new(
                    krkr_core::Size::new(self.viewport_width, self.viewport_height),
                    (delta_ms / 1000.0) as f32,
                )
                .with_safe_area(self.safe_area)
                .with_orientation(self.orientation),
                std::mem::take(&mut self.pending_events),
            )
            .with_text(std::mem::take(&mut self.pending_text)),
            Duration::from_secs_f64(delta_ms / 1000.0),
        ) {
            Ok(frame) => frame,
            Err(error) => {
                web_log(format!(
                    "runtime update failed (pendingExternal={}, pendingLoads={}, pendingAssets={}): {error}",
                    self.session
                        .engine()
                        .host()
                        .has_pending_external_resources(),
                    self.session.engine().host().has_pending_resource_loads(),
                    self.session.pending_asset_count(),
                ));
                return Err(wasm_bindgen::JsValue::from_str(&error.to_string()));
            }
        };
        if let Some(storage) = self.pending_scenario.clone()
            && !self.session.engine().is_script_suspended()
            && !self
                .session
                .engine()
                .host()
                .has_pending_external_resources()
            && !self.session.engine().host().has_pending_resource_loads()
            && !self
                .session
                .pending_asset_paths()
                .any(|path| path.eq_ignore_ascii_case(&storage))
        {
            match self.session.engine_mut().load_kag_scenario(&storage) {
                Ok(()) => {
                    self.pending_scenario = None;
                    self.last_timestamp_ms = None;
                    web_log(format!("scenario ready after fetch: {storage}"));
                }
                Err(error) if error.to_string().contains("resource is pending") => {
                    web_log(format!("scenario still waiting: {storage}"));
                }
                Err(error) => {
                    return Err(wasm_bindgen::JsValue::from_str(&error.to_string()));
                }
            }
        }
        let output = &frame.engine.output;
        // FrameInput is the authoritative logical canvas for the Web shell.
        // Script-created Window sizes describe application state, not a DOM
        // surface; using them here would letterbox layers against a stale
        // 960x600 default.
        let content_size = krkr_core::Size::new(self.viewport_width, self.viewport_height);
        let tree_draw_count = self
            .session
            .engine()
            .host()
            .layer_tree()
            .draw_model()
            .0
            .len();
        if let Some(renderer) = &mut self.renderer {
            renderer.set_content_size(Some(content_size));
            renderer
                .render(output)
                .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
        }
        let audio_commands = self.session.take_audio_commands();
        let logs = self.session.engine_mut().host_mut().drain_logs();
        let model = js_sys::Object::new();
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("drawCommands"),
            &wasm_bindgen::JsValue::from_f64(output.draw_commands.len() as f64),
        )
        .map_err(|error| error)?;
        set_num(&model, "treeDrawCommands", tree_draw_count as f64)?;
        set_num(&model, "contentWidth", content_size.width as f64)?;
        set_num(&model, "contentHeight", content_size.height as f64)?;
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("imageUploads"),
            &wasm_bindgen::JsValue::from_f64(output.image_uploads.len() as f64),
        )
        .map_err(|error| error)?;
        let releases = js_sys::Array::new();
        for texture_id in &output.image_releases {
            releases.push(&wasm_bindgen::JsValue::from_f64(*texture_id as f64));
        }
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("imageReleases"),
            &releases,
        )?;
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("assets"),
            &wasm_bindgen::JsValue::from_f64(frame.assets.len() as f64),
        )
        .map_err(|error| error)?;
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("audioEvents"),
            &wasm_bindgen::JsValue::from_f64(frame.audio.len() as f64),
        )
        .map_err(|error| error)?;
        let save_events = js_sys::Array::new();
        for event in &frame.saves {
            let item = js_sys::Object::new();
            match event {
                SaveEvent::Loaded {
                    id,
                    profile,
                    key,
                    data,
                } => {
                    set_str(&item, "kind", "loaded")?;
                    set_num(&item, "id", id.0 as f64)?;
                    set_str(&item, "profile", profile)?;
                    set_str(&item, "key", key)?;
                    if let Some(data) = data {
                        let bytes = js_sys::Uint8Array::from(data.as_ref());
                        js_sys::Reflect::set(
                            &item,
                            &wasm_bindgen::JsValue::from_str("data"),
                            &bytes,
                        )?;
                    }
                }
                SaveEvent::Saved { id, profile, key } => {
                    set_str(&item, "kind", "saved")?;
                    set_num(&item, "id", id.0 as f64)?;
                    set_str(&item, "profile", profile)?;
                    set_str(&item, "key", key)?;
                }
                SaveEvent::Failed {
                    id,
                    profile,
                    key,
                    message,
                } => {
                    set_str(&item, "kind", "failed")?;
                    set_num(&item, "id", id.0 as f64)?;
                    set_str(&item, "profile", profile)?;
                    set_str(&item, "key", key)?;
                    set_str(&item, "message", message)?;
                }
            }
            save_events.push(&item);
        }
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("saveEvents"),
            &save_events,
        )?;
        set_color(&model, output.clear_color)?;
        if let Some(storage) = &frame.engine.location.storage {
            set_str(&model, "locationStorage", storage)?;
        }
        if let Some(label) = &frame.engine.location.label {
            set_str(&model, "locationLabel", label)?;
        }
        set_num(&model, "locationPage", frame.engine.location.page as f64)?;
        set_str(
            &model,
            "kagState",
            &format!("{:?}", frame.engine.tick.state),
        )?;
        if let Some(transition) = &output.transition {
            set_str(&model, "transitionMethod", &transition.method)?;
            set_num(&model, "transitionProgress", transition.progress as f64)?;
            set_num(
                &model,
                "transitionFrozenDraws",
                transition.frozen_draw_commands.len() as f64,
            )?;
            let transition_model = js_sys::Object::new();
            set_str(&transition_model, "method", &transition.method)?;
            set_num(&transition_model, "progress", transition.progress as f64)?;
            let frozen_draws = draw_commands_to_js(&transition.frozen_draw_commands)?;
            js_sys::Reflect::set(
                &transition_model,
                &wasm_bindgen::JsValue::from_str("frozenDrawList"),
                &frozen_draws,
            )?;
            let frozen_uploads = image_uploads_to_js(&transition.frozen_image_uploads)?;
            js_sys::Reflect::set(
                &transition_model,
                &wasm_bindgen::JsValue::from_str("frozenUploads"),
                &frozen_uploads,
            )?;
            if let Some(upload) = &transition.rule_image_upload {
                let rule_uploads = image_uploads_to_js(std::slice::from_ref(upload))?;
                js_sys::Reflect::set(
                    &transition_model,
                    &wasm_bindgen::JsValue::from_str("ruleUploads"),
                    &rule_uploads,
                )?;
            }
            js_sys::Reflect::set(
                &model,
                &wasm_bindgen::JsValue::from_str("transition"),
                &transition_model,
            )?;
        }
        set_num(
            &model,
            "scriptSuspended",
            f64::from(self.session.engine().is_script_suspended() as u8),
        )?;
        set_num(
            &model,
            "pendingAssets",
            self.session.pending_asset_count() as f64,
        )?;
        if let Some(cache) = &self.package_cache {
            let cache = cache.borrow();
            set_num(&model, "cacheBytes", cache.bytes() as f64)?;
            set_num(&model, "cacheEntries", cache.len() as f64)?;
        }
        set_num(
            &model,
            "pendingResourceLoads",
            f64::from(self.session.engine().host().has_pending_resource_loads() as u8),
        )?;
        set_num(
            &model,
            "pendingExternalResources",
            f64::from(
                self.session
                    .engine()
                    .host()
                    .has_pending_external_resources() as u8,
            ),
        )?;
        set_str(
            &model,
            "lifecycle",
            &format!("{:?}", self.session.engine().host().lifecycle_state()),
        )?;
        let (continuous_handlers, script_events, idle_async_triggers, timers, window_updates) =
            self.session.engine().scheduler_diagnostics();
        set_num(&model, "continuousHandlers", continuous_handlers as f64)?;
        set_num(&model, "scriptEvents", script_events as f64)?;
        set_num(&model, "idleAsyncTriggers", idle_async_triggers as f64)?;
        set_num(&model, "timers", timers as f64)?;
        set_num(&model, "windowUpdates", window_updates as f64)?;
        let (timer_total, timer_enabled, timer_scheduled, timer_due, timer_now) =
            self.session.engine_mut().scheduler_timer_diagnostics();
        set_num(&model, "timerTotal", timer_total as f64)?;
        set_num(&model, "timerEnabled", timer_enabled as f64)?;
        set_num(&model, "timerScheduled", timer_scheduled as f64)?;
        set_num(&model, "timerDue", timer_due as f64)?;
        set_num(&model, "timerNow", timer_now as f64)?;
        if let Some((visible, closed, modal, primary_layer)) =
            self.session.engine().host().modal_window_snapshot()
        {
            set_num(&model, "modalVisible", f64::from(visible as u8))?;
            set_num(&model, "modalClosed", f64::from(closed as u8))?;
            set_num(&model, "modalActive", f64::from(modal as u8))?;
            if let Some(primary_layer) = primary_layer {
                set_num(&model, "modalPrimaryLayer", primary_layer.0 as f64)?;
            }
        }
        let layer_images = self
            .session
            .engine()
            .host()
            .layer_tree()
            .layers()
            .filter(|layer| layer.image.is_some())
            .count();
        set_num(&model, "layerImages", layer_images as f64)?;
        let layer_list = js_sys::Array::new();
        for layer in self.session.engine().host().layer_tree().layers() {
            if layer.image.is_none() {
                continue;
            }
            let item = js_sys::Object::new();
            set_str(&item, "name", &layer.name)?;
            set_num(&item, "id", layer.id as f64)?;
            if let Some(parent) = layer.parent {
                set_num(&item, "parent", parent as f64)?;
            }
            if let Some(storage) = self.session.engine().host().layer_image_storage(layer.id) {
                set_str(&item, "storage", storage)?;
            }
            set_num(&item, "visible", f64::from(layer.visible as u8))?;
            set_num(&item, "renderable", f64::from(layer.renderable as u8))?;
            set_num(&item, "zOrder", layer.z_order as f64)?;
            set_num(&item, "width", layer.width as f64)?;
            set_num(&item, "height", layer.height as f64)?;
            set_num(&item, "left", layer.left as f64)?;
            set_num(&item, "top", layer.top as f64)?;
            if let Some(image) = &layer.image {
                set_num(&item, "textureId", image.upload.texture_id as f64)?;
                set_num(&item, "imageWidth", image.upload.width as f64)?;
                set_num(&item, "imageHeight", image.upload.height as f64)?;
                set_num(&item, "imageLeft", layer.image_left as f64)?;
                set_num(&item, "imageTop", layer.image_top as f64)?;
                set_num(&item, "opacity", f64::from(layer.opacity) / 255.0)?;
            }
            layer_list.push(&item);
        }
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("imageLayers"),
            &layer_list,
        )?;
        let pending_paths = js_sys::Array::new();
        for path in self.session.pending_asset_paths() {
            pending_paths.push(&wasm_bindgen::JsValue::from_str(path));
        }
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("pendingAssetPaths"),
            &pending_paths,
        )?;
        let log_list = js_sys::Array::new();
        for log in logs {
            log_list.push(&wasm_bindgen::JsValue::from_str(&log));
        }
        js_sys::Reflect::set(&model, &wasm_bindgen::JsValue::from_str("logs"), &log_list)?;
        let draw_list = draw_commands_to_js(&output.draw_commands)?;
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("drawList"),
            &draw_list,
        )?;
        let uploads = image_uploads_to_js(&output.image_uploads)?;
        js_sys::Reflect::set(
            &model,
            &wasm_bindgen::JsValue::from_str("uploads"),
            &uploads,
        )?;
        let audio = js_sys::Array::new();
        for command in audio_commands {
            let item = js_sys::Object::new();
            let kind = match &command {
                AudioCommand::Play { .. } => "play",
                AudioCommand::PlayPcmStream { .. } => "play-pcm",
                AudioCommand::Preload { .. } => "preload",
                AudioCommand::Stop { .. } => "stop",
                AudioCommand::SetVolume { .. } => "set-volume",
                AudioCommand::Pause { .. } => "pause",
                AudioCommand::Resume { .. } => "resume",
                AudioCommand::StopBus { .. } => "stop-bus",
                AudioCommand::SetBusVolume { .. } => "set-bus-volume",
            };
            set_str(&item, "kind", kind)?;
            match command {
                AudioCommand::Play {
                    id,
                    bus,
                    source,
                    looping,
                    volume,
                    ..
                } => {
                    set_num(&item, "id", id.0 as f64)?;
                    set_str(&item, "bus", audio_bus_name(bus))?;
                    set_str(&item, "source", &source.storage)?;
                    set_num(&item, "looping", f64::from(looping as u8))?;
                    set_num(&item, "volume", volume as f64)?;
                }
                AudioCommand::PlayPcmStream {
                    id, bus, volume, ..
                } => {
                    set_num(&item, "id", id.0 as f64)?;
                    set_str(&item, "bus", audio_bus_name(bus))?;
                    set_num(&item, "volume", volume as f64)?;
                }
                AudioCommand::Preload { source, .. } => {
                    set_str(&item, "source", &source.storage)?;
                }
                AudioCommand::Stop { id, fade_seconds }
                | AudioCommand::Pause { id, fade_seconds }
                | AudioCommand::Resume { id, fade_seconds } => {
                    set_num(&item, "id", id.0 as f64)?;
                    set_num(&item, "fadeSeconds", fade_seconds as f64)?;
                }
                AudioCommand::SetVolume {
                    id,
                    volume,
                    fade_seconds,
                } => {
                    set_num(&item, "id", id.0 as f64)?;
                    set_num(&item, "volume", volume as f64)?;
                    set_num(&item, "fadeSeconds", fade_seconds as f64)?;
                }
                AudioCommand::StopBus { bus, fade_seconds } => {
                    set_str(&item, "bus", audio_bus_name(bus))?;
                    set_num(&item, "fadeSeconds", fade_seconds as f64)?;
                }
                AudioCommand::SetBusVolume {
                    bus,
                    volume,
                    fade_seconds,
                } => {
                    set_str(&item, "bus", audio_bus_name(bus))?;
                    set_num(&item, "volume", volume as f64)?;
                    set_num(&item, "fadeSeconds", fade_seconds as f64)?;
                }
            }
            audio.push(&item);
        }
        js_sys::Reflect::set(&model, &wasm_bindgen::JsValue::from_str("audio"), &audio)?;
        let videos = js_sys::Array::new();
        for video in self.session.engine_mut().video_overlay_snapshots() {
            let item = js_sys::Object::new();
            set_str(&item, "kind", "video")?;
            // Preserve exact object identity even after a long-running
            // session crosses JavaScript Number's safe-integer range.
            set_str(&item, "id", &video.id.to_string())?;
            if let Some(storage) = video.storage {
                set_str(&item, "storage", &storage)?;
            }
            set_str(&item, "status", &video.status)?;
            set_num(&item, "left", video.left as f64)?;
            set_num(&item, "top", video.top as f64)?;
            set_num(&item, "width", video.width as f64)?;
            set_num(&item, "height", video.height as f64)?;
            set_num(&item, "visible", f64::from(video.visible as u8))?;
            set_num(&item, "looping", f64::from(video.looping as u8))?;
            set_num(&item, "position", video.position_ms as f64)?;
            set_num(&item, "playRate", video.play_rate)?;
            set_num(&item, "audioVolume", video.audio_volume as f64)?;
            set_num(&item, "audioBalance", video.audio_balance as f64)?;
            videos.push(&item);
        }
        js_sys::Reflect::set(&model, &wasm_bindgen::JsValue::from_str("videos"), &videos)?;
        if let Some(base) = &self.package_base_url {
            set_str(&model, "packageBase", base)?;
        }
        Ok(model)
    }
}

#[cfg(target_arch = "wasm32")]
fn draw_commands_to_js(
    commands: &[krkr_core::DrawCommand],
) -> Result<js_sys::Array, wasm_bindgen::JsValue> {
    let draw_list = js_sys::Array::new();
    for command in commands {
        let item = js_sys::Object::new();
        match command {
            krkr_core::DrawCommand::Rect(rect) => {
                set_str(&item, "kind", "rect")?;
                set_rect(&item, rect.rect)?;
                set_color(&item, rect.color)?;
            }
            krkr_core::DrawCommand::Text(text) => {
                set_str(&item, "kind", "text")?;
                set_num(&item, "x", text.position.x as f64)?;
                set_num(&item, "y", text.position.y as f64)?;
                set_num(&item, "size", text.size as f64)?;
                set_str(&item, "text", &text.text)?;
                set_color(&item, text.color)?;
                set_str(&item, "fontFace", &text.font.face)?;
                set_num(&item, "bold", f64::from(text.font.bold as u8))?;
                set_num(&item, "italic", f64::from(text.font.italic as u8))?;
                set_num(&item, "underline", f64::from(text.font.underline as u8))?;
                set_num(&item, "antiAlias", f64::from(text.style.anti_alias as u8))?;
                if let Some(shadow) = text.style.shadow {
                    set_num(&item, "shadowX", shadow.offset_x as f64)?;
                    set_num(&item, "shadowY", shadow.offset_y as f64)?;
                    set_color_prefixed(&item, "shadow", shadow.color)?;
                }
            }
            krkr_core::DrawCommand::Image(image) => {
                set_str(&item, "kind", "image")?;
                set_num(&item, "textureId", image.texture_id as f64)?;
                set_rect(&item, image.rect)?;
                // Preserve atlas sampling coordinates for browser renderers.
                set_rect_prefixed(&item, "source", image.source_rect)?;
                set_rect_prefixed(
                    &item,
                    "texture",
                    krkr_core::Rect::new(
                        0.0,
                        0.0,
                        image.texture_size.width,
                        image.texture_size.height,
                    ),
                )?;
                set_num(&item, "opacity", image.opacity as f64)?;
            }
        }
        draw_list.push(&item);
    }
    Ok(draw_list)
}

#[cfg(target_arch = "wasm32")]
fn image_uploads_to_js(
    uploads: &[krkr_core::ImageUpload],
) -> Result<js_sys::Array, wasm_bindgen::JsValue> {
    let result = js_sys::Array::new();
    for upload in uploads {
        let item = js_sys::Object::new();
        set_num(&item, "textureId", upload.texture_id as f64)?;
        set_num(&item, "width", upload.width as f64)?;
        set_num(&item, "height", upload.height as f64)?;
        let bytes = js_sys::Uint8Array::from(upload.rgba.as_ref());
        js_sys::Reflect::set(&item, &wasm_bindgen::JsValue::from_str("rgba"), &bytes)?;
        result.push(&item);
    }
    Ok(result)
}

#[cfg(target_arch = "wasm32")]
fn set_str(object: &js_sys::Object, key: &str, value: &str) -> Result<(), wasm_bindgen::JsValue> {
    js_sys::Reflect::set(
        object,
        &wasm_bindgen::JsValue::from_str(key),
        &wasm_bindgen::JsValue::from_str(value),
    )
    .map(|_| ())
}

#[cfg(target_arch = "wasm32")]
fn set_num(object: &js_sys::Object, key: &str, value: f64) -> Result<(), wasm_bindgen::JsValue> {
    js_sys::Reflect::set(
        object,
        &wasm_bindgen::JsValue::from_str(key),
        &wasm_bindgen::JsValue::from_f64(value),
    )
    .map(|_| ())
}

#[cfg(target_arch = "wasm32")]
fn set_rect(object: &js_sys::Object, rect: krkr_core::Rect) -> Result<(), wasm_bindgen::JsValue> {
    set_num(object, "x", rect.x as f64)?;
    set_num(object, "y", rect.y as f64)?;
    set_num(object, "width", rect.width as f64)?;
    set_num(object, "height", rect.height as f64)
}

#[cfg(target_arch = "wasm32")]
fn set_rect_prefixed(
    object: &js_sys::Object,
    prefix: &str,
    rect: krkr_core::Rect,
) -> Result<(), wasm_bindgen::JsValue> {
    set_num(object, &format!("{prefix}X"), rect.x as f64)?;
    set_num(object, &format!("{prefix}Y"), rect.y as f64)?;
    set_num(object, &format!("{prefix}Width"), rect.width as f64)?;
    set_num(object, &format!("{prefix}Height"), rect.height as f64)
}

#[cfg(target_arch = "wasm32")]
fn audio_bus_name(bus: krkr_core::AudioBus) -> &'static str {
    match bus {
        krkr_core::AudioBus::Master => "master",
        krkr_core::AudioBus::Bgm => "bgm",
        krkr_core::AudioBus::SoundEffect => "sound-effect",
    }
}

#[cfg(target_arch = "wasm32")]
fn set_color(
    object: &js_sys::Object,
    color: krkr_core::Color,
) -> Result<(), wasm_bindgen::JsValue> {
    set_num(object, "r", color.r as f64)?;
    set_num(object, "g", color.g as f64)?;
    set_num(object, "b", color.b as f64)?;
    set_num(object, "a", color.a as f64)
}

#[cfg(target_arch = "wasm32")]
fn set_color_prefixed(
    object: &js_sys::Object,
    prefix: &str,
    color: [u8; 4],
) -> Result<(), wasm_bindgen::JsValue> {
    set_num(object, &format!("{prefix}R"), f64::from(color[0]) / 255.0)?;
    set_num(object, &format!("{prefix}G"), f64::from(color[1]) / 255.0)?;
    set_num(object, &format!("{prefix}B"), f64::from(color[2]) / 255.0)?;
    set_num(object, &format!("{prefix}A"), f64::from(color[3]) / 255.0)
}

#[cfg(target_arch = "wasm32")]
fn web_key(key: &str) -> krkr_core::EngineKey {
    use krkr_core::EngineKey;
    match key {
        "Escape" => EngineKey::Escape,
        "Enter" => EngineKey::Enter,
        " " | "Spacebar" => EngineKey::Space,
        "Tab" => EngineKey::Tab,
        "ArrowLeft" => EngineKey::Left,
        "ArrowUp" => EngineKey::Up,
        "ArrowRight" => EngineKey::Right,
        "ArrowDown" => EngineKey::Down,
        "PageUp" => EngineKey::PageUp,
        "PageDown" => EngineKey::PageDown,
        "Backspace" => EngineKey::Backspace,
        "Delete" => EngineKey::Delete,
        "Shift" => EngineKey::Shift,
        "Control" => EngineKey::Control,
        "Alt" => EngineKey::Alt,
        value => value
            .chars()
            .next()
            .map(EngineKey::Character)
            .unwrap_or(EngineKey::Other),
    }
}

#[cfg(target_arch = "wasm32")]
use krkr_assets::{WebManifest, WebManifestEntry};

#[cfg(target_arch = "wasm32")]
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    web_log(format!("fetch started: {url}"));
    let window = web_sys::window().ok_or_else(|| "window unavailable".to_string())?;
    let response_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|value| {
            let message = format!("fetch failed: {value:?}");
            web_warn(format!("{message} ({url})"));
            message
        })?;
    let response = response_value
        .dyn_into::<web_sys::Response>()
        .map_err(|_| "fetch returned a non-response value".to_string())?;
    if !response.ok() {
        let message = format!("HTTP {} while fetching {url}", response.status());
        web_warn(&message);
        return Err(message);
    }
    let buffer = wasm_bindgen_futures::JsFuture::from(
        response
            .array_buffer()
            .map_err(|value| format!("array_buffer failed: {value:?}"))?,
    )
    .await
    .map_err(|value| {
        let message = format!("reading {url} failed: {value:?}");
        web_warn(&message);
        message
    })?;
    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
    web_log(format!("fetch complete: {url} ({} bytes)", bytes.len()));
    Ok(bytes)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_web_manifest_entry(
    base_url: &str,
    entry: &WebManifestEntry,
) -> Result<Vec<u8>, String> {
    let normalized = entry.path.replace('\\', "/");
    if normalized.is_empty() || normalized.split('/').any(|part| part == "..") {
        return Err(format!("invalid Web asset path: {}", entry.path));
    }
    let bytes = fetch_bytes(&package_url(base_url, &entry.path)).await?;
    if bytes.len() as u64 != entry.size {
        return Err(format!(
            "size mismatch for {}: expected {}, got {}",
            entry.path,
            entry.size,
            bytes.len()
        ));
    }
    Ok(bytes)
}

#[cfg(target_arch = "wasm32")]
fn package_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(target_arch = "wasm32")]
fn normalize_web_path(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Semantic-path v1 resource scheduler. Requests are issued only when the
/// engine reports a cache miss; duplicate requests for the same logical path
/// share one in-flight fetch. Completed bytes are retained under a bounded
/// LRU budget so long-running or mobile sessions cannot grow without limit.
#[cfg(target_arch = "wasm32")]
pub struct WebResourceStore {
    base_url: String,
    manifest: WebManifest,
    next_id: u64,
    in_flight: Rc<RefCell<BTreeMap<String, Vec<AssetWaiter>>>>,
    cache: Rc<RefCell<ByteCache>>,
    events: Rc<RefCell<Vec<AssetEvent>>>,
    revision: u64,
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug)]
struct AssetWaiter {
    id: AssetRequestId,
    path: String,
    kind: AssetKind,
}

#[cfg(target_arch = "wasm32")]
const WEB_CACHE_CAPACITY_BYTES: usize = 128 * 1024 * 1024;

#[cfg(target_arch = "wasm32")]
const WEB_CACHE_MAX_ENTRY_BYTES: usize = 32 * 1024 * 1024;

#[cfg(target_arch = "wasm32")]
struct ByteCache {
    entries: BTreeMap<String, std::sync::Arc<[u8]>>,
    order: std::collections::VecDeque<String>,
    bytes: usize,
}

#[cfg(target_arch = "wasm32")]
impl ByteCache {
    fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            order: std::collections::VecDeque::new(),
            bytes: 0,
        }
    }

    fn get(&mut self, key: &str) -> Option<std::sync::Arc<[u8]>> {
        let value = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(value)
    }

    fn get_case_insensitive(&mut self, key: &str) -> Option<std::sync::Arc<[u8]>> {
        let matched = self
            .entries
            .keys()
            .find(|candidate| candidate.eq_ignore_ascii_case(key))
            .cloned()?;
        self.get(&matched)
    }

    fn insert(&mut self, key: String, value: std::sync::Arc<[u8]>) {
        if value.len() > WEB_CACHE_MAX_ENTRY_BYTES {
            return;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.len());
            self.order.retain(|candidate| candidate != &key);
        }
        while self.bytes.saturating_add(value.len()) > WEB_CACHE_CAPACITY_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(previous) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(previous.len());
            }
        }
        self.bytes = self.bytes.saturating_add(value.len());
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
    }

    fn touch(&mut self, key: &str) {
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(key.to_string());
    }

    fn bytes(&self) -> usize {
        self.bytes
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(target_arch = "wasm32")]
impl WebResourceStore {
    pub fn new(base_url: String, manifest: WebManifest) -> Self {
        Self {
            base_url,
            manifest,
            next_id: 1,
            in_flight: Rc::new(RefCell::new(BTreeMap::new())),
            cache: Rc::new(RefCell::new(ByteCache::new())),
            events: Rc::new(RefCell::new(Vec::new())),
            revision: 1,
        }
    }

    fn fetch_entry(
        base_url: String,
        entry: WebManifestEntry,
        cache_key: String,
        in_flight: Rc<RefCell<BTreeMap<String, Vec<AssetWaiter>>>>,
        cache: Rc<RefCell<ByteCache>>,
        events: Rc<RefCell<Vec<AssetEvent>>>,
    ) {
        wasm_bindgen_futures::spawn_local(async move {
            web_log(format!("asset fetch started: {}", entry.path));
            let result = fetch_web_manifest_entry(&base_url, &entry).await;
            let result = match result {
                Ok(data) => {
                    let data: std::sync::Arc<[u8]> = std::sync::Arc::from(data);
                    cache.borrow_mut().insert(cache_key.clone(), data.clone());
                    Ok(data)
                }
                Err(message) => Err(message),
            };
            let waiters = in_flight
                .borrow_mut()
                .remove(&cache_key)
                .unwrap_or_default();
            let mut pending_events = events.borrow_mut();
            for waiter in waiters {
                match &result {
                    Ok(data) => {
                        web_log(format!(
                            "asset ready: {} ({} bytes, request {})",
                            waiter.path,
                            data.len(),
                            waiter.id.0
                        ));
                        pending_events.push(AssetEvent::Ready {
                            id: waiter.id,
                            path: waiter.path,
                            kind: waiter.kind,
                            data: data.clone(),
                        });
                    }
                    Err(message) => {
                        web_warn(format!("asset failed: {}: {message}", waiter.path));
                        pending_events.push(AssetEvent::Failed {
                            id: waiter.id,
                            path: waiter.path,
                            kind: waiter.kind,
                            message: message.clone(),
                        });
                    }
                }
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
impl AssetScheduler for WebResourceStore {
    fn request(&mut self, path: &str, kind: AssetKind) -> AssetRequestId {
        let id = AssetRequestId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let normalized = normalize_web_path(path);
        if normalized.split('/').any(|part| part == "..") {
            self.events.borrow_mut().push(AssetEvent::Failed {
                id,
                path: path.to_string(),
                kind,
                message: "asset path escapes package root".to_string(),
            });
            return id;
        }
        let manifest_kind = match kind {
            AssetKind::Image => "image",
            AssetKind::Text => "script",
            AssetKind::Font | AssetKind::Media | AssetKind::Binary => "binary",
        };
        let kind_entry = self.manifest.entry_for_kind(&normalized, manifest_kind);
        let Some(entry) = kind_entry
            .or_else(|| self.manifest.entry(&normalized))
            .cloned()
        else {
            web_warn(format!("asset missing from Web manifest: {normalized}"));
            self.events.borrow_mut().push(AssetEvent::Failed {
                id,
                path: path.to_string(),
                kind,
                message: "asset is not present in Web manifest v1".to_string(),
            });
            return id;
        };
        // Different logical aliases can point at one physical URL. Coalesce
        // by that canonical path so `foo` and `uipsd/foo.tlg` never trigger
        // duplicate network requests or duplicate decode work.
        let cache_key = normalize_web_path(&entry.path);
        if let Some(data) = self.cache.borrow_mut().get(&cache_key) {
            web_log(format!("asset cache hit: {normalized} (request {})", id.0));
            self.events.borrow_mut().push(AssetEvent::Ready {
                id,
                path: path.to_string(),
                kind,
                data,
            });
            return id;
        }
        let waiter = AssetWaiter {
            id,
            path: path.to_string(),
            kind,
        };
        if let Some(waiters) = self.in_flight.borrow_mut().get_mut(&cache_key) {
            web_log(format!(
                "asset request joined in-flight fetch: {normalized} (request {})",
                id.0
            ));
            waiters.push(waiter);
            return id;
        }
        self.in_flight
            .borrow_mut()
            .insert(cache_key.clone(), vec![waiter]);
        web_log(format!(
            "asset fetch queued: {normalized} -> {} (request {})",
            entry.path, id.0
        ));
        Self::fetch_entry(
            self.base_url.clone(),
            entry,
            cache_key,
            Rc::clone(&self.in_flight),
            Rc::clone(&self.cache),
            Rc::clone(&self.events),
        );
        id
    }

    fn poll(&mut self) -> Vec<AssetEvent> {
        let events = std::mem::take(&mut *self.events.borrow_mut());
        if !events.is_empty() {
            self.revision = self.revision.saturating_add(1);
        }
        events
    }

    fn cancel(&mut self, id: AssetRequestId) -> bool {
        let mut cancelled = false;
        let mut empty_keys = Vec::new();
        for (key, waiters) in self.in_flight.borrow_mut().iter_mut() {
            let before = waiters.len();
            waiters.retain(|waiter| waiter.id != id);
            cancelled |= before != waiters.len();
            if waiters.is_empty() {
                empty_keys.push(key.clone());
            }
        }
        if !empty_keys.is_empty() {
            let mut in_flight = self.in_flight.borrow_mut();
            for key in empty_keys {
                in_flight.remove(&key);
            }
        }
        let mut events = self.events.borrow_mut();
        let before = events.len();
        events.retain(|event| match event {
            AssetEvent::Ready { id: event_id, .. } | AssetEvent::Failed { id: event_id, .. } => {
                *event_id != id
            }
        });
        cancelled || before != events.len()
    }

    fn revision(&self) -> u64 {
        self.revision
    }
}

/// Command bridge for a Web Audio implementation. JavaScript can consume the
/// queued commands through `take_commands` and report lifecycle events back to
/// the runtime with `push_event`; no native audio device is touched here.
#[derive(Clone, Debug, Default)]
pub struct WebAudioSink {
    commands: Vec<AudioCommand>,
    events: Vec<AudioEvent>,
}

impl WebAudioSink {
    pub fn take_commands(&mut self) -> Vec<AudioCommand> {
        std::mem::take(&mut self.commands)
    }

    pub fn push_event(&mut self, event: AudioEvent) {
        self.events.push(event);
    }
}

impl AudioSink for WebAudioSink {
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

    fn take_commands(&mut self) -> Vec<AudioCommand> {
        WebAudioSink::take_commands(self)
    }
}

#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

/// Fetch-backed implementation of the core asset protocol.  Requests are
/// issued immediately and become visible from `poll`, matching the same
/// contract as native stores without blocking the browser main thread.
#[cfg(target_arch = "wasm32")]
pub struct FetchAssetStore {
    next_id: u64,
    pending: BTreeMap<AssetRequestId, (String, AssetKind)>,
    events: Rc<RefCell<Vec<AssetEvent>>>,
    revision: u64,
}

#[cfg(target_arch = "wasm32")]
impl Default for FetchAssetStore {
    fn default() -> Self {
        Self {
            next_id: 1,
            pending: BTreeMap::new(),
            events: Rc::new(RefCell::new(Vec::new())),
            revision: 0,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl AssetScheduler for FetchAssetStore {
    fn request(&mut self, path: &str, kind: AssetKind) -> AssetRequestId {
        let id = AssetRequestId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let path = path.to_string();
        self.pending.insert(id, (path.clone(), kind));
        let events = Rc::clone(&self.events);
        let path_for_error = path.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = async {
                let window = web_sys::window().ok_or_else(|| "window unavailable".to_string())?;
                let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(&path))
                    .await
                    .map_err(|value| format!("fetch failed: {value:?}"))?
                    .dyn_into::<web_sys::Response>()
                    .map_err(|_| "fetch returned a non-response value".to_string())?;
                if !response.ok() {
                    return Err(format!("HTTP {} while fetching {path}", response.status()));
                }
                let buffer = wasm_bindgen_futures::JsFuture::from(
                    response
                        .array_buffer()
                        .map_err(|value| format!("array_buffer failed: {value:?}"))?,
                )
                .await
                .map_err(|value| format!("reading {path} failed: {value:?}"))?;
                let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                Ok(bytes)
            }
            .await;
            let event = match result {
                Ok(bytes) => AssetEvent::Ready {
                    id,
                    path,
                    kind,
                    data: std::sync::Arc::from(bytes),
                },
                Err(message) => AssetEvent::Failed {
                    id,
                    path: path_for_error,
                    kind,
                    message,
                },
            };
            events.borrow_mut().push(event);
        });
        id
    }

    fn poll(&mut self) -> Vec<AssetEvent> {
        let events = std::mem::take(&mut *self.events.borrow_mut());
        for event in &events {
            let id = match event {
                AssetEvent::Ready { id, .. } | AssetEvent::Failed { id, .. } => *id,
            };
            self.pending.remove(&id);
        }
        if !events.is_empty() {
            self.revision = self.revision.saturating_add(1);
        }
        events
    }

    fn revision(&self) -> u64 {
        self.revision
    }
}
