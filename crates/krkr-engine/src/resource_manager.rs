use std::{
    collections::{HashMap, VecDeque},
    io::Cursor,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use krkr_core::ResourceData;

use crate::storage::ProjectStorage;

const DECODED_IMAGE_CACHE_CAPACITY_BYTES: usize = 128 * 1024 * 1024;
const DECODED_IMAGE_CACHE_MAX_ENTRY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceTaskId(pub u64);

#[derive(Clone)]
pub struct ResourceManager {
    task_tx: mpsc::Sender<ResourceTask>,
    completion_rx: Arc<Mutex<mpsc::Receiver<ResourceCompletion>>>,
    next_task_id: Arc<AtomicU64>,
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
    DecodeImageBlocking {
        revision: u64,
        storage: String,
        reply_tx: mpsc::Sender<std::result::Result<DecodedImageData, String>>,
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
    pub fn new(storage: ProjectStorage) -> std::io::Result<Self> {
        let (task_tx, task_rx) = mpsc::channel();
        let (completion_tx, completion_rx) = mpsc::channel();
        thread::Builder::new()
            .name("krkr-resource-worker".to_string())
            .spawn(move || resource_worker(storage, task_rx, completion_tx))?;
        Ok(Self {
            task_tx,
            completion_rx: Arc::new(Mutex::new(completion_rx)),
            next_task_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn request_image_decode(
        &self,
        storage: impl Into<String>,
        revision: u64,
    ) -> ResourceTaskId {
        let storage = storage.into();
        let id = self.next_id();
        let _ = self.task_tx.send(ResourceTask::DecodeImage {
            id,
            revision,
            storage,
        });
        id
    }

    pub fn load_bytes_blocking(
        &self,
        storage: impl Into<String>,
    ) -> std::result::Result<ResourceData, String> {
        let storage = storage.into();
        let (reply_tx, reply_rx) = mpsc::channel();
        self.task_tx
            .send(ResourceTask::LoadBytesBlocking {
                storage: storage.clone(),
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

    pub fn decode_image_blocking(
        &self,
        storage: impl Into<String>,
        revision: u64,
    ) -> std::result::Result<DecodedImageData, String> {
        let storage = storage.into();
        let (reply_tx, reply_rx) = mpsc::channel();
        self.task_tx
            .send(ResourceTask::DecodeImageBlocking {
                revision,
                storage: storage.clone(),
                reply_tx,
            })
            .map_err(|_| format!("resource worker is not available for `{storage}`"))?;
        reply_rx
            .recv()
            .map_err(|_| format!("resource worker stopped while decoding `{storage}`"))?
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

    fn next_id(&self) -> ResourceTaskId {
        ResourceTaskId(self.next_task_id.fetch_add(1, Ordering::Relaxed))
    }
}

fn resource_worker(
    storage: ProjectStorage,
    task_rx: mpsc::Receiver<ResourceTask>,
    completion_tx: mpsc::Sender<ResourceCompletion>,
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
            } => ResourceCompletion {
                id,
                revision,
                storage: name.clone(),
                result: decode_image(&storage, &mut image_cache, revision, &name),
            },
            ResourceTask::LoadBytesBlocking {
                storage: name,
                reply_tx,
            } => {
                let _ = reply_tx.send(
                    storage
                        .read_binary_storage(&name)
                        .map_err(|error| error.to_string()),
                );
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
            ResourceTask::DecodeImageBlocking {
                revision,
                storage: name,
                reply_tx,
            } => {
                let _ = reply_tx.send(decode_image(&storage, &mut image_cache, revision, &name));
                continue;
            }
        };
        if completion_tx.send(completion).is_err() {
            break;
        }
    }
}

fn decode_image(
    storage: &ProjectStorage,
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

    let data = storage
        .read_binary_storage(name)
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
    Ok(DecodedImageData {
        width: decoded.width(),
        height: decoded.height(),
        rgba: Arc::<[u8]>::from(decoded.into_raw()),
    })
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
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
            }
        }
        TlgColorType::Bgra32 => {
            for pixel in tlg.data.chunks_exact(4) {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
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
    fn dropping_manager_clone_does_not_shutdown_worker() {
        let root = temp_root("resource-manager-clone");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("payload.bin"), b"payload").expect("write payload");
        let storage = ProjectStorage::for_root(&root).expect("storage");
        let manager = ResourceManager::new(storage.clone()).expect("manager");

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
