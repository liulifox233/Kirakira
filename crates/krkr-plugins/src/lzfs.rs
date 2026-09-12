//! `lzfs.dll` — the `lzfs` storage media (LZ4 frame passthrough).
//!
//! The reference has **no TJS surface at all**: `V2Link` (`0x10001630`)
//! registers one storage media named `lzfs` (`LZ4Storage`, vtable
//! `0x1000c674`) with `TVPRegisterStorageMedia`, `V2Unlink` (`0x10001080`)
//! unregisters it, and a refused registration makes `V2Link` return `E_FAIL`
//! (`docs/plugins/lzfs.md:39-49`). What the media serves is "every
//! file transparently LZ4-decompressed when the file is an LZ4 frame, and raw
//! bytes otherwise" (`lzfs.md:39-42`): a game can drop LZ4-framed files
//! anywhere in the resource tree and read them through the `lzfs://` storage
//! domain (`lzfs.md:82-84`).
//!
//! Everything the media decides lives in two routines of the DLL:
//!
//! * `LZ4Storage::Open` (`0x10001ac0`, `lzfs.md:69-80`) opens the name it was
//!   handed through the engine's **own storage search** (`TVPCreateIStream`,
//!   `lzfs.md:70-73`) — the media is a pure wrapper with no name space of its
//!   own — and throws `cannot open lz4 file:%1` when that fails
//!   (`lzfs.md:79`).
//! * `LZ4Stream::Init` (`0x10001250`, `lzfs.md:73-76`) feeds the payload to an
//!   `LZ4F` decompression context (`LZ4F_createDecompressionContext(…, 100)`,
//!   `lzfs.md:15`) in 64 KiB reads. **Any** LZ4F error declares the file "not
//!   LZ4", and the media then returns the *same inner stream*, rewound to its
//!   start, for raw passthrough (`lzfs.md:77-78`, `:80`). When the frame does
//!   decode, `Init` has already materialized the whole decoded file in memory
//!   (a `GlobalAlloc`/`CreateStreamOnHGlobal` `IStream`), and that buffer is
//!   what the game reads from.
//!
//! This port is that shape: `LzfsMedia::open` resolves the name through the
//! weak handle to the built-in stack the engine attaches
//! ([`StorageMediaProvider::attach_storage`]), walks the payload to see
//! whether one complete LZ4 frame is there (`frame_extent`) and serves the
//! decoded bytes from memory when it is; anything else is served raw. The
//! rest of `iTVPStorageMedia` is the reference's too: existence delegates to
//! the stack (`TVPIsExistentStorage`, `lzfs.md:63`), listing is unsupported
//! (`GetListAt` is a no-op, `lzfs.md:65`), `GetLocallyAccessibleName` is the
//! empty string (`lzfs.md:66-67`, which is what the engine already answers
//! for a media) and writes keep the engine's read-only default — the
//! reference's `Open` would pass a write flag to the *inner* file, and no
//! write path ever produces an LZ4 frame.
//!
//! # Corners the DLL cannot answer, and what this port does
//!
//! `Init`'s loop exits only when `LZ4F_decompress` reports the frame complete
//! or fails, so the DLL says nothing about a payload that *never* completes a
//! frame — and the LZ4F of its vintage (lz4 1.9.2/1.9.3) answers 0-size input
//! at the frame header with the 7 bytes it still wants
//! (`lib/lz4frame.c:1426` in 1.9.3, `:1408` in 1.9.2) and mid-frame with the
//! rest of the block header (`:1495`, `:1475`): `while (uVar3 != 0)` spins
//! forever over a read that keeps returning `S_OK` with 0 bytes. **A truncated
//! frame — and an empty payload, which needs its frame header the same way —
//! hangs the reference.** This port treats "no complete frame" as the same
//! "not LZ4" answer `Init` gives every other frame error, and serves the
//! payload raw.
//!
//! Two neighbouring corners, for the record:
//!
//! * The *legacy* magic `0x184C2102` is not an LZ4 frame to LZ4F —
//!   `LZ4F_decodeHeader` answers `ERROR_frameType_unknown`
//!   (`lz4frame.c:1142`) — so such a file is raw in the reference and here.
//! * A *skippable* frame is the one deliberate divergence: LZ4F skips it and
//!   reports the frame complete with no decompressed output, which the
//!   reference turns into an empty file; serving those bytes raw at least
//!   keeps what the file holds (`lzfs.md:133-135` records the same open
//!   question).
//!
//! `lz4_flex`'s frame API is the decoder (`lzfs.md:126-128`: close, but **not**
//! identical in failure semantics — it reports a payload truncated at a block
//! boundary as a clean end of input, which is why this module walks the frame
//! structure itself instead of trusting a decode attempt to fail).

use std::{
    io::{self, Cursor, Read, Seek, SeekFrom},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::{self, ProjectStoragePort, ResourceStream, StorageMediaProvider},
};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "the `lzfs` storage media (LZ4-frame passthrough)",
    notes: "Real: `register()` installs a media named `lzfs` on the project storage \
            (`TVPRegisterStorageMedia`, 0x100015d0), `unregister()` takes it away (`Plugins.unlink`), \
            every name resolves through the built-in stack (the media is a pure wrapper), a complete \
            modern LZ4 frame is decoded with its checksums and declared size verified, and anything \
            else is served raw — `LZ4Storage::Open` 0x10001ac0 + `LZ4Stream::Init` 0x10001250. \
            Deviations, all in corners the DLL leaves undecided: a payload whose frame never \
            completes (truncated frame, empty file) hangs the reference's `Init` loop and is served \
            raw here; a bare skippable frame would be an empty file there and is raw here; a \
            legacy-magic file is raw in both. Read-only and script-invisible, like the reference.",
    install: |engine| engine.register_plugin(LzfsPlugin::new()),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what a game's
/// `Plugins.link("lzfs.dll")` resolves through [`crate::catalog`].
pub(crate) const NAME: &str = "lzfs.dll";

/// The media name the reference registers with `TVPRegisterStorageMedia`
/// (`LZ4Storage::GetName` assigns `L"lzfs"`, `lzfs.md:60`).
pub(crate) const MEDIA_NAME: &str = "lzfs";

/// The banner `V2Link` logs at load (`lzfs.md:12`).
const BANNER: &str = "-- Kirikiri LZ4fs Plugin ---";

/// `cannot open lz4 file:%1` (`lzfs.md:79`), the message `Open` raises when
/// the inner `TVPCreateIStream` found nothing. `%1` is the reference's
/// placeholder for the name the media was handed.
const OPEN_ERROR: &str = "cannot open lz4 file:";

/// The LZ4 frame magic (`LZ4F_MAGICNUMBER`, `lib/lz4frame.c:210`).
const LZ4_FRAME_MAGIC: u32 = 0x184D_2204;

/// The `lzfs` media (`LZ4Storage`): a pure wrapper around the engine's
/// built-in stack that LZ4-decompresses what it finds there.
struct LzfsMedia {
    /// The built-in stack, attached by the engine before the media is
    /// registered. Weak, because the registry lives inside the storage — a
    /// strong handle here would be a reference cycle (and a wrapping media
    /// resolves ordinary names through it, never its own scheme).
    storage: Mutex<Option<Weak<dyn ProjectStoragePort>>>,
}

impl LzfsMedia {
    fn new() -> Self {
        Self {
            storage: Mutex::new(None),
        }
    }

    /// The engine's built-in stack, or `None` once it is gone.
    fn storage(&self) -> Option<Arc<dyn ProjectStoragePort>> {
        self.storage.lock().ok()?.as_ref().and_then(Weak::upgrade)
    }
}

impl StorageMediaProvider for LzfsMedia {
    fn media_name(&self) -> &str {
        MEDIA_NAME
    }

    /// `CheckExistentStorage` (`0x100011d0`) delegates straight to
    /// `TVPIsExistentStorage(name)` (`lzfs.md:63`): the media owns no name
    /// space of its own, so a name is served exactly when the built-in stack
    /// has it. A lost storage handle is a miss, never a panic.
    fn exists(&self, name: &str) -> bool {
        self.storage()
            .is_some_and(|storage| storage.storage_exists(name))
    }

    /// The engine attaches a weak handle to the built-in stack before the
    /// media is inserted (`KrkrHost::register_storage_media`); it is what the
    /// reference's `TVPCreateIStream` and `TVPIsExistentStorage` calls reach
    /// instead of a `TVPGetLocalName` global (`lzfs.md:96-115`).
    fn attach_storage(&self, storage: Weak<dyn ProjectStoragePort>) {
        if let Ok(mut slot) = self.storage.lock() {
            *slot = Some(storage);
        }
    }

    /// `Open(name, flags)` (`0x10001ac0`): `TVPCreateIStream` on the inner
    /// stack, then the `Init` sniff. Not an LZ4 frame — including every frame
    /// error the reference answers with "not LZ4" (`lzfs.md:74-76`) — means
    /// the *same stream*, rewound, is served raw (`lzfs.md:77-78`, `:80`).
    fn open(&self, name: &str) -> io::Result<Box<dyn ResourceStream>> {
        let storage = self.storage().ok_or_else(|| {
            io::Error::other(format!("the `{MEDIA_NAME}` media has no storage attached"))
        })?;
        // The reference throws its own message when the inner open found
        // nothing; the reason the built-in stack gave is not what a game (or
        // the engine log) is meant to see.
        let mut inner = storage.open(name).map_err(|_| open_error(name))?;
        match decode_lz4_frame(&mut *inner) {
            Some(decoded) => Ok(Box::new(Cursor::new(decoded))),
            None => {
                inner.seek(SeekFrom::Start(0))?;
                Ok(inner)
            }
        }
    }
}

/// `LZ4Storage::Open`'s failure message with the name filled in for `%1`
/// (`lzfs.md:79`).
fn open_error(name: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{OPEN_ERROR}{name}"))
}

/// The decoded payload of an LZ4 frame, or `None` when the stream is not one.
///
/// `None` covers everything `Init` answers with "not LZ4": a payload that is
/// no frame at all, one that never completes (see the module docs — the
/// reference hangs there), and every frame whose decode fails a check
/// afterwards: the decoder verifies the header checksum, the block checksums,
/// the declared content size and the content checksum, exactly the set
/// `Init`'s `LZ4F_decompress` validates, so a mismatch is raw passthrough the
/// way it is in the reference.
fn decode_lz4_frame(stream: &mut dyn ResourceStream) -> Option<Vec<u8>> {
    let extent = frame_extent(stream).ok()??;
    stream.seek(SeekFrom::Start(0)).ok()?;
    let mut decoder = lz4_flex::frame::FrameDecoder::new((&mut *stream).take(extent));
    let mut decoded = Vec::new();
    decoder.read_to_end(&mut decoded).ok()?;
    Some(decoded)
}

/// The offset just past the first LZ4 frame in `stream`, or `None` when the
/// payload is not one complete modern LZ4 frame — which is what makes the
/// media serve it raw.
///
/// This is the port's shape of `Init`'s decision, and it is a *structural*
/// walk for the reason in the module docs: only a payload whose frame ends
/// with a proper end marker counts, so a truncation anywhere (even where
/// `lz4_flex` would report a clean end of input) leaves the file raw instead
/// of serving half a frame. Bounding the frame here is also what stops the
/// decode at the first frame's end mark, the way the reference's `Init` stops
/// as soon as `LZ4F_decompress` reports the frame complete.
fn frame_extent(stream: &mut dyn ResourceStream) -> io::Result<Option<u64>> {
    // Magic, FLG and BD; the descriptor's optional fields and its checksum
    // byte are skipped below (the decoder verifies the checksum itself).
    let mut fixed = [0u8; 6];
    if !read_exact_or_eof(stream, &mut fixed)? {
        return Ok(None);
    }
    if u32::from_le_bytes(fixed[0..4].try_into().expect("4 magic bytes")) != LZ4_FRAME_MAGIC {
        // Nothing else is an LZ4 frame to LZ4F: the legacy magic is
        // `ERROR_frameType_unknown` (`lz4frame.c:1142`) and a skippable frame
        // is skipped rather than decoded, which the reference turns into an
        // empty file.
        return Ok(None);
    }
    let flg = fixed[4];
    let bd = fixed[5];
    // Frame version 01 in FLG bits 7-6, no reserved FLG bit, and only BD's
    // block-size field (bits 6-4) may be set: `ERROR_headerVersion_wrong` /
    // `ERROR_reservedFlag_set` in `LZ4F_decodeHeader`.
    if flg >> 6 != 0b01 || flg & 0b0000_0010 != 0 || bd & 0b1000_1111 != 0 {
        return Ok(None);
    }
    // Block size codes 4-7; 0-3 are `ERROR_maxBlockSize_invalid`.
    let block_max: u64 = match (bd >> 4) & 0b111 {
        4 => 64 * 1024,
        5 => 256 * 1024,
        6 => 1024 * 1024,
        7 => 4 * 1024 * 1024,
        _ => return Ok(None),
    };
    let block_checksums = flg & 0b0001_0000 != 0;
    let content_checksum = flg & 0b0000_0100 != 0;
    // Optional content size (FLG bit 3) and dictionary id (bit 0), then the
    // header checksum byte.
    let extras: u64 = u64::from(flg & 0b0000_1000 != 0) * 8 + u64::from(flg & 0b0000_0001 != 0) * 4;
    let mut position = fixed.len() as u64 + extras + 1;
    stream.seek(SeekFrom::Current((extras + 1) as i64))?;

    loop {
        let mut header = [0u8; 4];
        if !read_exact_or_eof(stream, &mut header)? {
            return Ok(None);
        }
        position += 4;
        let header = u32::from_le_bytes(header);
        if header == 0 {
            // The frame's end marker; a content checksum, if the frame has
            // one, is the last 4 bytes (`dstage_getSuffix`).
            if content_checksum {
                position += 4;
            }
            return Ok(Some(position));
        }
        let size = u64::from(header & 0x7FFF_FFFF);
        if size > block_max {
            return Ok(None);
        }
        let skip = size + if block_checksums { 4 } else { 0 };
        stream.seek(SeekFrom::Current(skip as i64))?;
        position += skip;
    }
}

/// Reads `buffer` whole. `false` means the stream ended first, which for a
/// frame walk is "there is no complete frame here".
fn read_exact_or_eof(stream: &mut dyn Read, buffer: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => return Ok(false),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

/// `LZ4Storage`'s plugin shell: `V2Link` registers the one media, `V2Unlink`
/// takes it away (`lzfs.md:44-49`).
pub struct LzfsPlugin {
    /// The media instance. `register` runs at boot and again when the first
    /// `Plugins.link` installs the module (`crates/krkr-engine/src/native/plugins.rs:26-28`),
    /// and the storage registry accepts a second registration only when it is
    /// the *same* `Arc` — a different provider under a name already taken is
    /// refused the way `TVPMediaNameHadAlreadyBeenRegistered` is. Keeping the
    /// media here makes both calls hand back one instance.
    media: OnceLock<Arc<LzfsMedia>>,
}

impl LzfsPlugin {
    pub fn new() -> Self {
        Self {
            media: OnceLock::new(),
        }
    }
}

impl Default for LzfsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KrkrPlugin for LzfsPlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let first_register = self.media.get().is_none();
        let media = self.media.get_or_init(|| Arc::new(LzfsMedia::new()));
        let registered = plugin_api::storage::register_storage_media(
            runtime,
            Arc::clone(media) as Arc<dyn StorageMediaProvider>,
        );
        match registered {
            Ok(()) => {
                // The banner is what `V2Link` logs when the module loads
                // (`lzfs.md:12`); a re-registration stays quiet.
                if first_register {
                    runtime.host_mut().log(&format!(
                        "lzfs: {BANNER} media `{MEDIA_NAME}` registered — `{MEDIA_NAME}://` names \
                         resolve through the built-in stack, LZ4-frame payloads are decompressed \
                         and everything else passes through raw",
                    ));
                }
                Ok(())
            }
            // A host with no project storage (a script-only engine, a browser
            // host) has nothing a media could be inserted into and no way to
            // address `lzfs://` at all. The reference's `V2Link` reports
            // `E_FAIL` for a failed registration (`lzfs.md:46-49`); failing a
            // whole engine boot over a media such a host cannot use is not
            // this port's call, so the media stays unregistered and the log
            // carries the reason.
            Err(error) if runtime.host().project_storage().is_err() => {
                runtime.host_mut().log(&format!(
                    "WARN lzfs: media `{MEDIA_NAME}` not registered: {error}"
                ));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        plugin_api::storage::unregister_storage_media(runtime, MEDIA_NAME);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        time::SystemTime,
    };

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths};

    use super::*;

    /// An LZ4 frame around `payload`, the way an LZ4 compressor writes one.
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
        encoder.write_all(payload).expect("encode payload");
        encoder.finish().expect("finish frame")
    }

    /// Pseudo-random bytes: incompressible, so the frame carries real blocks.
    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    fn temp_root(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-lzfs-{prefix}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    /// A project of `root` with the plugin installed, the way a host boots
    /// it.
    fn project_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("project storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine");
        engine
            .register_plugin(LzfsPlugin::new())
            .expect("register the lzfs plugin");
        engine
    }

    fn write_file(root: &Path, name: &str, bytes: &[u8]) {
        if let Some(parent) = root.join(name).parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(root.join(name), bytes).expect("write fixture");
    }

    fn read(engine: &KrkrEngine, name: &str) -> Vec<u8> {
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");
        let data = storage
            .read_binary_storage(name)
            .unwrap_or_else(|error| panic!("read `{name}`: {error}"));
        data.as_bytes().expect("fixture bytes").into_owned()
    }

    /// The media resolves the name it is handed through the built-in stack
    /// and decompresses what it finds: the same file read without the scheme
    /// is still the compressed bytes.
    #[test]
    fn a_frame_payload_is_served_decompressed() {
        let root = temp_root("frame");
        let payload = noise(200_000);
        let compressed = frame(&payload);
        write_file(&root, "data/payload.bin", &compressed);

        let engine = project_engine(&root);
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );
        assert_eq!(read(&engine, "./data/payload.bin"), compressed);
        assert_eq!(read(&engine, "lzfs://./data/payload.bin"), payload);

        // `CheckExistentStorage` is the stack's answer, so a name the stack
        // does not have is a miss and the read fails at the built-in
        // resolution behind it.
        let storage = engine
            .tjs_runtime()
            .host()
            .project_storage()
            .expect("project storage");
        assert!(storage.storage_exists("lzfs://./data/payload.bin"));
        assert!(!storage.storage_exists("lzfs://./data/missing.bin"));
        assert!(
            storage
                .read_binary_storage("lzfs://./data/missing.bin")
                .is_err()
        );
    }

    /// The media is a pure wrapper: it owns no name space of its own, a name
    /// is served exactly when the attached stack has it, and a name the stack
    /// does not have fails the way the reference's `Open` does — with
    /// `cannot open lz4 file:%1` (`lzfs.md:79`).
    #[test]
    fn the_media_wraps_the_attached_stack() {
        let root = temp_root("wrapper");
        write_file(&root, "data/payload.bin", &frame(b"payload"));

        let storage: Arc<dyn ProjectStoragePort> =
            Arc::new(ProjectStorage::for_root(&root).expect("project storage"));
        let media = LzfsMedia::new();
        media.attach_storage(Arc::downgrade(&storage));

        assert_eq!(media.media_name(), MEDIA_NAME);
        assert!(media.exists("./data/payload.bin"));
        assert!(!media.exists("./data/missing.bin"));
        let error = media
            .open("./data/missing.bin")
            .map(|_| ())
            .expect_err("a missing entry must fail");
        assert_eq!(error.to_string(), format!("{OPEN_ERROR}./data/missing.bin"));
    }

    /// Everything that is not a complete LZ4 frame is served byte-identical.
    #[test]
    fn a_payload_that_is_no_frame_passes_through_byte_identical() {
        let root = temp_root("raw");
        let cases: [(&str, Vec<u8>); 5] = [
            ("plain.bin", b"not an LZ4 frame, just bytes".to_vec()),
            ("empty.bin", Vec::new()),
            ("short.bin", vec![0x04, 0x22]),
            (
                "magic.bin",
                [LZ4_FRAME_MAGIC.to_le_bytes().as_slice(), &[0x40, 0x40]].concat(),
            ),
            (
                "legacy.bin",
                [
                    0x184C_2102u32.to_le_bytes().as_slice(),
                    noise(64).as_slice(),
                ]
                .concat(),
            ),
        ];
        for (name, bytes) in &cases {
            write_file(&root, name, bytes);
        }

        let engine = project_engine(&root);
        for (name, bytes) in &cases {
            assert_eq!(
                read(&engine, &format!("lzfs://./{name}")),
                *bytes,
                "`{name}` must come back raw"
            );
        }
    }

    /// A frame that was cut anywhere is not a frame: every proper prefix of a
    /// valid frame comes back raw, and none of them hangs or panics. (The
    /// reference's `Init` never leaves its loop for these — see the module
    /// docs; this port applies the "not LZ4" rule instead.)
    #[test]
    fn every_truncated_prefix_falls_back_to_the_raw_bytes() {
        let root = temp_root("truncated");
        let payload = noise(512);
        let compressed = frame(&payload);
        let engine = project_engine(&root);

        for cut in 1..compressed.len() {
            let name = format!("cut-{cut:04}.bin");
            write_file(&root, &name, &compressed[..cut]);
            assert_eq!(
                read(&engine, &format!("lzfs://./{name}")),
                compressed[..cut],
                "a frame cut at {cut} bytes must come back raw"
            );
        }

        let name = "cut-complete.bin";
        write_file(&root, name, &compressed);
        assert_eq!(read(&engine, &format!("lzfs://./{name}")), payload);
    }

    /// The reference stops feeding LZ4F as soon as the frame is complete
    /// (`Init` exits on the first 0 hint), so bytes behind the frame are not
    /// part of the payload — and they are not an error either.
    #[test]
    fn bytes_after_a_complete_frame_are_ignored() {
        let root = temp_root("trailing");
        let payload = noise(4_096);
        let mut file = frame(&payload);
        file.extend_from_slice(b"trailing bytes that are not a frame");
        write_file(&root, "trailing.bin", &file);

        let engine = project_engine(&root);
        assert_eq!(read(&engine, "lzfs://./trailing.bin"), payload);
    }

    /// The optional frame features reach the decoder: block checksums, the
    /// content checksum, a declared content size and linked blocks.
    #[test]
    fn a_checksummed_frame_with_a_declared_size_decodes() {
        use lz4_flex::frame::{BlockMode, FrameEncoder, FrameInfo};

        let root = temp_root("checksums");
        let payload = noise(70_000);
        let info = FrameInfo::new()
            .block_mode(BlockMode::Linked)
            .block_checksums(true)
            .content_checksum(true)
            .content_size(Some(payload.len() as u64));
        let mut encoder = FrameEncoder::with_frame_info(info, Vec::new());
        encoder.write_all(&payload).expect("encode payload");
        let compressed = encoder.finish().expect("finish frame");
        write_file(&root, "checksummed.bin", &compressed);

        let engine = project_engine(&root);
        assert_eq!(read(&engine, "lzfs://./checksummed.bin"), payload);

        // A frame that fails one of those checks is "not LZ4": the reference's
        // `LZ4F_decompress` reports the error and `Init` returns 0, so the raw
        // bytes are served. The last four bytes are the content checksum.
        let mut broken = compressed.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        write_file(&root, "broken-checksum.bin", &broken);
        assert_eq!(read(&engine, "lzfs://./broken-checksum.bin"), broken);
    }

    /// `Plugins.link` installs the module a second time (the engine calls
    /// `register` again) and `Plugins.unlink` runs `V2Unlink`'s unregistration;
    /// a second registration must not fail the way a duplicate name would.
    #[test]
    fn linking_again_keeps_one_media_and_unlinking_unregisters_it() {
        let root = temp_root("link");
        let mut engine = project_engine(&root);
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );

        engine
            .execute_expression("inline.tjs", "Plugins.link(\"lzfs.dll\")")
            .expect("link the module again");
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );

        engine
            .execute_expression("inline.tjs", "Plugins.unlink(\"lzfs.dll\")")
            .expect("unlink the module");
        assert!(engine.host().storage_media_names().is_empty());

        // Unlinking forgets the module (`TVPUnloadPlugin`), so linking it back
        // registers the media again — the reference's load/unload cycle.
        engine
            .execute_expression("inline.tjs", "Plugins.link(\"lzfs.dll\")")
            .expect("link the module back");
        assert_eq!(
            engine.host().storage_media_names(),
            vec![MEDIA_NAME.to_string()]
        );
    }

    /// A host without a project storage installs the plugin without a media
    /// and says so, instead of failing the boot (see `register`).
    #[test]
    fn a_host_without_storage_stays_installable() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(LzfsPlugin::new())
            .expect("install into a script-only host");
        assert!(engine.host().storage_media_names().is_empty());
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("lzfs") && line.contains("not registered")),
            "the failed registration must be visible in the log"
        );
    }
}
