//! Catalog of every plugin name this engine knows about, in one table.
//!
//! The engine has no DLL loader: a plugin is emulated by a Rust module that
//! registers the same TJS surface, and a game's `Plugins.link("foo.dll")`
//! resolves against the names registered here. That makes the *name* part of
//! the contract — a plugin that is registered under a spelling the game does
//! not use is a plugin the game silently does not get.
//!
//! [`CATALOG`] therefore lists every plugin identity with its canonical
//! `name` (what `KrkrPlugin::name` reports and what a profile selects), the
//! other spellings it is known by (`aliases`), the engine area it hooks
//! (`family`), how much of it exists today (`status`), where its behaviour is
//! documented (`source`), and whether PARQUET ships the DLL (`parquet`).
//! Name resolution ([`resolve`], [`canonical_name`], [`is_same_plugin`]) is
//! case-insensitive and covers aliases, so profiles and hosts can select a
//! plugin by any spelling. [`GIST_PLUGIN_NAMES`] and [`PARQUET_PLUGIN_FILES`]
//! are the two external name lists this catalog was built from, and the tests
//! at the bottom of this file hold it to them.
//!
//! # Implementing a plugin
//!
//! A [`PluginStatus::Missing`] entry has a module under `src/<plugin>.rs`
//! whose body is a `placeholder_plugin!` invocation: it installs no TJS
//! surface and reports itself through the engine log. To implement it, replace
//! that invocation with a real `KrkrPlugin` impl and update the entry's
//! `status`/`notes` here.

use krkr_engine::{KrkrEngine, KrkrHost};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::{
    AddFontPlugin, AlphaMoviePlugin, CsvParserPlugin, DirlistPlugin, DmmCloudPlugin,
    DrawDeviceD3DPlugin, DrawDeviceD3DZPlugin, EmotePlayerPlugin, ExpatPlugin, ExtKagParserPlugin,
    ExtNaganoPlugin, ExtransPlugin, FftGraphPlugin, FstatPlugin, GamepadPlugin, GetAboutPlugin,
    GetLangNamePlugin, GetSamplePlugin, GfxEffectPlugin, GlitchEffectPlugin, HttpRequestPlugin,
    JsonPlugin, K2CompatPlugin, KagParserExPlugin, KagParserExbPlugin, KagexOptPlugin,
    KaichoTransPlugin, Kirikiroid2Plugin, KrkrSteamPlugin, KrmoviePlugin, LayerExAlphaPlugin,
    LayerExAreaAveragePlugin, LayerExBtoaPlugin, LayerExDrawPlugin, LayerExImagePlugin,
    LayerExMoviePlugin, LayerExRasterPlugin, LayerExSavePlugin, LayerExShimmerPlugin, LzfsPlugin,
    MenuPlugin, MinizipPlugin, MotionPlayerPlugin, MultiImagePlugin, PackinOnePlugin,
    PerspectivePlugin, PluginMapping, PsbFilePlugin, PsdPlugin, SaveStructPlugin, ScriptsExPlugin,
    ShrinkCopyPlugin, Sqlite3Plugin, SteamDrawDevicePlugin, TextRenderPlugin, UtilGenericPlugin,
    UtilGraphPlugin, UtilSystemPlugin, VarfilePlugin, WfBasicEffectPlugin, WfTypicalDspPlugin,
    Win32DialogPlugin, Win32OlePlugin, WindowExPlugin, WuOpusPlugin, WuVorbisPlugin, WutcwfPlugin,
    Xp3FilterPlugin, YuzuExPlugin,
};

/// Which part of the engine a plugin hooks — i.e. which code a mission
/// implementing it has to understand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginFamily {
    /// Archives, containers and the storage layer.
    Archive,
    /// Audio decoding, sampling and DSP.
    Audio,
    /// Font registration and metrics.
    Font,
    /// Input devices.
    Input,
    /// Layer image operations and drawing effects.
    Layer,
    /// Movie playback and movie-into-layer drawing.
    Movie,
    /// Draw devices and rendering backends.
    Render,
    /// Script-level helpers: parsers, serialization, storage tools.
    Script,
    /// OS and platform services: dialogs, OLE, network, storefront APIs.
    System,
    /// Text layout and rasterization.
    Text,
    /// Screen transitions.
    Transition,
    /// Not identified yet — see the entry's `notes`.
    Unknown,
}

impl PluginFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginFamily::Archive => "archive",
            PluginFamily::Audio => "audio",
            PluginFamily::Font => "font",
            PluginFamily::Input => "input",
            PluginFamily::Layer => "layer",
            PluginFamily::Movie => "movie",
            PluginFamily::Render => "render",
            PluginFamily::Script => "script",
            PluginFamily::System => "system",
            PluginFamily::Text => "text",
            PluginFamily::Transition => "transition",
            PluginFamily::Unknown => "unknown",
        }
    }
}

/// How much of a plugin this crate implements today.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginStatus {
    /// The parts games call are implemented.
    Implemented,
    /// Surface-compatible only: the members exist, the behaviour behind them
    /// is absent or degraded.
    Shim,
    /// No implementation at all: the module is a self-reporting placeholder
    /// that installs nothing.
    Missing,
}

/// One plugin identity.
#[derive(Clone, Copy, Debug)]
pub struct PluginEntry {
    /// Canonical DLL name: what `KrkrPlugin::name` reports, what
    /// `Plugins.link` matches and what a profile selects.
    pub name: &'static str,
    /// Other spellings of the same plugin seen in the wild.
    pub aliases: &'static [&'static str],
    pub family: PluginFamily,
    pub status: PluginStatus,
    /// Where the plugin's behaviour is documented (upstream source, or the
    /// note that only a binary exists).
    pub source: &'static str,
    /// Whether PARQUET ships the DLL (under this name or an alias).
    pub parquet: bool,
    /// What the plugin extends in the engine — what a later mission builds.
    pub feature: &'static str,
    /// Coverage in this crate today, and what is left to do.
    pub notes: &'static str,
    /// Installs the module. One plugin per entry; see
    /// [`install_plugin`].
    pub install: fn(&mut KrkrEngine) -> Result<()>,
}

impl PluginEntry {
    /// True when the entry's module installs nothing and only reports itself.
    pub fn is_placeholder(&self) -> bool {
        self.status == PluginStatus::Missing
    }

    /// True when `name` names this plugin in any spelling.
    pub fn matches(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
            || self
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    }

    pub fn mapping(&self) -> PluginMapping {
        PluginMapping {
            feature: self.feature,
            provider: crate::PluginOrigin::Plugin,
            plugin_name: Some(self.name),
            notes: self.notes,
        }
    }
}

/// Every plugin identity, in registration order.
///
/// The fifteen entries first are the modules that existed before this catalog
/// and keep their original install order; the rest follow grouped by family.
pub const CATALOG: &[PluginEntry] = &[
    PluginEntry {
        name: "addFont.dll",
        aliases: &["AddFont.dll"],
        family: PluginFamily::Font,
        status: PluginStatus::Implemented,
        source: "https://github.com/wtnbgo/addFont",
        parquet: false,
        feature: "System.addFont",
        notes: "Registers fonts from game storage through the engine font system.",
        install: |engine| engine.register_plugin(AddFontPlugin),
    },
    PluginEntry {
        name: "motionplayer.dll",
        aliases: &[
            "MotionPlayer.dll",
            "motionplayer_nod3d.dll",
            "MotionPlayer_nod3D.dll",
        ],
        family: PluginFamily::Movie,
        status: PluginStatus::Shim,
        source: "(no public source; PARQUET ships motionplayer.dll and the no-D3D build)",
        parquet: true,
        feature: "Motion / Motion.Player / Motion.EmotePlayer",
        notes: "Motion.Player/EmotePlayer constructors exist and report idle/zero state; no motion playback.",
        install: |engine| engine.register_plugin(MotionPlayerPlugin),
    },
    PluginEntry {
        name: "win32dialog.dll",
        aliases: &["Win32Dialog.dll"],
        family: PluginFamily::System,
        status: PluginStatus::Shim,
        source: "https://github.com/wtnbgo/win32dialog",
        parquet: true,
        feature: "WIN32Dialog",
        notes: "No-op dialog classes (WIN32Dialog plus Header/Items/Bitmap/SolidBrush/DrawItem/Notify/Blob) with the constants scripts reference; open() reports immediately.",
        install: |engine| engine.register_plugin(Win32DialogPlugin),
    },
    PluginEntry {
        name: "windowEx.dll",
        aliases: &["WindowEx.dll"],
        family: PluginFamily::System,
        status: PluginStatus::Shim,
        source: "https://github.com/wtnbgo/windowEx",
        parquet: true,
        feature: "Window/MenuItem/Pad/Debug.console/System/Scripts extensions",
        notes: "No-op member surface attached to the engine's existing classes.",
        install: |engine| engine.register_plugin(WindowExPlugin),
    },
    PluginEntry {
        name: "json.dll",
        aliases: &["Json.dll"],
        family: PluginFamily::Script,
        status: PluginStatus::Implemented,
        source: "https://github.com/wtnbgo/json",
        parquet: true,
        feature: "Scripts.evalJSON / evalJSONStorage / saveJSON / toJSONString",
        notes: "Functional lenient JSON parser and serializer.",
        install: |engine| engine.register_plugin(JsonPlugin),
    },
    PluginEntry {
        name: "PackinOne.dll",
        aliases: &["packinone.dll"],
        family: PluginFamily::Script,
        status: PluginStatus::Shim,
        source: "PackinOne bundle (csvParser, scriptsEx, saveStruct, fstat, shrinkCopy, layerEx*, process)",
        parquet: true,
        feature: "CSVParser, Scripts.loadDataPack, Storages.saveOctet, System.getOSVersion, Layer effects",
        notes: "CSVParser, storages octet I/O, URL codecs and Scripts.loadDataPack/clone are functional; the rest of the bundle is no-op surface.",
        install: |engine| engine.register_plugin(PackinOnePlugin),
    },
    PluginEntry {
        name: "layerExDraw.dll",
        aliases: &["LayerExDraw.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Shim,
        source: "https://github.com/wtnbgo/layerExDraw",
        parquet: true,
        feature: "Layer drawing methods / GdiPlus namespace",
        notes: "GdiPlus PointF/RectF/Matrix are functional geometry; draw* methods paint nothing and return a zeroed update RectF.",
        install: |engine| engine.register_plugin(LayerExDrawPlugin),
    },
    PluginEntry {
        name: "textrender.dll",
        aliases: &["TextRender.dll", "textRender.dll"],
        family: PluginFamily::Text,
        status: PluginStatus::Implemented,
        source: "(no public source; TextRenderBase is subclassed by the game's own TextRender.tjs)",
        parquet: true,
        feature: "TextRenderBase",
        notes: "Line layout, character geometry and ruby grouping are implemented; glyph painting goes through Layer.drawText.",
        install: |engine| engine.register_plugin(TextRenderPlugin),
    },
    PluginEntry {
        name: "psbfile.dll",
        aliases: &["PSBFile.dll", "psbFile.dll"],
        family: PluginFamily::Script,
        status: PluginStatus::Implemented,
        source: "(no public source; M2 PSB container format)",
        parquet: true,
        feature: "PSBFile / PSBValueClass",
        notes: "Decodes PSB object trees into TJS dictionaries/arrays/octets and mounts embedded resources.",
        install: |engine| engine.register_plugin(PsbFilePlugin),
    },
    PluginEntry {
        name: "AlphaMovie.dll",
        aliases: &["alphamovie.dll", "nene.dll", "Nene.dll"],
        family: PluginFamily::Movie,
        status: PluginStatus::Shim,
        source: "http://kaede-software.com/krlm/plugin/alphamovie.zip",
        parquet: true,
        feature: "AlphaMovie (NI_AlphaMovie / CMoviePlayer / NI_LayerProxy)",
        notes: "nene.dll is the same binary as AlphaMovie.dll (same build stamp and PDB path per the M24 census), so it is an alias rather than a second plugin. Validates the movie file and reports a finished one-frame movie so polling wrappers terminate; no playback.",
        install: |engine| engine.register_plugin(AlphaMoviePlugin),
    },
    PluginEntry {
        name: "getSample.dll",
        aliases: &["GetSample.dll"],
        family: PluginFamily::Audio,
        status: PluginStatus::Shim,
        source: "https://github.com/wtnbgo/getSample",
        parquet: true,
        feature: "WaveSoundBuffer.getSample / sampleValue / sampleCount / sampleAhead",
        notes: "Reports silence (0 / 0.0) so lip-sync scripts stay idle.",
        install: |engine| engine.register_plugin(GetSamplePlugin),
    },
    PluginEntry {
        name: "KAGParserEx.dll",
        aliases: &["kagparserex.dll"],
        family: PluginFamily::Script,
        status: PluginStatus::Shim,
        source: "https://github.com/wtnbgo/KAGParserEx",
        parquet: true,
        feature: "KAGParser tag dictionaries expose taglist",
        notes: "Marker: the engine already attaches the ordered taglist to every KAG tag dictionary; paramMacros/pmacro/multiLineTagEnabled are not implemented.",
        install: |engine| engine.register_plugin(KagParserExPlugin),
    },
    PluginEntry {
        name: "extrans.dll",
        aliases: &["Extrans.dll"],
        family: PluginFamily::Transition,
        status: PluginStatus::Shim,
        source: "https://github.com/krkrz/SamplePlugin/tree/master/extrans",
        parquet: true,
        feature: "wave / mosaic / turn / rotatezoom / rotatevanish / rotateswap / ripple transitions",
        notes: "Marker: krkr-core degrades these transition names to crossfade.",
        install: |engine| engine.register_plugin(ExtransPlugin),
    },
    PluginEntry {
        name: "extNagano.dll",
        aliases: &["ExtNagano.dll"],
        family: PluginFamily::Transition,
        status: PluginStatus::Shim,
        source: "https://web.archive.org/web/20120604091809fw_/http://ymtkyk.sakura.ne.jp/krkr.STG/plugin/extNagano.html",
        parquet: true,
        feature: "zoomfade / blurfade / scanline / 3duniversal / rgbfade / spin / flutter / imagewipe / book / honeyturn / morphing / multiripple transitions",
        notes: "Marker: krkr-core degrades these transition names to crossfade.",
        install: |engine| engine.register_plugin(ExtNaganoPlugin),
    },
    PluginEntry {
        name: "KaichoTrans.dll",
        aliases: &["kaichotrans.dll"],
        family: PluginFamily::Transition,
        status: PluginStatus::Missing,
        source: "http://keepcreating.g2.xrea.com/krkrplugins/KaichoTrans/KaichoTrans.zip",
        parquet: false,
        feature: "Extra Layer.beginTransition methods (kaicho family)",
        notes: "Not implemented; transition names currently degrade to crossfade in krkr-core, so scripts keep running with the wrong effect.",
        install: |engine| engine.register_plugin(KaichoTransPlugin),
    },
    PluginEntry {
        name: "lzfs.dll",
        aliases: &["Lzfs.dll"],
        family: PluginFamily::Archive,
        status: PluginStatus::Shim,
        source: "(no public source; lzfs archive reader)",
        parquet: true,
        feature: "lzfs archive support",
        notes: "Marker: no TJS surface, and the engine has no lzfs reader yet, so .lzfs archives stay unreadable.",
        install: |engine| engine.register_plugin(LzfsPlugin),
    },
    PluginEntry {
        name: "minizip.dll",
        aliases: &[],
        family: PluginFamily::Archive,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/minizip",
        parquet: false,
        feature: "ZIP archive reading and writing",
        notes: "Not implemented; ZIP support would have to reach the storage layer, not just the script surface.",
        install: |engine| engine.register_plugin(MinizipPlugin),
    },
    PluginEntry {
        name: "varfile.dll",
        aliases: &[],
        family: PluginFamily::Archive,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/varfile",
        parquet: false,
        feature: "Virtual/bundled data files mounted as storages",
        notes: "Not implemented; overlaps the engine's external-resource provision path.",
        install: |engine| engine.register_plugin(VarfilePlugin),
    },
    PluginEntry {
        name: "xp3filter.dll",
        aliases: &["Xp3Filter.dll"],
        family: PluginFamily::Archive,
        status: PluginStatus::Missing,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        feature: "XP3 read filtering",
        notes: "Not implemented; archive filtering would have to happen inside krkr-xp3's read path.",
        install: |engine| engine.register_plugin(Xp3FilterPlugin),
    },
    PluginEntry {
        name: "fftgraph.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/fftgraph",
        parquet: false,
        feature: "FFT spectrum graph of playing audio",
        notes: "Not implemented; the engine already exposes sample data through getSample's stub, not through FFT.",
        install: |engine| engine.register_plugin(FftGraphPlugin),
    },
    PluginEntry {
        name: "wuopus.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "Opus codec registration",
        notes: "Not implemented; the engine decodes Opus in krkr-audio behind the optional `opus` feature, but the plugin's own surface is absent.",
        install: |engine| engine.register_plugin(WuOpusPlugin),
    },
    PluginEntry {
        name: "wuvorbis.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "Vorbis/OGG codec registration",
        notes: "Not implemented; the engine decodes Vorbis through krkr-audio/symphonia, but the plugin's own surface is absent.",
        install: |engine| engine.register_plugin(WuVorbisPlugin),
    },
    PluginEntry {
        name: "gamepad.dll",
        aliases: &[],
        family: PluginFamily::Input,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "Gamepad input",
        notes: "Not implemented; input reaches the engine through krkr-core's input events only.",
        install: |engine| engine.register_plugin(GamepadPlugin),
    },
    PluginEntry {
        name: "GlitchEffect.dll",
        aliases: &["glitcheffect.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "Glitch/CRT layer effect",
        notes: "Not implemented; PARQUET ships it, so its usage should be checked before the effect is built.",
        install: |engine| engine.register_plugin(GlitchEffectPlugin),
    },
    PluginEntry {
        name: "gfxEffect.dll",
        aliases: &[],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "http://kaede-software.com/krlm/plugin/gfx_effect.zip",
        parquet: false,
        feature: "Kaede layer effects (blur/glow family)",
        notes: "Not implemented; the engine's Layer has no effect pipeline to hang these on yet.",
        install: |engine| engine.register_plugin(GfxEffectPlugin),
    },
    PluginEntry {
        name: "layerExAlpha.dll",
        aliases: &["LayerExAlpha.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        feature: "Layer alpha channel operations",
        notes: "Not implemented; alpha extract/replace has no engine-side entry point.",
        install: |engine| engine.register_plugin(LayerExAlphaPlugin),
    },
    PluginEntry {
        name: "layerExAreaAverage.dll",
        aliases: &["LayerExAreaAverage.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/layerExAreaAverage",
        parquet: false,
        feature: "Average colour of a layer image area",
        notes: "Not implemented; needs layer pixel access, which the engine already has internally.",
        install: |engine| engine.register_plugin(LayerExAreaAveragePlugin),
    },
    PluginEntry {
        name: "layerExBTOA.dll",
        aliases: &["LayerExBTOA.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/layerExBTOA",
        parquet: false,
        feature: "Layer image operator/alpha blit",
        notes: "Not implemented; exact member list still to be confirmed from the census.",
        install: |engine| engine.register_plugin(LayerExBtoaPlugin),
    },
    PluginEntry {
        name: "layerExImage.dll",
        aliases: &["LayerExImage.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/layerExImage",
        parquet: false,
        feature: "Layer image load/blit helpers",
        notes: "Not implemented; the two spellings in the plugin list (`LayerExImage.dll`, `layerExImage.dll`) are one plugin.",
        install: |engine| engine.register_plugin(LayerExImagePlugin),
    },
    PluginEntry {
        name: "layerExMovie.dll",
        aliases: &["LayerExMovie.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExMovie",
        parquet: false,
        feature: "Movie drawn into a layer image",
        notes: "Not implemented; would sit on top of the engine's native video decode.",
        install: |engine| engine.register_plugin(LayerExMoviePlugin),
    },
    PluginEntry {
        name: "layerExRaster.dll",
        aliases: &["LayerExRaster.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/layerExRaster",
        parquet: false,
        feature: "Rasterisation (polygon/line) onto a layer image",
        notes: "Not implemented; overlaps the drawing surface layerExDraw still lacks.",
        install: |engine| engine.register_plugin(LayerExRasterPlugin),
    },
    PluginEntry {
        name: "layerExSave.dll",
        aliases: &["LayerExSave.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/layerExSave",
        parquet: false,
        feature: "Save layer images to image files",
        notes: "Not implemented; image encoding has no path from layer pixels yet.",
        install: |engine| engine.register_plugin(LayerExSavePlugin),
    },
    PluginEntry {
        name: "layerExShimmer.dll",
        aliases: &["LayerExShimmer.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "http://keepcreating.g2.xrea.com/krkrplugins/ShimmerPlugin/layerExShimmer.zip",
        parquet: false,
        feature: "Shimmer/hologram layer effect",
        notes: "Not implemented; a shader-backed effect would land in krkr-render.",
        install: |engine| engine.register_plugin(LayerExShimmerPlugin),
    },
    PluginEntry {
        name: "multiimage.dll",
        aliases: &["MultiImage.dll"],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        feature: "Image sheet split into several layer images",
        notes: "Not implemented; sheet splitting is expressible with the engine's existing blits.",
        install: |engine| engine.register_plugin(MultiImagePlugin),
    },
    PluginEntry {
        name: "perspective.dll",
        aliases: &[],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExPerspective",
        parquet: false,
        feature: "Four-corner perspective transform of layer images",
        notes: "Not implemented; krkr-render has no perspective blit yet.",
        install: |engine| engine.register_plugin(PerspectivePlugin),
    },
    PluginEntry {
        name: "psd.dll",
        aliases: &[],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "Photoshop PSD loading",
        notes: "Not implemented; PSD parsing plus layer reconstruction is needed before layer names/opacity can be honoured.",
        install: |engine| engine.register_plugin(PsdPlugin),
    },
    PluginEntry {
        name: "shrinkCopy.dll",
        aliases: &[],
        family: PluginFamily::Layer,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/shrinkCopy",
        parquet: false,
        feature: "Downscaled blit onto layers",
        notes: "Not implemented; the engine's stretch blits cover the pixels, the plugin's own members are absent.",
        install: |engine| engine.register_plugin(ShrinkCopyPlugin),
    },
    PluginEntry {
        name: "emoteplayer.dll",
        aliases: &["EmotePlayer.dll"],
        family: PluginFamily::Movie,
        status: PluginStatus::Missing,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        feature: "EmotePlayer (MPEG emote playback)",
        notes: "Not implemented; the motionplayer shim already installs a Motion.EmotePlayer class, so scripts see the name but not the playback.",
        install: |engine| engine.register_plugin(EmotePlayerPlugin),
    },
    PluginEntry {
        name: "krmovie.dll",
        aliases: &[],
        family: PluginFamily::Movie,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/krkrz",
        parquet: true,
        feature: "MoviePlayer / VideoOverlay",
        notes: "Not implemented; movies already play through the engine's native VideoOverlay (krkr-video), but the plugin's TJS classes are not installed.",
        install: |engine| engine.register_plugin(KrmoviePlugin),
    },
    PluginEntry {
        name: "DrawDeviceD3D.dll",
        aliases: &["drawdeviceD3D.dll"],
        family: PluginFamily::Render,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/krkr2/tree/master/kirikiri2/trunk/kirikiri2/src/plugins/win32/drawdeviceD3D",
        parquet: false,
        feature: "Direct3D draw device",
        notes: "Not implemented; Kirakira renders through wgpu, so the draw-device surface would be a compatibility marker.",
        install: |engine| engine.register_plugin(DrawDeviceD3DPlugin),
    },
    PluginEntry {
        name: "DrawDeviceD3DZ.dll",
        aliases: &[],
        family: PluginFamily::Render,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/drawdeviceD3D",
        parquet: false,
        feature: "krkrz Direct3D draw device",
        notes: "Not implemented; same as DrawDeviceD3D.dll — the engine's renderer replaces it.",
        install: |engine| engine.register_plugin(DrawDeviceD3DZPlugin),
    },
    PluginEntry {
        name: "SteamDrawDevice.dll",
        aliases: &["steamdrawdevice.dll", "DualDrawDevice.dll"],
        family: PluginFamily::Render,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it — built from the DualDrawDevice project per the M24 census)",
        parquet: true,
        feature: "Draw device (DrawDeviceClass<tTVPBasicDrawDevice<K2/KZInterfaceTypes>>)",
        notes: "Not implemented; only matters if PARQUET's Steam build requires it for overlay rendering.",
        install: |engine| engine.register_plugin(SteamDrawDevicePlugin),
    },
    PluginEntry {
        name: "csvParser.dll",
        aliases: &["CSVParser.dll"],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/csvParser",
        parquet: false,
        feature: "CSVParser class",
        notes: "Not implemented as a plugin: PackinOne already bundles a working CSVParser, so linking this file would add nothing new.",
        install: |engine| engine.register_plugin(CsvParserPlugin),
    },
    PluginEntry {
        name: "dirlist.dll",
        aliases: &[],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/dirlist",
        parquet: false,
        feature: "Storage directory listing",
        notes: "Not implemented; the engine can already list storages, so this is a matter of the plugin's own member names.",
        install: |engine| engine.register_plugin(DirlistPlugin),
    },
    PluginEntry {
        name: "expat.dll",
        aliases: &[],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/expat",
        parquet: false,
        feature: "XML (expat) parsing",
        notes: "Not implemented; needs an XML parser behind the plugin's class surface.",
        install: |engine| engine.register_plugin(ExpatPlugin),
    },
    PluginEntry {
        name: "ExtKAGParser.dll",
        aliases: &["extKAGParser.dll"],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "http://keepcreating.g2.xrea.com/krkrplugins/ExtKAGParser/ExtKAGParser-0143.zip",
        parquet: false,
        feature: "Extra KAGParser helpers",
        notes: "Not implemented; check whether PARQUET's KAG scripts need its tag dictionary extensions.",
        install: |engine| engine.register_plugin(ExtKagParserPlugin),
    },
    PluginEntry {
        name: "fstat.dll",
        aliases: &[],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/fstat",
        parquet: false,
        feature: "File statistics for storages and placed files",
        notes: "Not implemented; the engine's storage layer exposes existence and listing, not the plugin's stat members.",
        install: |engine| engine.register_plugin(FstatPlugin),
    },
    PluginEntry {
        name: "KAGParserExb.dll",
        aliases: &["kagparserexb.dll"],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "https://github.com/sakano/krkr_archives/tree/master/kagex_plugin/KAGParserExb",
        parquet: false,
        feature: "KAGParserExb variant",
        notes: "Not implemented; a separate plugin from KAGParserEx.dll, not an alias of it.",
        install: |engine| engine.register_plugin(KagParserExbPlugin),
    },
    PluginEntry {
        name: "k2compat.dll",
        aliases: &[],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "kirikiroid2 compatibility layer",
        notes: "Not implemented; relevant because PARQUET also ships kirikiroid2.dll and may expect its compat globals.",
        install: |engine| engine.register_plugin(K2CompatPlugin),
    },
    PluginEntry {
        name: "kagexopt.dll",
        aliases: &[],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it — exports only GetOptionDesc per the M24 census)",
        parquet: true,
        feature: "KAG option description resource (GetOptionDesc export; vomstyle/overlay/mixer/layer/… option names)",
        notes: "Not implemented; it is a resource plugin rather than a TJS surface, so the option descriptions have to reach KAG's config handling.",
        install: |engine| engine.register_plugin(KagexOptPlugin),
    },
    PluginEntry {
        name: "kirikiroid2.dll",
        aliases: &[],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "(no public source; the kirikiroid2 port's own module)",
        parquet: false,
        feature: "kirikiroid2 runtime module",
        notes: "Not implemented; Kirakira is a desktop engine, so the port-specific globals it adds are only needed if a game probes for them.",
        install: |engine| engine.register_plugin(Kirikiroid2Plugin),
    },
    PluginEntry {
        name: "savestruct.dll",
        aliases: &["saveStruct.dll", "SaveStruct.dll"],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/saveStruct",
        parquet: false,
        feature: "TJS object graph (de)serialization",
        notes: "Not implemented; games that use it cannot save nested structures until it lands.",
        install: |engine| engine.register_plugin(SaveStructPlugin),
    },
    PluginEntry {
        name: "scriptsEx.dll",
        aliases: &["ScriptsEx.dll"],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "https://github.com/wtnbgo/scriptsEx",
        parquet: false,
        feature: "Extra Scripts/System helper methods",
        notes: "Not implemented; PackinOne bundles part of this surface already.",
        install: |engine| engine.register_plugin(ScriptsExPlugin),
    },
    PluginEntry {
        name: "sqlite3.dll",
        aliases: &[],
        family: PluginFamily::Script,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/sqlite3",
        parquet: false,
        feature: "SQLite access",
        notes: "Not implemented; would need a Rust SQLite dependency and the plugin's class surface.",
        install: |engine| engine.register_plugin(Sqlite3Plugin),
    },
    PluginEntry {
        name: "yuzuex.dll",
        aliases: &["proxyfs.dll"],
        family: PluginFamily::Archive,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it — built from the proxyfs project per the M24 census)",
        parquet: true,
        feature: "ProxyStorageMap / proxyfs storage remapping",
        notes: "Not implemented; it is a storage-media plugin (no TJS surface), so the remap has to happen where archives are resolved.",
        install: |engine| engine.register_plugin(YuzuExPlugin),
    },
    PluginEntry {
        name: "dmmcloud.dll",
        aliases: &[],
        family: PluginFamily::System,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "DMM GAMES platform integration (cloud save / account)",
        notes: "Not implemented; platform-locked, so PARQUET only reaches it on a DMM build.",
        install: |engine| engine.register_plugin(DmmCloudPlugin),
    },
    PluginEntry {
        name: "getabout.dll",
        aliases: &[],
        family: PluginFamily::System,
        status: PluginStatus::Missing,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        feature: "About/build information surface",
        notes: "Not implemented; launcher scripts that print build info will not find its members.",
        install: |engine| engine.register_plugin(GetAboutPlugin),
    },
    PluginEntry {
        name: "getLangName.dll",
        aliases: &["getlangname.dll"],
        family: PluginFamily::System,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "OS/user language name",
        notes: "Not implemented; PARQUET ships it, so a language-selection path may depend on it.",
        install: |engine| engine.register_plugin(GetLangNamePlugin),
    },
    PluginEntry {
        name: "httprequest.dll",
        aliases: &[],
        family: PluginFamily::System,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "HTTP(S) requests from scripts",
        notes: "Not implemented; needs a network stack and a decision about what a game's requests should do offline.",
        install: |engine| engine.register_plugin(HttpRequestPlugin),
    },
    PluginEntry {
        name: "krkrsteam.dll",
        aliases: &[],
        family: PluginFamily::System,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "Steamworks integration",
        notes: "Not implemented; achievements/API calls need a Steamworks binding.",
        install: |engine| engine.register_plugin(KrkrSteamPlugin),
    },
    PluginEntry {
        name: "menu.dll",
        aliases: &[],
        family: PluginFamily::System,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "Native menu (menu bar / context menu)",
        notes: "Not implemented; the windowed shells have no menu surface yet.",
        install: |engine| engine.register_plugin(MenuPlugin),
    },
    PluginEntry {
        name: "win32ole.dll",
        aliases: &[],
        family: PluginFamily::System,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        feature: "Win32 OLE automation",
        notes: "Not implemented; Windows-only by nature, so other platforms can only degrade.",
        install: |engine| engine.register_plugin(Win32OlePlugin),
    },
    PluginEntry {
        name: "util_generic.dll",
        aliases: &[],
        family: PluginFamily::Unknown,
        status: PluginStatus::Missing,
        source: "(no source and no binary anywhere; in the kirikiroid2 plugin list)",
        parquet: false,
        feature: "(unidentified surface)",
        notes: "Not implemented; nothing to census. Implement from observed KAGEX usage and keep the surface marked unverified.",
        install: |engine| engine.register_plugin(UtilGenericPlugin),
    },
    PluginEntry {
        name: "util_graph.dll",
        aliases: &[],
        family: PluginFamily::Unknown,
        status: PluginStatus::Missing,
        source: "(no source and no binary anywhere; in the kirikiroid2 plugin list)",
        parquet: false,
        feature: "(unidentified surface)",
        notes: "Not implemented; nothing to census. Implement from observed KAGEX usage and keep the surface marked unverified.",
        install: |engine| engine.register_plugin(UtilGraphPlugin),
    },
    PluginEntry {
        name: "util_system.dll",
        aliases: &[],
        family: PluginFamily::Unknown,
        status: PluginStatus::Missing,
        source: "(no source and no binary anywhere; in the kirikiroid2 plugin list)",
        parquet: false,
        feature: "(unidentified surface)",
        notes: "Not implemented; nothing to census. Implement from observed KAGEX usage and keep the surface marked unverified.",
        install: |engine| engine.register_plugin(UtilSystemPlugin),
    },
    PluginEntry {
        name: "wfBasicEffect.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it — class names recovered by the M24 census)",
        parquet: true,
        feature: "GraphicEqualizer / StkFreeVerb / WaveDelay filters on WaveSoundBuffer",
        notes: "Not implemented; needs DSP hooks in krkr-audio that WaveSoundBuffer can drive.",
        install: |engine| engine.register_plugin(WfBasicEffectPlugin),
    },
    PluginEntry {
        name: "wfTypicalDSP.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        status: PluginStatus::Missing,
        source: "(no public source; PARQUET ships it — class names recovered by the M24 census)",
        parquet: true,
        feature: "WaveDSPFilter (tTJSNC_WaveDSPFilter / tTJSNI_WaveDSPFilter) on WaveSoundBuffer",
        notes: "Not implemented; PARQUET ships it, so check whether the game uses it before the DSP chain is built.",
        install: |engine| engine.register_plugin(WfTypicalDspPlugin),
    },
    PluginEntry {
        name: "wutcwf.dll",
        aliases: &[],
        family: PluginFamily::Unknown,
        status: PluginStatus::Missing,
        source: "https://github.com/krkrz/SamplePlugin/tree/master/wutcwf",
        parquet: false,
        feature: "(surface still to be read from the plugin's source)",
        notes: "Not implemented; source exists in krkrz's SamplePlugin tree (plus a Kirikiroid2 port), so read it before implementing.",
        install: |engine| engine.register_plugin(WutcwfPlugin),
    },
];

/// Every name in the community "Kirikiroid2 dll list" gist
/// (<https://gist.github.com/uyjulian/060e6d7e4a0ca50916182b351590867b>),
/// verbatim — including the duplicate spellings (`LayerExImage.dll` /
/// `layerExImage.dll`, `savestruct.dll` / `saveStruct.dll`) that the alias
/// list has to absorb.
pub const GIST_PLUGIN_NAMES: &[&str] = &[
    "AlphaMovie.dll",
    "DrawDeviceD3D.dll",
    "DrawDeviceD3DZ.dll",
    "ExtKAGParser.dll",
    "KAGParserEx.dll",
    "KAGParserExb.dll",
    "KaichoTrans.dll",
    "LayerExDraw.dll",
    "LayerExImage.dll",
    "LayerExSave.dll",
    "PSBFile.dll",
    "PackinOne.dll",
    "TextRender.dll",
    "addFont.dll",
    "csvParser.dll",
    "dirlist.dll",
    "emoteplayer.dll",
    "expat.dll",
    "extNagano.dll",
    "extrans.dll",
    "fftgraph.dll",
    "fstat.dll",
    "getSample.dll",
    "getabout.dll",
    "gfxEffect.dll",
    "json.dll",
    "kirikiroid2.dll",
    "layerExAlpha.dll",
    "layerExAreaAverage.dll",
    "layerExBTOA.dll",
    "layerExImage.dll",
    "layerExMovie.dll",
    "layerExRaster.dll",
    "layerExShimmer.dll",
    "lzfs.dll",
    "minizip.dll",
    "motionplayer.dll",
    "multiimage.dll",
    "perspective.dll",
    "saveStruct.dll",
    "savestruct.dll",
    "scriptsEx.dll",
    "shrinkCopy.dll",
    "sqlite3.dll",
    "util_generic.dll",
    "util_graph.dll",
    "util_system.dll",
    "varfile.dll",
    "win32dialog.dll",
    "windowEx.dll",
    "wutcwf.dll",
    "xp3filter.dll",
];

/// Every DLL under PARQUET's `plugin/` directory (34 files).
pub const PARQUET_PLUGIN_FILES: &[&str] = &[
    "AlphaMovie.dll",
    "KAGParserEx.dll",
    "PackinOne.dll",
    "SteamDrawDevice.dll",
    "dmmcloud.dll",
    "extNagano.dll",
    "extrans.dll",
    "gamepad.dll",
    "getLangName.dll",
    "getSample.dll",
    "GlitchEffect.dll",
    "httprequest.dll",
    "json.dll",
    "k2compat.dll",
    "kagexopt.dll",
    "krkrsteam.dll",
    "krmovie.dll",
    "layerExDraw.dll",
    "lzfs.dll",
    "menu.dll",
    "motionplayer.dll",
    "motionplayer_nod3d.dll",
    "nene.dll",
    "psbfile.dll",
    "psd.dll",
    "textrender.dll",
    "wfBasicEffect.dll",
    "wfTypicalDSP.dll",
    "win32dialog.dll",
    "win32ole.dll",
    "windowEx.dll",
    "wuopus.dll",
    "wuvorbis.dll",
    "yuzuex.dll",
];

/// The catalog entry `name` refers to, in any spelling.
pub fn resolve(name: &str) -> Option<&'static PluginEntry> {
    CATALOG.iter().find(|entry| entry.matches(name))
}

/// The canonical name of the plugin `name` refers to.
pub fn canonical_name(name: &str) -> Option<&'static str> {
    resolve(name).map(|entry| entry.name)
}

/// True when two names refer to the same plugin. Names that are in no catalog
/// entry fall back to a case-insensitive comparison.
pub fn is_same_plugin(a: &str, b: &str) -> bool {
    match (resolve(a), resolve(b)) {
        // Compared by canonical name: `CATALOG` is a const, so the same entry
        // may be inlined at several addresses and pointer identity is not
        // meaningful.
        (Some(left), Some(right)) => left.name == right.name,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// Canonical names of the plugins PARQUET ships.
pub fn parquet_plugin_names() -> impl Iterator<Item = &'static str> {
    CATALOG
        .iter()
        .filter(|entry| entry.parquet)
        .map(|entry| entry.name)
}

/// Entries with no implementation yet.
pub fn missing_plugins() -> impl Iterator<Item = &'static PluginEntry> {
    CATALOG.iter().filter(|entry| entry.is_placeholder())
}

/// The full feature→plugin mapping: the engine-owned runtime objects plus
/// every catalog entry.
pub fn plugin_mappings() -> impl Iterator<Item = PluginMapping> {
    crate::NATIVE_MAPPINGS
        .iter()
        .copied()
        .chain(CATALOG.iter().map(PluginEntry::mapping))
}

/// Installs one catalog entry into `engine`.
pub fn install_plugin(engine: &mut KrkrEngine, entry: &PluginEntry) -> Result<()> {
    (entry.install)(engine)
}

/// Reports a plugin that has no implementation. Called by the placeholder
/// modules (`src/placeholder.rs`); the line is a warning because a game that
/// loads the plugin is asking for members that will not be there.
pub(crate) fn report_unimplemented_plugin(runtime: &mut Runtime<KrkrHost>, name: &str) {
    let message = match resolve(name) {
        Some(entry) => format!(
            "WARN plugin not implemented: {name} (family {}, source {}) — inert placeholder \
             registered, no TJS surface installed",
            entry.family.as_str(),
            entry.source,
        ),
        None => format!(
            "WARN plugin not implemented: {name} — inert placeholder registered, no TJS surface \
             installed"
        ),
    };
    runtime.host_mut().log(&message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names_are_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for entry in CATALOG {
            assert!(
                !seen
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(entry.name)),
                "duplicate catalog entry for {}",
                entry.name
            );
            seen.push(entry.name);
        }
    }

    #[test]
    fn aliases_resolve_to_their_own_entry() {
        for entry in CATALOG {
            for alias in entry.aliases {
                assert_eq!(
                    canonical_name(alias),
                    Some(entry.name),
                    "{alias} does not resolve to the entry that lists it"
                );
                assert!(
                    *alias != entry.name,
                    "{} lists its own canonical name as an alias",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn every_gist_name_resolves_to_a_catalog_entry() {
        for name in GIST_PLUGIN_NAMES {
            assert!(
                resolve(name).is_some(),
                "gist plugin name {name} has no catalog entry"
            );
        }
    }

    #[test]
    fn every_parquet_dll_resolves_and_every_parquet_entry_is_shipped() {
        for name in PARQUET_PLUGIN_FILES {
            let entry = resolve(name)
                .unwrap_or_else(|| panic!("PARQUET plugin {name} has no catalog entry"));
            assert!(
                entry.parquet,
                "{} is shipped by PARQUET but the catalog entry does not say so",
                entry.name
            );
        }
        for entry in parquet_plugin_names() {
            let canonical = canonical_name(entry).expect("parquet names are canonical");
            assert!(
                PARQUET_PLUGIN_FILES
                    .iter()
                    .any(|file| resolve(file).is_some_and(|other| other.name == canonical)),
                "{} is marked as shipped by PARQUET but is not in plugin/",
                entry
            );
        }
    }

    #[test]
    fn every_name_resolves_case_insensitively() {
        for entry in CATALOG {
            let upper = entry.name.to_ascii_uppercase();
            assert_eq!(
                canonical_name(&upper),
                Some(entry.name),
                "{} does not resolve when upper-cased",
                entry.name
            );
        }
    }

    #[test]
    fn unimplemented_entries_have_their_own_source_or_a_census_note() {
        for entry in missing_plugins() {
            assert!(
                !entry.source.is_empty(),
                "{} has no source note",
                entry.name
            );
            assert!(
                entry.status == PluginStatus::Missing,
                "{} is a placeholder but not marked as one",
                entry.name
            );
        }
    }

    #[test]
    fn same_plugin_understands_aliases_and_unknown_names() {
        assert!(is_same_plugin("textRender.dll", "textrender.dll"));
        assert!(is_same_plugin("motionplayer_nod3d.dll", "motionplayer.dll"));
        assert!(!is_same_plugin("json.dll", "jsonp.dll"));
        assert!(is_same_plugin("NotInCatalog.dll", "notincatalog.DLL"));
        assert_eq!(canonical_name("TextRender.dll"), Some("textrender.dll"));
        assert_eq!(canonical_name("saveStruct.dll"), Some("savestruct.dll"));
        assert_eq!(canonical_name("nowhere.dll"), None);
    }
}
