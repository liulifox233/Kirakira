use std::{
    collections::BTreeSet,
    collections::{HashMap, VecDeque},
    io::Cursor,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use krkr_core::{AssetKind, ProjectStoragePort, ProvinceImage, ResourceData};

#[cfg(test)]
use krkr_assets::ProjectStorage;

const DECODED_IMAGE_CACHE_CAPACITY_BYTES: usize = 128 * 1024 * 1024;
const DECODED_IMAGE_CACHE_MAX_ENTRY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceTaskId(pub u64);

#[derive(Clone)]
pub struct ResourceManager {
    task_tx: mpsc::Sender<ResourceTask>,
    completion_rx: Arc<Mutex<mpsc::Receiver<ResourceCompletion>>>,
    next_task_id: Arc<AtomicU64>,
    cancelled: Arc<Mutex<BTreeSet<ResourceTaskId>>>,
}

#[derive(Clone, Debug)]
pub struct DecodedImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

#[derive(Clone)]
pub struct ResourceCompletion {
    pub id: ResourceTaskId,
    pub revision: u64,
    pub storage: String,
    pub result: std::result::Result<DecodedImageData, String>,
}

enum ResourceTask {
    DecodeImage {
        id: ResourceTaskId,
        revision: u64,
        storage: String,
    },
    LoadBytesBlocking {
        storage: String,
        kind: AssetKind,
        reply_tx: mpsc::Sender<std::result::Result<ResourceData, String>>,
    },
    LoadTextBlocking {
        storage: String,
        encoding: String,
        reply_tx: mpsc::Sender<std::result::Result<String, String>>,
    },
    ClearDecodedImageCache {
        reply_tx: mpsc::Sender<()>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DecodedImageCacheKey {
    revision: u64,
    storage: String,
}

struct DecodedImageCacheEntry {
    image: DecodedImageData,
    bytes: usize,
}

struct DecodedImageCache {
    entries: HashMap<DecodedImageCacheKey, DecodedImageCacheEntry>,
    lru: VecDeque<DecodedImageCacheKey>,
    bytes: usize,
    capacity_bytes: usize,
    max_entry_bytes: usize,
}

impl ResourceManager {
    pub fn new(storage: Arc<dyn ProjectStoragePort>) -> std::io::Result<Self> {
        let (task_tx, task_rx) = mpsc::channel();
        let (completion_tx, completion_rx) = mpsc::channel();
        let cancelled = Arc::new(Mutex::new(BTreeSet::new()));
        let worker_cancelled = Arc::clone(&cancelled);
        thread::Builder::new()
            .name("krkr-resource-worker".to_string())
            .spawn(move || resource_worker(storage, task_rx, completion_tx, worker_cancelled))?;
        Ok(Self {
            task_tx,
            completion_rx: Arc::new(Mutex::new(completion_rx)),
            next_task_id: Arc::new(AtomicU64::new(1)),
            cancelled,
        })
    }

    /// Queues an image decode on the resource worker.
    ///
    /// `None` means the worker is gone (it panicked, or the process is
    /// shutting down). Callers must then decode on their own thread: parking a
    /// script call on a completion that can never arrive leaves
    /// `has_pending_resource_loads` set forever.
    pub fn request_image_decode(
        &self,
        storage: impl Into<String>,
        revision: u64,
    ) -> Option<ResourceTaskId> {
        let storage = storage.into();
        let id = self.next_id();
        self.task_tx
            .send(ResourceTask::DecodeImage {
                id,
                revision,
                storage,
            })
            .ok()
            .map(|()| id)
    }

    pub fn load_bytes_blocking(
        &self,
        storage: impl Into<String>,
    ) -> std::result::Result<ResourceData, String> {
        self.load_bytes_blocking_for_kind(storage, AssetKind::Binary)
    }

    /// Image-load counterpart of [`Self::load_bytes_blocking`]: the worker
    /// resolves the name through
    /// [`ProjectStoragePort::read_image_storage`], whose candidate list
    /// suggests only the extensions with a registered graphic handler, the way
    /// `TVPInternalLoadGraphic` does
    /// (`visual/GraphicsLoaderIntf.cpp:1478-1506`).
    pub fn load_image_bytes_blocking(
        &self,
        storage: impl Into<String>,
    ) -> std::result::Result<ResourceData, String> {
        self.load_bytes_blocking_for_kind(storage, AssetKind::Image)
    }

    fn load_bytes_blocking_for_kind(
        &self,
        storage: impl Into<String>,
        kind: AssetKind,
    ) -> std::result::Result<ResourceData, String> {
        let storage = storage.into();
        let (reply_tx, reply_rx) = mpsc::channel();
        self.task_tx
            .send(ResourceTask::LoadBytesBlocking {
                storage: storage.clone(),
                kind,
                reply_tx,
            })
            .map_err(|_| format!("resource worker is not available for `{storage}`"))?;
        reply_rx
            .recv()
            .map_err(|_| format!("resource worker stopped while loading `{storage}`"))?
    }

    pub fn load_text_blocking(
        &self,
        storage: impl Into<String>,
        encoding: impl Into<String>,
    ) -> std::result::Result<String, String> {
        let storage = storage.into();
        let (reply_tx, reply_rx) = mpsc::channel();
        self.task_tx
            .send(ResourceTask::LoadTextBlocking {
                storage: storage.clone(),
                encoding: encoding.into(),
                reply_tx,
            })
            .map_err(|_| format!("resource worker is not available for `{storage}`"))?;
        reply_rx
            .recv()
            .map_err(|_| format!("resource worker stopped while loading text `{storage}`"))?
    }

    pub fn clear_decoded_image_cache_blocking(&self) -> std::result::Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.task_tx
            .send(ResourceTask::ClearDecodedImageCache { reply_tx })
            .map_err(|_| "resource worker is not available for cache clear".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "resource worker stopped while clearing image cache".to_string())
    }

    pub fn drain_completions(&self) -> Vec<ResourceCompletion> {
        let mut completions = Vec::new();
        let Ok(rx) = self.completion_rx.lock() else {
            return completions;
        };
        while let Ok(completion) = rx.try_recv() {
            completions.push(completion);
        }
        completions
    }

    /// Blocks up to `timeout` for the next decode completion. Script image
    /// loads use this to finish a fast decode inside the calling tick, the way
    /// official `TVPLoadGraphic` loads synchronously; `None` means the worker
    /// is still busy and the caller must keep the asynchronous path.
    pub fn wait_completion(&self, timeout: Duration) -> Option<ResourceCompletion> {
        let rx = self.completion_rx.lock().ok()?;
        rx.recv_timeout(timeout).ok()
    }

    /// Marks an in-flight decode as no longer needed. The worker checks this
    /// before and after decoding, so stale image work is dropped without ever
    /// waking the VM after a storage revision or layer generation change.
    pub fn cancel(&self, id: ResourceTaskId) -> bool {
        self.cancelled
            .lock()
            .map(|mut cancelled| cancelled.insert(id))
            .unwrap_or(false)
    }

    fn next_id(&self) -> ResourceTaskId {
        ResourceTaskId(self.next_task_id.fetch_add(1, Ordering::Relaxed))
    }
}

fn resource_worker(
    storage: Arc<dyn ProjectStoragePort>,
    task_rx: mpsc::Receiver<ResourceTask>,
    completion_tx: mpsc::Sender<ResourceCompletion>,
    cancelled: Arc<Mutex<BTreeSet<ResourceTaskId>>>,
) {
    let mut image_cache = DecodedImageCache::new(
        DECODED_IMAGE_CACHE_CAPACITY_BYTES,
        DECODED_IMAGE_CACHE_MAX_ENTRY_BYTES,
    );

    while let Ok(task) = task_rx.recv() {
        let completion = match task {
            ResourceTask::DecodeImage {
                id,
                revision,
                storage: name,
            } => {
                if take_cancelled(&cancelled, id) {
                    continue;
                }
                let result = decode_image(storage.as_ref(), &mut image_cache, revision, &name);
                if take_cancelled(&cancelled, id) {
                    continue;
                }
                ResourceCompletion {
                    id,
                    revision,
                    storage: name.clone(),
                    result,
                }
            }
            ResourceTask::LoadBytesBlocking {
                storage: name,
                kind,
                reply_tx,
            } => {
                // `AssetKind::Image` resolves through the graphic loader's
                // candidate list; every other kind keeps the plain binary read.
                let result = match kind {
                    AssetKind::Image => storage.read_image_storage(&name),
                    _ => storage.read_binary_storage(&name),
                };
                let _ = reply_tx.send(result.map_err(|error| error.to_string()));
                continue;
            }
            ResourceTask::LoadTextBlocking {
                storage: name,
                encoding,
                reply_tx,
            } => {
                let _ = reply_tx.send(
                    storage
                        .read_text_storage(&name, &encoding)
                        .map_err(|error| error.to_string()),
                );
                continue;
            }
            ResourceTask::ClearDecodedImageCache { reply_tx } => {
                image_cache.clear();
                let _ = reply_tx.send(());
                continue;
            }
        };
        if completion_tx.send(completion).is_err() {
            break;
        }
    }
}

fn take_cancelled(cancelled: &Mutex<BTreeSet<ResourceTaskId>>, id: ResourceTaskId) -> bool {
    cancelled
        .lock()
        .map(|mut ids| ids.remove(&id))
        .unwrap_or(false)
}

fn decode_image(
    storage: &dyn ProjectStoragePort,
    cache: &mut DecodedImageCache,
    revision: u64,
    name: &str,
) -> std::result::Result<DecodedImageData, String> {
    let key = DecodedImageCacheKey {
        revision,
        storage: name.to_string(),
    };
    if let Some(image) = cache.get(&key) {
        return Ok(image);
    }

    // The decode worker is the image load path for every asynchronous caller
    // (`Layer.loadImages`, preloads), so it resolves the name with the graphic
    // loader's suggestions: `PageBreak` must reach `PageBreak.png`, never the
    // `PageBreak.asd` sidecar.
    let data = storage
        .read_image_storage(name)
        .map_err(|error| error.to_string())?;
    let bytes = data.as_bytes().map_err(|error| error.to_string())?;
    let image = decode_image_bytes(&bytes, name)?;
    cache.insert(key, image.clone());
    Ok(image)
}

pub(crate) fn decode_image_bytes(
    bytes: &[u8],
    name: &str,
) -> std::result::Result<DecodedImageData, String> {
    if libtlg_rs::is_valid_tlg(bytes) {
        let tlg = libtlg_rs::load_tlg(Cursor::new(bytes))
            .map_err(|error| format!("failed to decode TLG image `{name}`: {error}"))?;
        return tlg_to_rgba(tlg, name);
    }

    let decoded = image::load_from_memory(bytes)
        .map_err(|error| format!("failed to decode image `{name}`: {error}"))?
        .to_rgba8();
    let width = decoded.width();
    let height = decoded.height();
    let mut rgba = decoded.into_raw();
    apply_magenta_color_key(&mut rgba);
    Ok(DecodedImageData {
        width,
        height,
        rgba: Arc::<[u8]>::from(rgba),
    })
}

/// Official `TVPLoadGraphic(..., glmPalettized)` for province images
/// (`GraphicsLoaderIntf.h:57`): the result must be 8-bit and the source's
/// color index must be preserved. KRKR's PNG loader accepts palette or
/// grayscale sources of at most 8 bits and rejects the rest
/// (`LoadPNG.cpp:255`); non-PNG sources fall back to luminance, which is
/// exact for a grayscale map and an approximation for a palettized one.
pub(crate) fn decode_province_image(
    bytes: &[u8],
    name: &str,
) -> std::result::Result<ProvinceImage, String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return decode_png_province(bytes, name);
    }
    let decoded = decode_image_bytes(bytes, name)?;
    let mut pixels = Vec::with_capacity(decoded.rgba.len() / 4);
    for rgba in decoded.rgba.chunks_exact(4) {
        let luma = 0.299 * f32::from(rgba[0]) + 0.587 * f32::from(rgba[1]) + 0.114 * f32::from(rgba[2]);
        pixels.push(luma.round().clamp(0.0, 255.0) as u8);
    }
    Ok(ProvinceImage::new(decoded.width, decoded.height, pixels))
}

fn decode_png_province(
    bytes: &[u8],
    name: &str,
) -> std::result::Result<ProvinceImage, String> {
    use png::{BitDepth, ColorType, Transformations};

    let probe = png::Decoder::new(Cursor::new(bytes))
        .read_info()
        .map_err(|error| format!("failed to decode province image `{name}`: {error}"))?;
    let color_type = probe.info().color_type;
    let bit_depth = probe.info().bit_depth;
    let (width, height) = (probe.info().width, probe.info().height);
    drop(probe);

    let supported = matches!(color_type, ColorType::Indexed | ColorType::Grayscale)
        && bit_depth != BitDepth::Sixteen;
    if !supported {
        return Err(format!(
            "province image `{name}` must be an 8-bit palettized or grayscale image"
        ));
    }

    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    if color_type == ColorType::Grayscale {
        // 1/2/4-bit grayscale samples expand to one byte per pixel; indexed
        // sources keep their packed indices so the palette index survives.
        decoder.set_transformations(Transformations::EXPAND);
    } else {
        decoder.set_transformations(Transformations::IDENTITY);
    }
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("failed to decode province image `{name}`: {error}"))?;
    let mut buffer = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("failed to decode province image `{name}`: {error}"))?;

    let mut pixels = Vec::with_capacity((width as usize) * (height as usize));
    match (info.color_type, info.bit_depth) {
        (ColorType::Grayscale, BitDepth::Eight) => {
            for y in 0..height as usize {
                let row = &buffer[y * info.line_size..(y + 1) * info.line_size];
                pixels.extend_from_slice(&row[..width as usize]);
            }
        }
        // A grayscale tRNS chunk expands to an alpha pair; the gray sample is
        // still the province value.
        (ColorType::GrayscaleAlpha, BitDepth::Eight) => {
            for y in 0..height as usize {
                let row = &buffer[y * info.line_size..(y + 1) * info.line_size];
                for x in 0..width as usize {
                    pixels.push(row[x * 2]);
                }
            }
        }
        (ColorType::Indexed, BitDepth::Eight) => {
            for y in 0..height as usize {
                let row = &buffer[y * info.line_size..(y + 1) * info.line_size];
                pixels.extend_from_slice(&row[..width as usize]);
            }
        }
        (ColorType::Indexed, depth) => {
            let bits = match depth {
                BitDepth::One => 1,
                BitDepth::Two => 2,
                BitDepth::Four => 4,
                _ => 8,
            };
            let mask = (1u8 << bits) - 1;
            for y in 0..height as usize {
                let row = &buffer[y * info.line_size..(y + 1) * info.line_size];
                for x in 0..width as usize {
                    let bit = x * bits;
                    let byte = row.get(bit / 8).copied().unwrap_or(0);
                    let shift = 8 - bits - (bit % 8);
                    pixels.push((byte >> shift) & mask);
                }
            }
        }
        _ => {
            return Err(format!(
                "province image `{name}` must be an 8-bit palettized or grayscale image"
            ));
        }
    }
    Ok(ProvinceImage::new(width, height, pixels))
}

fn apply_magenta_color_key(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        if pixel[0] == 255 && pixel[1] == 0 && pixel[2] == 255 && pixel[3] == 255 {
            pixel[3] = 0;
        }
    }
}

fn tlg_to_rgba(tlg: libtlg_rs::Tlg, name: &str) -> std::result::Result<DecodedImageData, String> {
    use libtlg_rs::TlgColorType;

    let pixels = usize::try_from(tlg.width)
        .ok()
        .and_then(|width| {
            usize::try_from(tlg.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| format!("TLG image `{name}` dimensions overflow"))?;
    let channels = match tlg.color {
        TlgColorType::Grayscale8 => 1,
        TlgColorType::Bgr24 => 3,
        TlgColorType::Bgra32 => 4,
    };
    let expected = pixels
        .checked_mul(channels)
        .ok_or_else(|| format!("TLG image `{name}` buffer size overflow"))?;
    if tlg.data.len() != expected {
        return Err(format!(
            "TLG image `{name}` decoded to {} bytes, expected {expected}",
            tlg.data.len()
        ));
    }

    let mut rgba = Vec::with_capacity(
        pixels
            .checked_mul(4)
            .ok_or_else(|| format!("TLG image `{name}` RGBA size overflow"))?,
    );
    match tlg.color {
        TlgColorType::Grayscale8 => {
            for value in tlg.data {
                rgba.extend_from_slice(&[value, value, value, 255]);
            }
        }
        TlgColorType::Bgr24 => {
            for pixel in tlg.data.chunks_exact(3) {
                let alpha = if pixel[2] == 255 && pixel[1] == 0 && pixel[0] == 255 {
                    0
                } else {
                    255
                };
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], alpha]);
            }
        }
        TlgColorType::Bgra32 => {
            for pixel in tlg.data.chunks_exact(4) {
                // A number of KRKR UI assets use the historical magenta
                // colour key (RGB 255,0,255) for transparent pixels.  TLG
                // stores those pixels as fully opaque BGRA, so preserve the
                // engine's colour-key semantics while retaining real alpha
                // values on ordinary pixels.
                let alpha =
                    if pixel[2] == 255 && pixel[1] == 0 && pixel[0] == 255 && pixel[3] == 255 {
                        0
                    } else {
                        pixel[3]
                    };
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], alpha]);
            }
        }
    }
    Ok(DecodedImageData {
        width: tlg.width,
        height: tlg.height,
        rgba: Arc::from(rgba),
    })
}

impl DecodedImageCache {
    fn new(capacity_bytes: usize, max_entry_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            bytes: 0,
            capacity_bytes,
            max_entry_bytes,
        }
    }

    fn get(&mut self, key: &DecodedImageCacheKey) -> Option<DecodedImageData> {
        let image = self.entries.get(key)?.image.clone();
        self.touch(key.clone());
        Some(image)
    }

    fn insert(&mut self, key: DecodedImageCacheKey, image: DecodedImageData) {
        let bytes = image.rgba.len();
        if bytes > self.max_entry_bytes || bytes > self.capacity_bytes {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries
            .insert(key.clone(), DecodedImageCacheEntry { image, bytes });
        self.touch(key);
        self.evict_to_capacity();
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.bytes = 0;
    }

    fn touch(&mut self, key: DecodedImageCacheKey) {
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn converts_tlg_bgra_pixels_to_rgba() {
        let image = tlg_to_rgba(
            libtlg_rs::Tlg {
                tags: HashMap::new(),
                version: 6,
                width: 2,
                height: 1,
                color: libtlg_rs::TlgColorType::Bgra32,
                data: vec![3, 2, 1, 4, 30, 20, 10, 40],
            },
            "probe.tlg",
        )
        .expect("convert TLG pixels");

        assert_eq!(image.width, 2);
        assert_eq!(image.height, 1);
        assert_eq!(image.rgba.as_ref(), &[1, 2, 3, 4, 10, 20, 30, 40]);
    }

    #[test]
    fn converts_tlg_magenta_colour_key_to_transparency() {
        let image = tlg_to_rgba(
            libtlg_rs::Tlg {
                tags: HashMap::new(),
                version: 6,
                width: 1,
                height: 1,
                color: libtlg_rs::TlgColorType::Bgra32,
                data: vec![255, 0, 255, 255],
            },
            "colour-key.tlg",
        )
        .expect("convert TLG colour key");

        assert_eq!(image.rgba.as_ref(), &[255, 0, 255, 0]);

        let image = tlg_to_rgba(
            libtlg_rs::Tlg {
                tags: HashMap::new(),
                version: 6,
                width: 1,
                height: 1,
                color: libtlg_rs::TlgColorType::Bgr24,
                data: vec![255, 0, 255],
            },
            "colour-key24.tlg",
        )
        .expect("convert TLG 24-bit colour key");
        assert_eq!(image.rgba.as_ref(), &[255, 0, 255, 0]);
    }

    /// A `.tlg` that is really a WebP with a *separate alpha plane*
    /// (`RIFF/WEBP`, `VP8X` + `ALPH` + `VP8`) — the shape 少女世界的生存之道
    /// ships in `fgimage/**.tlg`. `is_valid_tlg` rejects it, so the bytes take
    /// the `image::load_from_memory` branch, and the alpha plane has to survive
    /// that decoder: a dropped plane turns every sprite into a drawn-but-
    /// transparent layer, which is indistinguishable from "no layer" in a
    /// screenshot.
    ///
    /// The game's assets are not redistributable, so this 64x64 fixture was
    /// generated once from a soft-edged circle (`cwebp -q 80 -alpha_q 90 -m 6`)
    /// and checked in as bytes: alpha 0 outside the circle, 255 inside, and
    /// 99 anti-aliased steps between them. The alpha plane it decodes to is
    /// identical under `dwebp`, macOS ImageIO and the `image` crate.
    #[rustfmt::skip]
    const WEBP_SEPARATE_ALPHA: &[u8] = &[
        0x52, 0x49, 0x46, 0x46, 0xd8, 0x02, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x58,
        0x0a, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x3f, 0x00, 0x00, 0x3f, 0x00, 0x00, 0x41, 0x4c,
        0x50, 0x48, 0x73, 0x02, 0x00, 0x00, 0x1d, 0xa0, 0x24, 0x49, 0x92, 0xe3, 0x48, 0x32, 0x4f, 0x70,
        0xa0, 0x89, 0x54, 0xff, 0xff, 0x8d, 0x53, 0x4d, 0x81, 0x02, 0x4b, 0x3b, 0x40, 0x1a, 0x45, 0x7a,
        0xc8, 0xee, 0x2d, 0x22, 0x26, 0x80, 0xff, 0x33, 0x67, 0x8b, 0x79, 0x09, 0xd6, 0xcb, 0xf9, 0xfa,
        0x03, 0xca, 0x76, 0xbd, 0x9e, 0x41, 0x00, 0xe1, 0xda, 0x75, 0xfb, 0x3a, 0xa9, 0xf5, 0xc3, 0x96,
        0x10, 0xbe, 0x17, 0xd9, 0x7f, 0x76, 0x93, 0x59, 0x3f, 0x2f, 0x03, 0x01, 0x62, 0x30, 0x02, 0x82,
        0xa7, 0xb7, 0x6e, 0x12, 0xa5, 0xd9, 0x10, 0x08, 0x21, 0x06, 0x30, 0x22, 0x82, 0x1c, 0xda, 0x3a,
        0xde, 0xa6, 0x09, 0x21, 0x21, 0x40, 0xb8, 0x15, 0x10, 0x45, 0x6c, 0x0f, 0x63, 0x3d, 0xed, 0x42,
        0x42, 0x08, 0x24, 0x20, 0x01, 0x05, 0x11, 0xc5, 0xaf, 0xf7, 0x71, 0x9a, 0x55, 0x48, 0x08, 0x09,
        0x01, 0x02, 0x08, 0x88, 0x22, 0x8a, 0xc7, 0x76, 0x8c, 0x66, 0x51, 0x48, 0x48, 0x02, 0xb9, 0x01,
        0x32, 0x5f, 0xcc, 0x51, 0x50, 0x51, 0xea, 0xb9, 0x1d, 0xee, 0x69, 0x55, 0x2c, 0xdc, 0x50, 0xc2,
        0xbd, 0xb3, 0xf5, 0x6e, 0x6d, 0xe5, 0x86, 0x9a, 0x7a, 0x7c, 0x1f, 0x6a, 0xb3, 0x0b, 0x85, 0x50,
        0xa0, 0xd0, 0x7b, 0xfe, 0xb4, 0xad, 0x50, 0x91, 0x8a, 0x5f, 0x87, 0x61, 0x4a, 0x13, 0x42, 0x12,
        0xc2, 0xa0, 0xcb, 0x97, 0x88, 0x8a, 0xd8, 0xd6, 0x41, 0x9a, 0x90, 0x84, 0x24, 0x0c, 0xdd, 0xac,
        0x54, 0x54, 0x6c, 0x87, 0x58, 0x6f, 0x48, 0xa0, 0x10, 0x86, 0x7f, 0xdc, 0x48, 0x05, 0xe5, 0xd0,
        0x0d, 0xf0, 0x4c, 0x42, 0x42, 0x18, 0xf3, 0x71, 0x25, 0x8a, 0xf2, 0xd6, 0x6f, 0xbd, 0x0c, 0x24,
        0x61, 0xe4, 0x26, 0xa8, 0xe0, 0xa9, 0xeb, 0xf5, 0x10, 0x12, 0x42, 0x19, 0x89, 0x97, 0x8a, 0x28,
        0x7e, 0xf6, 0x29, 0x5b, 0x08, 0xa1, 0x30, 0xf6, 0x72, 0x5b, 0x11, 0x61, 0x5f, 0x7b, 0x6c, 0x21,
        0x24, 0x64, 0x34, 0x9e, 0x44, 0x11, 0xf6, 0x3d, 0xd6, 0x09, 0x81, 0x30, 0xfe, 0x7c, 0x2d, 0x88,
        0x76, 0x7d, 0x20, 0x90, 0x29, 0xb0, 0x53, 0x10, 0x7a, 0xcc, 0x66, 0x01, 0xc2, 0x24, 0xd7, 0x20,
        0xe0, 0xf5, 0x7a, 0xd7, 0x02, 0xc8, 0x54, 0x66, 0x73, 0x44, 0xe0, 0x7c, 0xd7, 0x1c, 0x62, 0x98,
        0xe8, 0x02, 0x8c, 0x70, 0xb9, 0xab, 0x04, 0x03, 0x99, 0xc6, 0x5c, 0x30, 0x58, 0xef, 0x0a, 0x04,
        0x64, 0x9a, 0x21, 0x20, 0x78, 0xd7, 0xcf, 0x17, 0x84, 0x90, 0x49, 0x88, 0x90, 0x3e, 0x55, 0x22,
        0x38, 0x9f, 0xc4, 0x25, 0x10, 0xc9, 0x7d, 0x17, 0x30, 0xc2, 0x62, 0x12, 0x67, 0x88, 0xa1, 0xe7,
        0x19, 0x10, 0x99, 0xcf, 0x26, 0x70, 0xbd, 0x10, 0x42, 0xdf, 0xeb, 0x55, 0x40, 0x58, 0x4f, 0xa0,
        0x83, 0x00, 0xe9, 0x41, 0x07, 0x82, 0xee, 0x26, 0xf0, 0x95, 0x40, 0xe8, 0xdd, 0x29, 0x82, 0xeb,
        0xf9, 0x68, 0x97, 0x2e, 0x10, 0x92, 0x3e, 0x7b, 0x10, 0xc5, 0xa7, 0xd1, 0xde, 0x43, 0x42, 0xe8,
        0x5d, 0xf7, 0x20, 0x52, 0xb7, 0xcb, 0x91, 0x4e, 0xfb, 0x42, 0x08, 0x03, 0x7e, 0x8a, 0x22, 0xf5,
        0x65, 0xa4, 0x5f, 0x85, 0x90, 0x90, 0x7e, 0xdd, 0x49, 0x50, 0x49, 0x33, 0x4a, 0x2b, 0x49, 0x20,
        0x0c, 0xf8, 0x86, 0xa2, 0xb8, 0x7a, 0x1c, 0xe1, 0xe3, 0x18, 0x12, 0x12, 0x86, 0xec, 0x0e, 0x28,
        0x54, 0xdc, 0x3c, 0x0e, 0xf6, 0x71, 0x08, 0x05, 0x12, 0x86, 0x6d, 0x45, 0x45, 0x5d, 0x35, 0x03,
        0xb5, 0xc7, 0x24, 0x24, 0x21, 0xc3, 0xd4, 0x56, 0x44, 0xc5, 0xbc, 0x2c, 0x07, 0x38, 0xfd, 0x32,
        0x24, 0x21, 0x84, 0x81, 0x0f, 0x5f, 0x52, 0x91, 0x0a, 0x75, 0xfb, 0x34, 0xef, 0x71, 0x79, 0xdf,
        0x17, 0x28, 0x84, 0x42, 0x18, 0xfc, 0xfd, 0x58, 0x53, 0x11, 0xa5, 0xba, 0xde, 0xad, 0x67, 0xdf,
        0x5c, 0xbb, 0xaf, 0x2e, 0x85, 0x84, 0x50, 0x2c, 0x8c, 0xd8, 0x9e, 0x2b, 0xde, 0x08, 0xca, 0x7c,
        0x31, 0x0f, 0x5e, 0xce, 0x17, 0x12, 0xc8, 0x4d, 0x28, 0x8c, 0xda, 0x1e, 0x45, 0x11, 0x45, 0x40,
        0x20, 0x40, 0x48, 0x08, 0x09, 0x61, 0xe4, 0xf7, 0x2f, 0x51, 0x44, 0x50, 0x08, 0x42, 0x02, 0x21,
        0x24, 0x84, 0xd1, 0x0f, 0xad, 0x88, 0x22, 0x20, 0xb7, 0x01, 0x42, 0x42, 0x08, 0x13, 0xac, 0xed,
        0x01, 0x41, 0xc4, 0x08, 0xc4, 0x10, 0x02, 0x61, 0xaa, 0xdd, 0xdb, 0x49, 0x10, 0x30, 0x12, 0x03,
        0x04, 0xc2, 0x84, 0xbb, 0xcf, 0x3d, 0x22, 0xdf, 0xe7, 0x66, 0xe2, 0x75, 0xdf, 0x75, 0x57, 0x10,
        0x08, 0x3f, 0xf6, 0x7a, 0xbe, 0x54, 0xf9, 0xcf, 0x1b, 0x00, 0x56, 0x50, 0x38, 0x20, 0x3e, 0x00,
        0x00, 0x00, 0x90, 0x03, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x00, 0x40, 0x00, 0x3e, 0x6d, 0x36, 0x98,
        0x49, 0x24, 0x23, 0x22, 0xa1, 0x22, 0x08, 0x00, 0x80, 0x0d, 0x89, 0x69, 0x00, 0x00, 0x11, 0x9f,
        0x71, 0xd6, 0x00, 0x2b, 0xc4, 0x29, 0x80, 0x00, 0xfe, 0xeb, 0xde, 0x2d, 0xc5, 0x34, 0xcf, 0xff,
        0xf6, 0xd0, 0xff, 0xff, 0x6d, 0x0f, 0xff, 0xf6, 0xd0, 0xff, 0x97, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn webp_with_a_separate_alpha_plane_decodes_its_alpha() {
        assert!(
            !libtlg_rs::is_valid_tlg(WEBP_SEPARATE_ALPHA),
            "a WebP must take the image-crate branch, not the TLG loader"
        );

        let image = decode_image_bytes(WEBP_SEPARATE_ALPHA, "probe.tlg").expect("decode webp");
        assert_eq!((image.width, image.height), (64, 64));

        let mut zero = 0usize;
        let mut opaque = 0usize;
        let mut present = 0usize;
        for pixel in image.rgba.chunks_exact(4) {
            match pixel[3] {
                0 => zero += 1,
                255 => opaque += 1,
                _ => present += 1,
            }
        }
        assert_eq!(zero, 1968, "transparent area of the fixture");
        assert_eq!(opaque, 1020, "opaque area of the fixture");
        assert_eq!(present, 1108, "anti-aliased pixels keep their alpha");

        let alpha_at = |x: usize, y: usize| image.rgba[(y * 64 + x) * 4 + 3];
        assert_eq!(alpha_at(0, 0), 0);
        assert_eq!(alpha_at(32, 32), 255, "the centre of the circle is opaque");
        // `ALPH` is a separate plane: the colour under a transparent pixel is
        // kept as decoded, not zeroed by a premultiply.
        let colour_at = |x: usize, y: usize| {
            let start = (y * 64 + x) * 4;
            (
                image.rgba[start],
                image.rgba[start + 1],
                image.rgba[start + 2],
            )
        };
        assert_eq!(colour_at(0, 0), (223, 39, 92));
        assert_eq!(colour_at(32, 32), (221, 40, 90));
    }

    #[test]
    fn dropping_manager_clone_does_not_shutdown_worker() {
        let root = temp_root("resource-manager-clone");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("payload.bin"), b"payload").expect("write payload");
        let storage = ProjectStorage::for_root(&root).expect("storage");
        let manager = ResourceManager::new(Arc::new(storage.clone())).expect("manager");

        drop(manager.clone());

        let data = manager
            .load_bytes_blocking("payload.bin")
            .expect("bytes result");
        assert_eq!(data.as_bytes().expect("payload bytes").as_ref(), b"payload");

        fs::remove_dir_all(root).expect("cleanup");
    }

    fn temp_root(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "Kirakira-engine-{prefix}-{}-{nanos}",
            std::process::id()
        ))
    }
}
