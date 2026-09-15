//! Graphic-loader registration for plugins (`TVPRegisterGraphicLoadingHandler`).
//!
//! The reference's graphic load path is extension-dispatched: `TVPLoadGraphic`
//! (`visual/GraphicsLoaderIntf.cpp:1672`) normalizes the storage name, checks
//! the graphic cache and otherwise calls `TVPInternalLoadGraphic` (`:1452`),
//! which extracts the name's extension (`TVPExtractStorageExt`, `:1487`) and
//! looks it up in the handler table built by the constructor (`:61-93`) plus
//! `TVPRegisterGraphicLoadingHandler` (`:142`, plugin-facing declaration
//! `visual/GraphicsLoaderIntf.h:130`). An extension nobody claims throws
//! `TVPUnknownGraphicFormat` — "The image format could not be determined" —
//! and a claimed one hands the handler a `tTVPBaseBitmap` it fills through the
//! size/scanline callbacks (`tTVPGraphicLoadingHandlerForPlugin`,
//! `GraphicsLoaderIntf.h:115-123`; invoked through
//! `tTVPGraphicHandlerType::Load`, `visual/win32/GraphicsLoaderImpl.cpp:26`).
//! The engine owns the bitmap and its lifetime; the plugin only writes pixels.
//!
//! That is the seam the E-mote drivers use to make a `.mtn` motion usable where
//! a script asks for an image: `Layer.loadImages` (`visual/LayerIntf.cpp:2494`)
//! and `tTVPBaseBitmap::Load` (`visual/BitmapIntf.cpp:92`) both go through
//! `TVPLoadGraphic`, so a plugin that claims `.mtn` answers them and the layer
//! gets a real bitmap instead of the format error. The reference unregisters
//! around module unlink (`GraphicsLoaderIntf.cpp:170`).
//!
//! This module is our counterpart, with one addition the reference does not
//! need spelled out in its C++ (its handler keeps the layer's bitmap alive and
//! can render into it whenever it likes): a loader may return a **live**
//! graphic, a [`LiveGraphic`] the engine asks for a fresh frame on every frame
//! tick. That is how an animated motion stays animated once it is a layer
//! image — [`crate::KrkrHost::tick_live_graphics`] replaces the pixels of every
//! layer showing that image, which is the same thing the reference's handler
//! does when it draws into the bitmap it was handed.
//!
//! Call these from `KrkrPlugin::register`/`KrkrPlugin::unregister`:
//!
//! ```ignore
//! fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
//!     let loader = self
//!         .loader
//!         .get_or_init(|| Arc::new(MotionGraphicLoader) as Arc<dyn GraphicLoader>);
//!     plugin_api::graphic::register_graphic_loader(runtime, Arc::clone(loader))
//! }
//!
//! fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
//!     plugin_api::graphic::unregister_graphic_loader(runtime, "motionplayer.dll");
//!     Ok(())
//! }
//! ```
//!
//! Two lifecycle rules mirror the storage-media registry
//! (`plugin_api::storage`): `register` runs at boot and again on the first
//! `Plugins.link`, so a plugin must hand back the **same `Arc`** (the registry
//! treats an identical `Arc` as a no-op and rejects a different loader claiming
//! an extension that is already claimed), and `unregister` drops every live
//! graphic the loader produced.
//!
//! What is engine-owned here: the registry, the extension split and
//! lowercasing, the dispatch, the storage read, the image cache entry, the
//! texture id, the layer updates and the live-graphic lifecycle. What is the
//! plugin's: its container format, the decode, the error text and — for live
//! graphics — the clock state inside its own [`LiveGraphic`].

use std::sync::Arc;
use std::time::Duration;

use krkr_tjs2::{Result, runtime::Runtime};

use crate::host::KrkrHost;

/// One RGBA8 bitmap a loader produced, row-major from the top-left.
///
/// The reference's handler writes the same pixels through the size/scanline
/// callbacks; the engine turns them into a [`krkr_core::LayerImage`] and owns
/// them from then on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphicFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl GraphicFrame {
    /// A transparent frame of the given size. A loader that only wants to
    /// paint part of its frame (an empty region, a background it draws itself)
    /// starts here.
    pub fn transparent(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            rgba: vec![0; width as usize * height as usize * 4],
        }
    }
}

/// The engine's request to one loader: the storage name the script asked for
/// (as written, before any extension suggestion) and the bytes resolved for it.
pub struct GraphicSource<'a> {
    pub storage: &'a str,
    pub bytes: &'a [u8],
}

/// A graphic whose pixels change over time — an animated motion used as a
/// layer image.
///
/// The engine calls [`LiveGraphic::frame`] once per frame tick and swaps the
/// pixels of every layer showing the image; `None` means "nothing changed",
/// and the engine leaves the layer alone. The handle is dropped — and the
/// plugin's state with it — when the image leaves the cache or the loader is
/// unregistered.
pub trait LiveGraphic: Send + Sync {
    /// The frame for `elapsed` since the load, or `None` when the current one
    /// still stands.
    fn frame(&self, elapsed: Duration) -> Option<GraphicFrame>;
}

/// What a loader hands back for one storage name.
pub struct LoadedGraphic {
    /// The bitmap the load resolves to. The script-visible load returns once
    /// this frame is in place, exactly like the reference's in-call load.
    pub frame: GraphicFrame,
    /// Set when the graphic keeps animating: the engine ticks it with the
    /// frame clock and re-uploads the layers showing it.
    pub live: Option<Arc<dyn LiveGraphic>>,
}

impl LoadedGraphic {
    /// A static graphic — the plain reference case.
    pub fn still(frame: GraphicFrame) -> Self {
        Self { frame, live: None }
    }
}

/// A plugin's claim on one or more storage extensions.
///
/// `extensions` are compared against the storage name's extension, lowercased
/// and dot-included (`".mtn"`, `".psb"`), the way the reference's hash is keyed
/// (`GraphicsLoaderIntf.cpp:61`, `TVPExtractStorageExt`).
pub trait GraphicLoader: Send + Sync {
    /// The module the loader belongs to, for diagnostics and unregistration.
    fn name(&self) -> &str;

    /// The extensions this loader answers. Never empty; an extension claimed
    /// twice is a registration error, not a silent shadow.
    fn extensions(&self) -> &[&str];

    /// Decodes `source` into a graphic. The error text reaches the script as
    /// the load's failure reason (the reference lets the handler's own
    /// exception propagate), so it should say what the loader could not do.
    fn load(&self, source: GraphicSource<'_>) -> std::result::Result<LoadedGraphic, String>;
}

/// Registers `loader` — the counterpart of `TVPRegisterGraphicLoadingHandler`
/// (`GraphicsLoaderIntf.cpp:142`).
///
/// Idempotent for the same `Arc` (the engine runs every plugin's `register`
/// twice); a *different* loader claiming an extension already claimed fails
/// with both module names, so a genuine conflict is visible.
pub fn register_graphic_loader(
    runtime: &mut Runtime<KrkrHost>,
    loader: Arc<dyn GraphicLoader>,
) -> Result<()> {
    runtime.host_mut().register_graphic_loader(loader)
}

/// Unregisters the loader registered under module name `name`, dropping every
/// live graphic it produced — the counterpart of
/// `TVPUnregisterGraphicLoadingHandler` (`GraphicsLoaderIntf.cpp:170`).
/// Returns whether a loader was registered; the name is matched exactly.
pub fn unregister_graphic_loader(runtime: &mut Runtime<KrkrHost>, name: &str) -> bool {
    runtime.host_mut().unregister_graphic_loader(name)
}

/// The registered loaders' module names, sorted. Diagnostics and tests only.
pub fn graphic_loader_names(runtime: &Runtime<KrkrHost>) -> Vec<String> {
    runtime.host().graphic_loader_names()
}

/// The extensions the registered loaders claim, sorted and deduplicated. The
/// engine uses this to decide whether a storage name takes the plugin path
/// before it tries the built-in decoders.
pub fn claimed_graphic_extensions(runtime: &Runtime<KrkrHost>) -> Vec<String> {
    runtime.host().claimed_graphic_extensions()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicU32, AtomicU64, Ordering},
        },
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use krkr_assets::ProjectStorage;
    use krkr_core::{FrameInput, Size};
    use krkr_tjs2::runtime::Variant;

    use crate::{EngineConfig, EngineInput, KrkrEngine, SystemPaths, host::storage_extension};

    use super::*;

    /// A loader that claims `.fake`: 3x2, with the number of frames produced so
    /// far in the top-left pixel's red channel, and a live graphic when asked
    /// for one. The count lives in one `Arc<AtomicU32>` shared with the live
    /// handle, so the test can assert on what the loader actually produced.
    struct FakeLoader {
        live: bool,
        frames: Arc<AtomicU32>,
    }

    struct FakeLive {
        frames: Arc<AtomicU32>,
    }

    impl LiveGraphic for FakeLive {
        fn frame(&self, elapsed: Duration) -> Option<GraphicFrame> {
            // One frame per second of host time, the load itself being frame 1.
            let wanted = (elapsed.as_millis() / 1000) as u32 + 1;
            let produced = self.frames.load(Ordering::SeqCst);
            if wanted <= produced {
                return None;
            }
            self.frames.store(wanted, Ordering::SeqCst);
            Some(fake_frame(wanted))
        }
    }

    fn fake_frame(index: u32) -> GraphicFrame {
        let mut frame = GraphicFrame::transparent(3, 2);
        frame.rgba[..4].copy_from_slice(&[index as u8, 0, 0, 255]);
        frame
    }

    impl GraphicLoader for FakeLoader {
        fn name(&self) -> &str {
            "fake.dll"
        }

        fn extensions(&self) -> &[&str] {
            &[".fake"]
        }

        fn load(&self, source: GraphicSource<'_>) -> std::result::Result<LoadedGraphic, String> {
            assert_eq!(source.bytes, b"fake-bytes");
            self.frames.store(1, Ordering::SeqCst);
            Ok(LoadedGraphic {
                frame: fake_frame(1),
                live: self.live.then(|| {
                    Arc::new(FakeLive {
                        frames: Arc::clone(&self.frames),
                    }) as Arc<dyn LiveGraphic>
                }),
            })
        }
    }

    fn fake_loader(live: bool) -> Arc<FakeLoader> {
        Arc::new(FakeLoader {
            live,
            frames: Arc::new(AtomicU32::new(0)),
        })
    }

    fn temp_root(tag: &str) -> PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "Kirakira-graphic-{tag}-{}-{nanos}-{id}",
            std::process::id()
        ))
    }

    fn engine_with_root(tag: &str) -> (KrkrEngine, PathBuf) {
        let root = temp_root(tag);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("art.fake"), b"fake-bytes").expect("write storage entry");
        let storage = ProjectStorage::for_root(&root).expect("storage");
        let engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths::default(),
            ..EngineConfig::default()
        })
        .expect("engine");
        (engine, root)
    }

    fn layer_script() -> &'static str {
        r#"
        var layer = new Layer(0, 0, 3, 2);
        layer.loadImages("art.fake");
        "#
    }

    fn main_pixel(engine: &mut KrkrEngine) -> i64 {
        match engine
            .execute_expression("read.tjs", "layer.getMainPixel(0, 0)")
            .expect("main pixel")
        {
            Variant::Integer(value) => value,
            other => panic!("getMainPixel returned {other:?}"),
        }
    }

    fn tick(engine: &mut KrkrEngine, delta: Duration) {
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                delta,
            )
            .expect("frame");
    }

    fn register(engine: &mut KrkrEngine, loader: Arc<dyn GraphicLoader>) {
        register_graphic_loader(engine.tjs_runtime_mut(), loader).expect("register");
    }

    /// `Layer.loadImages` of a claimed extension resolves through the plugin
    /// loader, not the built-in decoders: the script sees a bitmap of the
    /// loader's size and pixels.
    #[test]
    fn a_claimed_extension_loads_through_the_plugin() {
        let (mut engine, root) = engine_with_root("claimed");
        register(&mut engine, fake_loader(false) as Arc<dyn GraphicLoader>);

        engine
            .execute_script("load.tjs", layer_script())
            .expect("load");
        assert_eq!(
            engine
                .execute_expression("size.tjs", "layer.imageWidth + \"x\" + layer.imageHeight")
                .expect("size"),
            Variant::String("3x2".to_string()),
        );
        assert_eq!(main_pixel(&mut engine), 1 << 16);
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// A name nobody claims keeps the built-in path, and its error: the honest
    /// "The image format could not be determined" the decoder raises.
    #[test]
    fn an_unclaimed_extension_keeps_the_built_in_format_error() {
        let (mut engine, root) = engine_with_root("unclaimed");
        register(&mut engine, fake_loader(false) as Arc<dyn GraphicLoader>);
        fs::write(root.join("mystery.unknown"), b"not an image").expect("write unknown");

        let error = engine
            .execute_script(
                "load.tjs",
                r#"
                var layer = new Layer(0, 0, 3, 2);
                layer.loadImages("mystery.unknown");
                "#,
            )
            .expect_err("an unclaimed format fails");
        let message = error.to_string();
        assert!(
            message.contains("The image format could not be determined"),
            "unexpected error: {message}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// A live graphic keeps the layer's image moving: the engine ticks the
    /// loader once per frame and the layer reads the new pixels.
    #[test]
    fn a_live_graphic_refreshes_the_layer_every_frame() {
        let (mut engine, root) = engine_with_root("live");
        let loader = fake_loader(true);
        register(&mut engine, Arc::clone(&loader) as Arc<dyn GraphicLoader>);
        engine
            .execute_script("load.tjs", layer_script())
            .expect("load");
        assert_eq!(main_pixel(&mut engine), 1 << 16, "the load's own frame");

        tick(&mut engine, Duration::from_secs(1));
        assert_eq!(
            loader.frames.load(Ordering::SeqCst),
            2,
            "the engine asked the live graphic for a frame per tick"
        );
        assert_eq!(main_pixel(&mut engine), 2 << 16, "one frame later");

        // The loader produces nothing for a repeat of the same tick, so the
        // layer keeps what it has.
        tick(&mut engine, Duration::from_millis(200));
        assert_eq!(main_pixel(&mut engine), 2 << 16);

        // 6.2 s of host time in: the loader's own counter is one ahead of the
        // whole seconds elapsed, the load being frame 1.
        tick(&mut engine, Duration::from_secs(5));
        assert_eq!(main_pixel(&mut engine), 7 << 16, "6.2 s in");
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// A second loader cannot claim an extension that is already claimed; the
    /// engine's double `register` of the *same* loader is a no-op.
    #[test]
    fn extension_claims_are_exclusive_and_repeat_registration_is_idempotent() {
        let (mut engine, root) = engine_with_root("claims");
        let loader = fake_loader(false);
        register(&mut engine, Arc::clone(&loader) as Arc<dyn GraphicLoader>);
        register(&mut engine, Arc::clone(&loader) as Arc<dyn GraphicLoader>);
        assert_eq!(graphic_loader_names(engine.tjs_runtime()), vec!["fake.dll"]);
        assert_eq!(
            claimed_graphic_extensions(engine.tjs_runtime()),
            vec![".fake".to_string()],
        );

        let error = register_graphic_loader(engine.tjs_runtime_mut(), fake_loader(false))
            .expect_err("a second claim fails");
        assert!(
            error.to_string().contains("already claimed by `fake.dll`"),
            "unexpected error: {error}"
        );

        // Unregistering drops the claim, and the load falls back to the
        // built-in decoder.
        assert!(unregister_graphic_loader(
            engine.tjs_runtime_mut(),
            "fake.dll"
        ));
        assert!(!unregister_graphic_loader(
            engine.tjs_runtime_mut(),
            "fake.dll"
        ));
        assert!(graphic_loader_names(engine.tjs_runtime()).is_empty());
        let error = engine
            .execute_script("load.tjs", layer_script())
            .expect_err("no loader claims it any more");
        assert!(
            error
                .to_string()
                .contains("The image format could not be determined"),
            "unexpected error: {error}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// A live graphic dies with its loader: unregistering drops the handle and
    /// the frames stop.
    #[test]
    fn unregistering_drops_the_live_graphics_of_that_loader() {
        let (mut engine, root) = engine_with_root("live-drop");
        register(&mut engine, fake_loader(true) as Arc<dyn GraphicLoader>);
        engine
            .execute_script("load.tjs", layer_script())
            .expect("load");
        tick(&mut engine, Duration::from_secs(1));
        assert_eq!(main_pixel(&mut engine), 2 << 16);

        unregister_graphic_loader(engine.tjs_runtime_mut(), "fake.dll");
        tick(&mut engine, Duration::from_secs(5));
        assert_eq!(
            main_pixel(&mut engine),
            2 << 16,
            "the dropped graphic stops producing frames"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The extension split looks at the storage name's last segment, so a
    /// qualified name (`motion/title_bg.mtn`) or an archive member
    /// (`data.xp3>title_bg.mtn`) still resolves to the extension.
    #[test]
    fn storage_extension_reads_the_last_segment() {
        assert_eq!(
            storage_extension("motion/title_bg.mtn").as_deref(),
            Some(".mtn")
        );
        assert_eq!(storage_extension("title_bg.MTN").as_deref(), Some(".mtn"));
        assert_eq!(
            storage_extension("psb://container.psb/entry").as_deref(),
            None
        );
        assert_eq!(
            storage_extension("data.xp3>motion/title_bg.mtn").as_deref(),
            Some(".mtn")
        );
        assert_eq!(storage_extension("noextension").as_deref(), None);
    }
}
