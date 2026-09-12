//! krmovie.dll compatibility registration (`docs/plugins/krmovie.md`).
//!
//! The DLL has no TJS surface of its own. The `VideoOverlay` class a game
//! reaches after `Plugins.link("krmovie.dll")` is engine-native — the
//! reference registers it in `VideoOvlIntf.cpp`, this engine in
//! `krkr-engine/src/native/video.rs` — and the DLL is the platform-decoder
//! factory that the engine calls per open (`VideoOvlImpl.cpp:113-200`). Its
//! factories each build one backend object (`krkrz/src/core/movie/win32/`):
//!
//! | export | backend | selected by |
//! |---|---|---|
//! | `GetVideoOverlayObject` | `tTVPDSVideoOverlay` (DirectShow VMR9 / hardware overlay) | `vomOverlay`, and every mode without a factory of its own |
//! | `GetVideoLayerObject` | `tTVPDSLayerVideo` (decode into the engine's two bitmaps) | `vomLayer` |
//! | `GetMixingVideoOverlayObject` | `tTVPDSMixerVideoOverlay` (VMR mixing with the engine canvas) | `vomMixer` |
//! | `GetMFVideoOverlayObject` (not exported by the 2010 build PARQUET ships) | `tTVPMFPlayer` (Media Foundation + EVR) | `vomMFEVR` |
//!
//! So linking the plugin installs nothing: the surface is already there, and
//! what the plugin side owns is the mapping data behind it. Registering this
//! module therefore carries [`KRMOVIE_API_VERSION`] (what `GetAPIVersion`
//! reports), the factory table with the reference's mode selection
//! ([`KrmoviePlugin::factory_for_mode`]) and the `GetOptionDesc` payload
//! ([`KrmoviePlugin::option`]), and *checks* that the engine surface the
//! factories back is complete — a build that lost a member the DLL's contract
//! needs says so in the log instead of letting a scenario fail later on a
//! missing member.
//!
//! # Real vs mapped
//!
//! One decoder path exists: krkr-video's platform backend behind the engine's
//! top-most overlay quad, which is exactly what `vomOverlay` names. The other
//! three modes have no output path of their own here — `vomLayer`'s
//! per-frame bitmaps (`SetVideoBuffer`), `vomMixer`'s
//! `SetMixingBitmap`/`PresentVideoImage` and Media Foundation's EVR — so
//! their movies play through the same quad and the mode-specific rendering
//! does not happen (`native/video.rs` logs the `vomLayer` degradation once);
//! [`MovieOutput::DegradedToOverlayQuad`] is that mapping, stated rather than
//! implied. The plugin interface's frame access
//! (`SetFrame`/`GetFrame`/`GetFrontBuffer`) and its DirectShow event queue
//! (`GetEvent`/`FreeEventParams`) have no counterpart either: the engine
//! delivers `onStatusChanged`/`onPeriod` itself (and never fires
//! `onCallbackCommand`/`onFrameUpdate`). The option descriptor has no
//! consumer — nothing here presents or applies it.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};
use crate::kagexopt::{OptionCategory, OptionDesc, OptionKind, OptionValue};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "krmovie's decoder factories behind the engine-native VideoOverlay (GetVideoOverlayObject / GetVideoLayerObject / GetMixingVideoOverlayObject / GetMFVideoOverlayObject)",
    notes: "Registers krmovie.dll and checks that the engine's VideoOverlay surface its factories back is complete; installs no TJS member, because the reference has none (the class is engine-native, the DLL only builds the decoder that open calls). All four modes run on this engine's one decoder — krkr-video's platform backend presented as the overlay quad, which is exactly vomOverlay; vomLayer (per-frame bitmaps through SetVideoBuffer), vomMixer (SetMixingBitmap/PresentVideoImage) and vomMFEVR (Media Foundation + EVR, not exported by the shipped 2010 DLL anyway) have no output path of their own and play through the same quad. SetFrame/GetFrame/GetFrontBuffer and the GetEvent queue have no counterpart; the engine fires onStatusChanged/onPeriod itself. The GetOptionDesc payload (movie_reg_rot) is carried as data and has no consumer. The factory/backend mapping, the API version and the descriptor are Rust API on KrmoviePlugin; nothing here fakes playback the engine cannot deliver.",
    install: |engine| engine.register_plugin(KrmoviePlugin),
};

pub struct KrmoviePlugin;

impl KrkrPlugin for KrmoviePlugin {
    fn name(&self) -> &str {
        "krmovie.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_krmovie(runtime);
        Ok(())
    }
}

/// `TVP_KRMOVIE_VER` (`krkrz/src/core/visual/win32/krmovie.h:16`): the value
/// the DLL's `GetAPIVersion` export stores into its argument
/// (`krkr2/.../core/visual/win32/krmovie/krmovie.cpp:56-61`, exported through
/// `krmovie.def`). Nothing in this port gates on the number; it is carried
/// because it is part of the DLL's exported contract and what a later
/// version check would read.
pub const KRMOVIE_API_VERSION: u32 = 0x0001_000C;

/// The `tTVPVideoOverlayMode` values the engine publishes as the globals
/// `vomOverlay` … `vomMFEVR`
/// (`krkr-engine/src/globals/constants.rs:211-215`): what `VideoOverlay.mode`
/// holds and what picks a factory (`VideoOvlImpl.cpp:161-174`).
pub const VOM_OVERLAY: i64 = 0;
pub const VOM_LAYER: i64 = 1;
pub const VOM_MIXER: i64 = 2;
pub const VOM_MFEVR: i64 = 3;

/// What this engine delivers for the mode a factory serves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MovieOutput {
    /// The decoded movie presented as the engine's top-most overlay quad —
    /// the one movie output this port has.
    OverlayQuad,
    /// The mode's own output path is absent: the movie still plays, through
    /// the overlay quad, and the rendering the mode names does not happen.
    DegradedToOverlayQuad,
}

/// One factory export of the DLL, with the mode that selects it and what this
/// engine delivers for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MovieFactory {
    /// The export the engine's loader looks up (`krmovie.cpp:33-45`).
    pub export: &'static str,
    /// The backend object that export builds.
    pub backend: &'static str,
    /// The `tTVPVideoOverlayMode` value that selects this factory.
    pub mode: i64,
    /// False for the factory the 2010 build PARQUET ships does not export
    /// (`llvm-objdump --private-headers` lists seven exports, this one not
    /// among them).
    pub shipped: bool,
    /// What this engine delivers for the mode.
    pub output: MovieOutput,
}

/// The DLL's factories, in the order the reference selects them
/// (`VideoOvlImpl.cpp:161-174`). [`KrmoviePlugin::factory_for_mode`] reads the
/// table; the first row is also the reference's default branch.
pub const MOVIE_FACTORIES: &[MovieFactory] = &[
    MovieFactory {
        export: "GetVideoOverlayObject",
        backend: "tTVPDSVideoOverlay (DirectShow VMR9 / hardware overlay)",
        mode: VOM_OVERLAY,
        shipped: true,
        output: MovieOutput::OverlayQuad,
    },
    MovieFactory {
        export: "GetVideoLayerObject",
        backend: "tTVPDSLayerVideo (decode into the engine's two bitmaps)",
        mode: VOM_LAYER,
        shipped: true,
        output: MovieOutput::DegradedToOverlayQuad,
    },
    MovieFactory {
        export: "GetMixingVideoOverlayObject",
        backend: "tTVPDSMixerVideoOverlay (VMR mixing with the engine canvas)",
        mode: VOM_MIXER,
        shipped: true,
        output: MovieOutput::DegradedToOverlayQuad,
    },
    MovieFactory {
        export: "GetMFVideoOverlayObject",
        backend: "tTVPMFPlayer (Media Foundation + EVR)",
        mode: VOM_MFEVR,
        shipped: false,
        output: MovieOutput::DegradedToOverlayQuad,
    },
];

/// The DLL's `GetOptionDesc` payload, read out of the shipped binary the way
/// `docs/plugins/kagexopt.md` reads kagexopt's `IDR_OPTION_DESC_JSON`: one
/// category with one option. The reference engine hands a chosen value to the
/// game as `-<name>=<value>`; nothing here presents or applies it yet, which
/// is why the option is data rather than behaviour.
pub const OPTION_CATEGORIES: &[OptionCategory] = &[OptionCategory {
    name: "デバッグ",
    options: &[OptionDesc {
        name: "movie_reg_rot",
        kind: OptionKind::Select,
        user: false,
        values: &[
            OptionValue {
                value: Some("no"),
                desc: Some("いいえ"),
                default: true,
            },
            OptionValue {
                value: Some("yes"),
                desc: Some("はい"),
                default: false,
            },
            OptionValue {
                value: Some("pause"),
                desc: Some("ポーズ"),
                default: false,
            },
        ],
    }],
}];

/// The DLL's contract, reached through the plugin type because that is the
/// name `lib.rs` re-exports (the `kagexopt` registry is reached the same way).
impl KrmoviePlugin {
    /// What the DLL's `GetAPIVersion` reports.
    pub fn api_version() -> u32 {
        KRMOVIE_API_VERSION
    }

    /// The DLL's factory table, in the reference's selection order.
    pub fn factories() -> &'static [MovieFactory] {
        MOVIE_FACTORIES
    }

    /// The factory the reference engine loads for `mode`
    /// (`VideoOvlImpl.cpp:161-174`): the row whose `mode` matches, or the
    /// first row — `GetVideoOverlayObject` — for a mode the table does not
    /// list, which is the reference's default branch (`vomOverlay` and every
    /// unknown value).
    pub fn factory_for_mode(mode: i64) -> &'static MovieFactory {
        MOVIE_FACTORIES
            .iter()
            .find(|factory| factory.mode == mode)
            .unwrap_or(&MOVIE_FACTORIES[0])
    }

    /// The descriptor named `name` from the DLL's embedded JSON.
    pub fn option(name: &str) -> Option<&'static OptionDesc> {
        OPTION_CATEGORIES
            .iter()
            .flat_map(|category| category.options)
            .find(|option| option.name == name)
    }

    /// The DLL's descriptor registry, by category.
    pub fn options() -> &'static [OptionCategory] {
        OPTION_CATEGORIES
    }
}

/// Installs what linking `krmovie.dll` owes the game: nothing in TJS — the
/// reference plugin has no surface — but a check that the engine's
/// `VideoOverlay` is the surface the DLL's factories back. A build that lost a
/// member reports the gap here, where a human reads it at link time, instead
/// of a scenario stopping later on a member the movie code forwards to.
fn install_krmovie(runtime: &mut Runtime<KrkrHost>) {
    let missing = missing_overlay_surface(runtime);
    if missing.is_empty() {
        return;
    }
    runtime.host_mut().log(&format!(
        "krmovie.dll: the engine's VideoOverlay surface is incomplete ({}); the plugin's decoder \
         factories cannot supply TJS members",
        missing.join(", ")
    ));
}

/// The engine members the DLL's contract needs and that are absent from the
/// live surface, in a fixed order: the `vom*` globals that select a factory,
/// then the class object's methods.
fn missing_overlay_surface(runtime: &Runtime<KrkrHost>) -> Vec<&'static str> {
    let mut missing = Vec::new();
    for (name, value) in [
        ("vomOverlay", VOM_OVERLAY),
        ("vomLayer", VOM_LAYER),
        ("vomMixer", VOM_MIXER),
        ("vomMFEVR", VOM_MFEVR),
    ] {
        if !matches!(runtime.global_member(name), Variant::Integer(found) if found == value) {
            missing.push(name);
        }
    }
    match runtime.global_member("VideoOverlay").object_handle() {
        Some(class) => missing.extend(
            VIDEO_OVERLAY_METHODS
                .iter()
                .copied()
                .filter(|method| matches!(runtime.object_member(class, method), Variant::Void)),
        ),
        None => missing.push("VideoOverlay"),
    }
    missing
}

/// The engine `VideoOverlay` methods the DLL's contract is reached through
/// (`VideoOvlIntf.cpp:219-427`, installed by
/// `krkr-engine/src/native/video.rs:1005-1035`). A game's `Movie` extends
/// `VideoOverlay` and forwards `SUPER.*()` to these, so a missing one stops a
/// scenario. The list is a copy of the engine's own member set — the class
/// spec is private to `krkr-engine` — and the module's tests hold the live
/// surface against it, so a rename on either side fails instead of drifting.
///
/// The four event entry points
/// (`onStatusChanged`/`onCallbackCommand`/`onPeriod`/`onFrameUpdate`) are
/// handlers the engine delivers to rather than class members it provides, so
/// they are not checked here.
const VIDEO_OVERLAY_METHODS: &[&str] = &[
    "open",
    "play",
    "stop",
    "close",
    "setPos",
    "setSize",
    "setBounds",
    "pause",
    "rewind",
    "prepare",
    "setSegmentLoop",
    "cancelSegmentLoop",
    "setPeriodEvent",
    "cancelPeriodEvent",
    "selectAudioStream",
    "setMixingLayer",
    "resetMixingLayer",
];

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::{
        KRMOVIE_API_VERSION, KrmoviePlugin, MOVIE_FACTORIES, MovieOutput, OPTION_CATEGORIES,
        VOM_LAYER, VOM_MFEVR, VOM_MIXER, VOM_OVERLAY, missing_overlay_surface,
    };
    use crate::catalog::{PluginFamily, PluginStatus, resolve};
    use crate::kagexopt::OptionKind;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(KrmoviePlugin).expect("plugin");
        engine
    }

    fn script(engine: &mut KrkrEngine, source: &str) -> Variant {
        engine
            .execute_script("probe.tjs", source)
            .expect("probe script")
    }

    #[test]
    fn the_catalog_entry_is_this_plugin_and_no_longer_missing() {
        let entry = resolve("krmovie.dll").expect("catalog entry");
        assert_eq!(entry.name, "krmovie.dll");
        assert_eq!(entry.family, PluginFamily::Movie);
        assert!(entry.parquet, "PARQUET ships krmovie.dll");
        assert_eq!(super::META.status, PluginStatus::Shim);
        assert!(!entry.is_placeholder());
    }

    #[test]
    fn registering_links_the_name_without_the_missing_plugin_warning() {
        let engine = engine();
        assert!(
            engine
                .host()
                .linked_plugins()
                .any(|name| name == "krmovie.dll"),
            "the module did not register its name"
        );
        assert!(
            !engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("not implemented: krmovie.dll")),
            "an implemented module must not report itself as missing"
        );
        assert!(
            !engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("krmovie.dll")),
            "a complete surface must not need the gap report: {:?}",
            engine.host().logs()
        );
    }

    /// The module's whole claim is that `Plugins.link("krmovie.dll")` reaches
    /// a complete engine `VideoOverlay`; the check the registration runs must
    /// find nothing missing on this build.
    #[test]
    fn the_engine_overlay_surface_the_factories_back_is_complete() {
        let engine = engine();
        assert_eq!(
            missing_overlay_surface(engine.tjs_runtime()),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn linking_krmovie_reaches_the_engine_overlay_with_its_defaults() {
        let mut engine = engine();
        let value = script(
            &mut engine,
            r#"
            Plugins.link("krmovie.dll");
            var overlay = new VideoOverlay();
            return overlay.mode + "|" + overlay.audioVolume + "|" + overlay.mixingMovieAlpha;
            "#,
        );
        assert_eq!(value, Variant::String(format!("{VOM_OVERLAY}|100000|255")));
    }

    #[test]
    fn the_engine_publishes_the_mode_values_the_factory_table_names() {
        let mut engine = engine();
        let value = script(
            &mut engine,
            "return vomOverlay + '|' + vomLayer + '|' + vomMixer + '|' + vomMFEVR;",
        );
        let expected = [VOM_OVERLAY, VOM_LAYER, VOM_MIXER, VOM_MFEVR].map(|mode| mode.to_string());
        assert_eq!(value, Variant::String(expected.join("|")));
    }

    #[test]
    fn factory_selection_follows_the_reference_mode_table() {
        assert_eq!(
            KrmoviePlugin::factory_for_mode(VOM_OVERLAY).export,
            "GetVideoOverlayObject"
        );
        assert_eq!(
            KrmoviePlugin::factory_for_mode(VOM_LAYER).export,
            "GetVideoLayerObject"
        );
        assert_eq!(
            KrmoviePlugin::factory_for_mode(VOM_MIXER).export,
            "GetMixingVideoOverlayObject"
        );
        assert_eq!(
            KrmoviePlugin::factory_for_mode(VOM_MFEVR).export,
            "GetMFVideoOverlayObject"
        );
        // The reference's default branch: a mode without a factory of its own
        // gets the overlay object.
        for mode in [7, -1, i64::MAX] {
            assert_eq!(
                KrmoviePlugin::factory_for_mode(mode).export,
                "GetVideoOverlayObject",
                "mode {mode}"
            );
        }
    }

    /// The honest split the `META` states: one mode is delivered, the other
    /// three are mapped onto it, and the Media Foundation factory is not in
    /// the binary PARQUET ships.
    #[test]
    fn one_mode_is_delivered_and_the_other_three_degrade_to_the_overlay_quad() {
        assert_eq!(
            KrmoviePlugin::factory_for_mode(VOM_OVERLAY).output,
            MovieOutput::OverlayQuad
        );
        for mode in [VOM_LAYER, VOM_MIXER, VOM_MFEVR] {
            let factory = KrmoviePlugin::factory_for_mode(mode);
            assert_eq!(
                factory.output,
                MovieOutput::DegradedToOverlayQuad,
                "{}",
                factory.export
            );
        }
        assert!(!KrmoviePlugin::factory_for_mode(VOM_MFEVR).shipped);
        assert_eq!(MOVIE_FACTORIES.len(), 4);
        assert_eq!(
            MOVIE_FACTORIES
                .iter()
                .filter(|factory| factory.shipped)
                .count(),
            3
        );
    }

    #[test]
    fn the_api_version_is_the_recovered_tvp_krmovie_ver() {
        assert_eq!(KRMOVIE_API_VERSION, 0x0001_000C);
        assert_eq!(KrmoviePlugin::api_version(), KRMOVIE_API_VERSION);
    }

    #[test]
    fn the_option_payload_is_the_recovered_movie_reg_rot_descriptor() {
        assert_eq!(KrmoviePlugin::options(), OPTION_CATEGORIES);
        assert_eq!(OPTION_CATEGORIES.len(), 1);
        assert_eq!(OPTION_CATEGORIES[0].name, "デバッグ");
        let option = KrmoviePlugin::option("movie_reg_rot").expect("movie_reg_rot");
        assert_eq!(option.kind, OptionKind::Select);
        assert!(!option.user, "the descriptor marks the option user: false");
        assert_eq!(
            option.default_value().and_then(|value| value.value),
            Some("no")
        );
        assert_eq!(
            option.value("yes").and_then(|value| value.desc),
            Some("はい")
        );
        assert_eq!(
            option.value("pause").and_then(|value| value.desc),
            Some("ポーズ")
        );
        assert_eq!(
            option.command_line_argument("yes").as_deref(),
            Some("-movie_reg_rot=yes")
        );
        assert_eq!(option.command_line_argument("vomstyle"), None);
        assert!(KrmoviePlugin::option("vomstyle").is_none());
    }

    /// The DLL's own interface members (`SetFrame`, `GetFrontBuffer`, the
    /// `GetEvent` queue) are COM-side calls the engine makes, never TJS
    /// members; nothing in this module fabricates a surface the reference
    /// class does not have.
    #[test]
    fn no_plugin_interface_member_is_fabricated_as_a_tjs_member() {
        let engine = engine();
        let runtime = engine.tjs_runtime();
        let class = runtime
            .global_member("VideoOverlay")
            .object_handle()
            .expect("VideoOverlay class object");
        for name in [
            "SetFrame",
            "GetFrame",
            "SetVideoBuffer",
            "GetFrontBuffer",
            "GetEvent",
            "FreeEventParams",
            "GetAPIVersion",
        ] {
            assert!(
                matches!(runtime.object_member(class, name), Variant::Void),
                "VideoOverlay.{name} was fabricated"
            );
        }
    }

    /// A movie that is not in storage must fail `open` where a game can catch
    /// it: `Movie.tjs` implementations branch on that failure, and the
    /// reference's factory path raises there too. The browser build hands the
    /// storage to a media element instead of reading it, so it is excluded.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn opening_a_missing_movie_raises_where_a_game_can_catch_it() {
        let storage = krkr_assets::ProjectStorage::from_memory(std::iter::empty::<(&str, &[u8])>());
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(KrmoviePlugin).expect("plugin");
        let error = engine
            .execute_script(
                "probe.tjs",
                r#"
                var overlay = new VideoOverlay();
                overlay.open("no_such_movie.wmv");
                "#,
            )
            .expect_err("a movie that is not in storage must fail open");
        assert!(
            error.message.contains("no_such_movie.wmv"),
            "the failure does not name the storage: {}",
            error.message
        );
    }
}
