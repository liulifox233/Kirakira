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
//! (`family`), where its behaviour is documented (`source`), and whether
//! PARQUET ships the DLL (`parquet`). What this crate has *built* of the
//! plugin — [`PluginStatus`], the engine `feature` it covers, `notes`, and the
//! `install` fn — is not spelled here: each entry carries its module's own
//! [`PluginMeta`] as `meta`, so that state has one home (`src/<plugin>.rs`)
//! while this table still shows all of it at once.
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
//! surface and reports itself through the engine log. Implementing the plugin
//! is a change to that one file — replace the invocation with a real
//! `KrkrPlugin` impl, set `META.status` to `Shim` or `Implemented`, and
//! update `META.notes` (and `META.feature` when the surface grew). Nothing
//! here has to move, so parallel implementations never share a file.

use krkr_engine::{KrkrEngine, KrkrHost};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::PluginMapping;

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

/// Implementation state of one plugin: everything that changes when the
/// plugin is implemented. Every plugin module (`src/<plugin>.rs`) exposes its
/// own as a `pub(crate) const META`, and [`CATALOG`] reads it from there, so
/// implementing a plugin is a one-file change — see [`crate::placeholder`].
///
/// Identity fields (name spellings, [`PluginFamily`], upstream `source`,
/// PARQUET shipping) are *not* here: they describe what the plugin is and
/// change only when the upstream census does, so they stay in the entry.
#[derive(Clone, Copy, Debug)]
pub struct PluginMeta {
    /// How much of the plugin exists today.
    pub status: PluginStatus,
    /// What the plugin extends in the engine — what a later mission builds.
    pub feature: &'static str,
    /// Coverage in this crate today, and what is left to do.
    pub notes: &'static str,
    /// Installs the module. One plugin per entry; see [`install_plugin`].
    pub install: fn(&mut KrkrEngine) -> Result<()>,
}

/// One plugin identity, as the name contract sees it.
#[derive(Clone, Copy, Debug)]
pub struct PluginEntry {
    /// Canonical DLL name: what `KrkrPlugin::name` reports, what
    /// `Plugins.link` matches and what a profile selects.
    pub name: &'static str,
    /// Other spellings of the same plugin seen in the wild.
    pub aliases: &'static [&'static str],
    pub family: PluginFamily,
    /// Where the plugin's behaviour is documented (upstream source, or the
    /// note that only a binary exists).
    pub source: &'static str,
    /// Whether PARQUET ships the DLL (under this name or an alias).
    pub parquet: bool,
    /// The module's own implementation state (`crate::<module>::META`).
    pub meta: PluginMeta,
}

impl PluginEntry {
    /// True when the entry's module installs nothing and only reports itself.
    pub fn is_placeholder(&self) -> bool {
        self.meta.status == PluginStatus::Missing
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
            feature: self.meta.feature,
            provider: crate::PluginOrigin::Plugin,
            plugin_name: Some(self.name),
            notes: self.meta.notes,
        }
    }
}

/// Every plugin identity, in registration order, each carrying the `META` of
/// the module named by its canonical name.
///
/// The modules that existed before this catalog keep their original install
/// order: the fourteen entries first, then `lzfs.dll` — the KaichoTrans
/// placeholder between them installs nothing, so no real module changed
/// position. The entries after `lzfs.dll` are the census tail, grouped by
/// family as far as the two source lists allowed.
pub const CATALOG: &[PluginEntry] = &[
    PluginEntry {
        name: "addFont.dll",
        aliases: &["AddFont.dll"],
        family: PluginFamily::Font,
        source: "https://github.com/wtnbgo/addFont",
        parquet: false,
        meta: crate::add_font::META,
    },
    PluginEntry {
        name: "motionplayer.dll",
        aliases: &[
            "MotionPlayer.dll",
            "motionplayer_nod3d.dll",
            "MotionPlayer_nod3D.dll",
        ],
        family: PluginFamily::Movie,
        source: "(no public source; PARQUET ships motionplayer.dll and the no-D3D build)",
        parquet: true,
        meta: crate::motion_player::META,
    },
    PluginEntry {
        name: "win32dialog.dll",
        aliases: &["Win32Dialog.dll"],
        family: PluginFamily::System,
        source: "https://github.com/wtnbgo/win32dialog",
        parquet: true,
        meta: crate::win32_dialog::META,
    },
    PluginEntry {
        name: "windowEx.dll",
        aliases: &["WindowEx.dll"],
        family: PluginFamily::System,
        source: "https://github.com/wtnbgo/windowEx",
        parquet: true,
        meta: crate::window_ex::META,
    },
    PluginEntry {
        name: "json.dll",
        aliases: &["Json.dll"],
        family: PluginFamily::Script,
        source: "https://github.com/wtnbgo/json",
        parquet: true,
        meta: crate::json::META,
    },
    PluginEntry {
        name: "PackinOne.dll",
        aliases: &["packinone.dll"],
        family: PluginFamily::Script,
        source: "PackinOne bundle (csvParser, scriptsEx, saveStruct, fstat, shrinkCopy, layerEx*, process)",
        parquet: true,
        meta: crate::packinone::META,
    },
    PluginEntry {
        name: "layerExDraw.dll",
        aliases: &["LayerExDraw.dll"],
        family: PluginFamily::Layer,
        source: "https://github.com/wtnbgo/layerExDraw",
        parquet: true,
        meta: crate::layer_ex_draw::META,
    },
    PluginEntry {
        name: "textrender.dll",
        aliases: &["TextRender.dll", "textRender.dll"],
        family: PluginFamily::Text,
        source: "(no public source; TextRenderBase is subclassed by the game's own TextRender.tjs)",
        parquet: true,
        meta: crate::text_render::META,
    },
    PluginEntry {
        name: "psbfile.dll",
        aliases: &["PSBFile.dll", "psbFile.dll"],
        family: PluginFamily::Script,
        source: "(no public source; M2 PSB container format)",
        parquet: true,
        meta: crate::psb_file::META,
    },
    PluginEntry {
        name: "AlphaMovie.dll",
        aliases: &["alphamovie.dll", "nene.dll", "Nene.dll"],
        family: PluginFamily::Movie,
        source: "http://kaede-software.com/krlm/plugin/alphamovie.zip",
        parquet: true,
        meta: crate::alpha_movie::META,
    },
    PluginEntry {
        name: "getSample.dll",
        aliases: &["GetSample.dll"],
        family: PluginFamily::Audio,
        source: "https://github.com/wtnbgo/getSample",
        parquet: true,
        meta: crate::get_sample::META,
    },
    PluginEntry {
        name: "KAGParserEx.dll",
        aliases: &["kagparserex.dll"],
        family: PluginFamily::Script,
        source: "https://github.com/wtnbgo/KAGParserEx",
        parquet: true,
        meta: crate::kag_parser_ex::META,
    },
    PluginEntry {
        name: "extrans.dll",
        aliases: &["Extrans.dll"],
        family: PluginFamily::Transition,
        source: "https://github.com/krkrz/SamplePlugin/tree/master/extrans",
        parquet: true,
        meta: crate::extrans::META,
    },
    PluginEntry {
        name: "extNagano.dll",
        aliases: &["ExtNagano.dll"],
        family: PluginFamily::Transition,
        source: "https://web.archive.org/web/20120604091809fw_/http://ymtkyk.sakura.ne.jp/krkr.STG/plugin/extNagano.html",
        parquet: true,
        meta: crate::extnagano::META,
    },
    PluginEntry {
        name: "KaichoTrans.dll",
        aliases: &["kaichotrans.dll"],
        family: PluginFamily::Transition,
        source: "http://keepcreating.g2.xrea.com/krkrplugins/KaichoTrans/KaichoTrans.zip",
        parquet: false,
        meta: crate::kaicho_trans::META,
    },
    PluginEntry {
        name: "lzfs.dll",
        aliases: &["Lzfs.dll"],
        family: PluginFamily::Archive,
        source: "(no public source; lzfs archive reader)",
        parquet: true,
        meta: crate::lzfs::META,
    },
    PluginEntry {
        name: "minizip.dll",
        aliases: &[],
        family: PluginFamily::Archive,
        source: "https://github.com/wtnbgo/minizip",
        parquet: false,
        meta: crate::minizip::META,
    },
    PluginEntry {
        name: "varfile.dll",
        aliases: &[],
        family: PluginFamily::Archive,
        source: "https://github.com/wtnbgo/varfile",
        parquet: false,
        meta: crate::varfile::META,
    },
    PluginEntry {
        name: "xp3filter.dll",
        aliases: &["Xp3Filter.dll"],
        family: PluginFamily::Archive,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        meta: crate::xp3_filter::META,
    },
    PluginEntry {
        name: "fftgraph.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        source: "https://github.com/krkrz/fftgraph",
        parquet: false,
        meta: crate::fft_graph::META,
    },
    PluginEntry {
        name: "wuopus.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::wuopus::META,
    },
    PluginEntry {
        name: "wuvorbis.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::wuvorbis::META,
    },
    PluginEntry {
        name: "gamepad.dll",
        aliases: &[],
        family: PluginFamily::Input,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::gamepad::META,
    },
    PluginEntry {
        name: "GlitchEffect.dll",
        aliases: &["glitcheffect.dll"],
        family: PluginFamily::Layer,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::glitch_effect::META,
    },
    PluginEntry {
        name: "gfxEffect.dll",
        aliases: &[],
        family: PluginFamily::Layer,
        source: "http://kaede-software.com/krlm/plugin/gfx_effect.zip",
        parquet: false,
        meta: crate::gfx_effect::META,
    },
    PluginEntry {
        name: "layerExAlpha.dll",
        aliases: &["LayerExAlpha.dll"],
        family: PluginFamily::Layer,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        meta: crate::layer_ex_alpha::META,
    },
    PluginEntry {
        name: "layerExAreaAverage.dll",
        aliases: &["LayerExAreaAverage.dll"],
        family: PluginFamily::Layer,
        source: "https://github.com/wtnbgo/layerExAreaAverage",
        parquet: false,
        meta: crate::layer_ex_area_average::META,
    },
    PluginEntry {
        name: "layerExBTOA.dll",
        aliases: &["LayerExBTOA.dll"],
        family: PluginFamily::Layer,
        source: "https://github.com/wtnbgo/layerExBTOA",
        parquet: false,
        meta: crate::layer_ex_btoa::META,
    },
    PluginEntry {
        name: "layerExImage.dll",
        aliases: &["LayerExImage.dll"],
        family: PluginFamily::Layer,
        source: "https://github.com/wtnbgo/layerExImage",
        parquet: false,
        meta: crate::layer_ex_image::META,
    },
    PluginEntry {
        name: "layerExLongExposure.dll",
        aliases: &["LayerExLongExposure.dll"],
        family: PluginFamily::Layer,
        source: "(no public source; kirikiri2 trunk src/plugins/win32/layerExLongExposure)",
        parquet: false,
        meta: crate::layer_ex_long_exposure::META,
    },
    PluginEntry {
        name: "layerExMovie.dll",
        aliases: &["LayerExMovie.dll"],
        family: PluginFamily::Layer,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExMovie",
        parquet: false,
        meta: crate::layer_ex_movie::META,
    },
    PluginEntry {
        name: "layerExRaster.dll",
        aliases: &["LayerExRaster.dll"],
        family: PluginFamily::Layer,
        source: "https://github.com/wtnbgo/layerExRaster",
        parquet: false,
        meta: crate::layer_ex_raster::META,
    },
    PluginEntry {
        name: "layerExSave.dll",
        aliases: &["LayerExSave.dll"],
        family: PluginFamily::Layer,
        source: "https://github.com/wtnbgo/layerExSave",
        parquet: false,
        meta: crate::layer_ex_save::META,
    },
    PluginEntry {
        name: "layerExShimmer.dll",
        aliases: &["LayerExShimmer.dll"],
        family: PluginFamily::Layer,
        source: "http://keepcreating.g2.xrea.com/krkrplugins/ShimmerPlugin/layerExShimmer.zip",
        parquet: false,
        meta: crate::layer_ex_shimmer::META,
    },
    PluginEntry {
        name: "multiimage.dll",
        aliases: &["MultiImage.dll"],
        family: PluginFamily::Layer,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        meta: crate::multi_image::META,
    },
    PluginEntry {
        name: "perspective.dll",
        aliases: &[],
        family: PluginFamily::Layer,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExPerspective",
        parquet: false,
        meta: crate::layer_ex_perspective::META,
    },
    PluginEntry {
        name: "psd.dll",
        aliases: &[],
        family: PluginFamily::Layer,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::psd::META,
    },
    PluginEntry {
        name: "shrinkCopy.dll",
        aliases: &[],
        family: PluginFamily::Layer,
        source: "https://github.com/wtnbgo/shrinkCopy",
        parquet: false,
        meta: crate::shrink_copy::META,
    },
    PluginEntry {
        name: "emoteplayer.dll",
        aliases: &["EmotePlayer.dll"],
        family: PluginFamily::Movie,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        meta: crate::emoteplayer::META,
    },
    PluginEntry {
        name: "krmovie.dll",
        aliases: &[],
        family: PluginFamily::Movie,
        source: "https://github.com/krkrz/krkrz",
        parquet: true,
        meta: crate::krmovie::META,
    },
    PluginEntry {
        name: "DrawDeviceD3D.dll",
        aliases: &["drawdeviceD3D.dll"],
        family: PluginFamily::Render,
        source: "https://github.com/krkrz/krkr2/tree/master/kirikiri2/trunk/kirikiri2/src/plugins/win32/drawdeviceD3D",
        parquet: false,
        meta: crate::draw_device_d3d::META,
    },
    PluginEntry {
        name: "DrawDeviceD3DZ.dll",
        aliases: &[],
        family: PluginFamily::Render,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/drawdeviceD3D",
        parquet: false,
        meta: crate::draw_device_d3dz::META,
    },
    PluginEntry {
        name: "SteamDrawDevice.dll",
        aliases: &["steamdrawdevice.dll", "DualDrawDevice.dll"],
        family: PluginFamily::Render,
        source: "(no public source; PARQUET ships it — built from the DualDrawDevice project per the M24 census)",
        parquet: true,
        meta: crate::steam_draw_device::META,
    },
    PluginEntry {
        name: "csvParser.dll",
        aliases: &["CSVParser.dll"],
        family: PluginFamily::Script,
        source: "https://github.com/wtnbgo/csvParser",
        parquet: false,
        meta: crate::csv_parser::META,
    },
    PluginEntry {
        name: "dirlist.dll",
        aliases: &[],
        family: PluginFamily::Script,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/dirlist",
        parquet: false,
        meta: crate::dirlist::META,
    },
    PluginEntry {
        name: "expat.dll",
        aliases: &[],
        family: PluginFamily::Script,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/expat",
        parquet: false,
        meta: crate::expat::META,
    },
    PluginEntry {
        name: "ExtKAGParser.dll",
        aliases: &["extKAGParser.dll"],
        family: PluginFamily::Script,
        source: "http://keepcreating.g2.xrea.com/krkrplugins/ExtKAGParser/ExtKAGParser-0143.zip",
        parquet: false,
        meta: crate::ext_kag_parser::META,
    },
    PluginEntry {
        name: "fstat.dll",
        aliases: &[],
        family: PluginFamily::Script,
        source: "https://github.com/wtnbgo/fstat",
        parquet: false,
        meta: crate::fstat::META,
    },
    PluginEntry {
        name: "KAGParserExb.dll",
        aliases: &["kagparserexb.dll"],
        family: PluginFamily::Script,
        source: "https://github.com/sakano/krkr_archives/tree/master/kagex_plugin/KAGParserExb",
        parquet: false,
        meta: crate::kag_parser_exb::META,
    },
    PluginEntry {
        name: "k2compat.dll",
        aliases: &[],
        family: PluginFamily::Script,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::k2compat::META,
    },
    PluginEntry {
        name: "kagexopt.dll",
        aliases: &[],
        family: PluginFamily::Script,
        source: "(no public source; PARQUET ships it — exports only GetOptionDesc per the M24 census)",
        parquet: true,
        meta: crate::kagexopt::META,
    },
    PluginEntry {
        name: "kirikiroid2.dll",
        aliases: &[],
        family: PluginFamily::Script,
        source: "(no public source; the kirikiroid2 port's own module)",
        parquet: false,
        meta: crate::kirikiroid2::META,
    },
    PluginEntry {
        name: "savestruct.dll",
        aliases: &["saveStruct.dll", "SaveStruct.dll"],
        family: PluginFamily::Script,
        source: "https://github.com/wtnbgo/saveStruct",
        parquet: false,
        meta: crate::save_struct::META,
    },
    PluginEntry {
        name: "scriptsEx.dll",
        aliases: &["ScriptsEx.dll"],
        family: PluginFamily::Script,
        source: "https://github.com/wtnbgo/scriptsEx",
        parquet: false,
        meta: crate::scripts_ex::META,
    },
    PluginEntry {
        name: "sqlite3.dll",
        aliases: &[],
        family: PluginFamily::Script,
        source: "https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/sqlite3",
        parquet: false,
        meta: crate::sqlite3::META,
    },
    PluginEntry {
        name: "yuzuex.dll",
        aliases: &["proxyfs.dll"],
        family: PluginFamily::Archive,
        source: "(no public source; PARQUET ships it — built from the proxyfs project per the M24 census)",
        parquet: true,
        meta: crate::yuzuex::META,
    },
    PluginEntry {
        name: "dmmcloud.dll",
        aliases: &[],
        family: PluginFamily::System,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::dmmcloud::META,
    },
    PluginEntry {
        name: "getabout.dll",
        aliases: &[],
        family: PluginFamily::System,
        source: "(no public source; in the kirikiroid2 plugin list)",
        parquet: false,
        meta: crate::get_about::META,
    },
    PluginEntry {
        name: "getLangName.dll",
        aliases: &["getlangname.dll"],
        family: PluginFamily::System,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::get_lang_name::META,
    },
    PluginEntry {
        name: "httprequest.dll",
        aliases: &[],
        family: PluginFamily::System,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::http_request::META,
    },
    PluginEntry {
        name: "krkrsteam.dll",
        aliases: &[],
        family: PluginFamily::System,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::krkrsteam::META,
    },
    PluginEntry {
        name: "menu.dll",
        aliases: &[],
        family: PluginFamily::System,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::menu::META,
    },
    PluginEntry {
        name: "win32ole.dll",
        aliases: &[],
        family: PluginFamily::System,
        source: "(no public source; PARQUET ships it)",
        parquet: true,
        meta: crate::win32ole::META,
    },
    PluginEntry {
        name: "util_generic.dll",
        aliases: &[],
        family: PluginFamily::Unknown,
        source: "(no source and no binary anywhere; in the kirikiroid2 plugin list)",
        parquet: false,
        meta: crate::util_generic::META,
    },
    PluginEntry {
        name: "util_graph.dll",
        aliases: &[],
        family: PluginFamily::Unknown,
        source: "(no source and no binary anywhere; in the kirikiroid2 plugin list)",
        parquet: false,
        meta: crate::util_graph::META,
    },
    PluginEntry {
        name: "util_system.dll",
        aliases: &[],
        family: PluginFamily::Unknown,
        source: "(no source and no binary anywhere; in the kirikiroid2 plugin list)",
        parquet: false,
        meta: crate::util_system::META,
    },
    PluginEntry {
        name: "wfBasicEffect.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        source: "(no public source; PARQUET ships it — class names recovered by the M24 census)",
        parquet: true,
        meta: crate::wf_basic_effect::META,
    },
    PluginEntry {
        name: "wfTypicalDSP.dll",
        aliases: &[],
        family: PluginFamily::Audio,
        source: "(no public source; PARQUET ships it — class names recovered by the M24 census)",
        parquet: true,
        meta: crate::wf_typical_dsp::META,
    },
    PluginEntry {
        name: "wutcwf.dll",
        aliases: &[],
        family: PluginFamily::Unknown,
        source: "https://github.com/krkrz/SamplePlugin/tree/master/wutcwf",
        parquet: false,
        meta: crate::wutcwf::META,
    },
];

/// Every name in the community "Kirikiroid2 dll list" gist
/// (<https://gist.github.com/uyjulian/060e6d7e4a0ca50916182b351590867b>),
/// verbatim — including the duplicate spellings (`LayerExImage.dll` /
/// `layerExImage.dll`, `savestruct.dll` / `saveStruct.dll`) that the alias
/// list has to absorb. Kept in alphabetical order rather than the gist's own
/// order; the tests compare it as a set, so the order carries no meaning.
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

/// Installs one catalog entry into `engine` through the module that owns it
/// (`entry.meta.install`).
pub fn install_plugin(engine: &mut KrkrEngine, entry: &PluginEntry) -> Result<()> {
    (entry.meta.install)(engine)
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
                entry.meta.status == PluginStatus::Missing,
                "{} is a placeholder but not marked as one",
                entry.name
            );
        }
    }

    /// The `META` each plugin module exposes, keyed by canonical name. The
    /// table is spelled out here rather than derived from `CATALOG` so that it
    /// is an independent copy: an entry that carries a duplicated or
    /// neighbouring `META` instead of its own module's fails the comparison
    /// below, and a module whose `META` is not in the table at all is caught
    /// by the reverse check in the same test.
    fn module_meta(name: &str) -> PluginMeta {
        match name {
            "addFont.dll" => crate::add_font::META,
            "motionplayer.dll" => crate::motion_player::META,
            "win32dialog.dll" => crate::win32_dialog::META,
            "windowEx.dll" => crate::window_ex::META,
            "json.dll" => crate::json::META,
            "PackinOne.dll" => crate::packinone::META,
            "layerExDraw.dll" => crate::layer_ex_draw::META,
            "textrender.dll" => crate::text_render::META,
            "psbfile.dll" => crate::psb_file::META,
            "AlphaMovie.dll" => crate::alpha_movie::META,
            "getSample.dll" => crate::get_sample::META,
            "KAGParserEx.dll" => crate::kag_parser_ex::META,
            "extrans.dll" => crate::extrans::META,
            "extNagano.dll" => crate::extnagano::META,
            "KaichoTrans.dll" => crate::kaicho_trans::META,
            "lzfs.dll" => crate::lzfs::META,
            "minizip.dll" => crate::minizip::META,
            "varfile.dll" => crate::varfile::META,
            "xp3filter.dll" => crate::xp3_filter::META,
            "fftgraph.dll" => crate::fft_graph::META,
            "wuopus.dll" => crate::wuopus::META,
            "wuvorbis.dll" => crate::wuvorbis::META,
            "gamepad.dll" => crate::gamepad::META,
            "GlitchEffect.dll" => crate::glitch_effect::META,
            "gfxEffect.dll" => crate::gfx_effect::META,
            "layerExAlpha.dll" => crate::layer_ex_alpha::META,
            "layerExAreaAverage.dll" => crate::layer_ex_area_average::META,
            "layerExBTOA.dll" => crate::layer_ex_btoa::META,
            "layerExImage.dll" => crate::layer_ex_image::META,
            "layerExLongExposure.dll" => crate::layer_ex_long_exposure::META,
            "layerExMovie.dll" => crate::layer_ex_movie::META,
            "layerExRaster.dll" => crate::layer_ex_raster::META,
            "layerExSave.dll" => crate::layer_ex_save::META,
            "layerExShimmer.dll" => crate::layer_ex_shimmer::META,
            "multiimage.dll" => crate::multi_image::META,
            "perspective.dll" => crate::layer_ex_perspective::META,
            "psd.dll" => crate::psd::META,
            "shrinkCopy.dll" => crate::shrink_copy::META,
            "emoteplayer.dll" => crate::emoteplayer::META,
            "krmovie.dll" => crate::krmovie::META,
            "DrawDeviceD3D.dll" => crate::draw_device_d3d::META,
            "DrawDeviceD3DZ.dll" => crate::draw_device_d3dz::META,
            "SteamDrawDevice.dll" => crate::steam_draw_device::META,
            "csvParser.dll" => crate::csv_parser::META,
            "dirlist.dll" => crate::dirlist::META,
            "expat.dll" => crate::expat::META,
            "ExtKAGParser.dll" => crate::ext_kag_parser::META,
            "fstat.dll" => crate::fstat::META,
            "KAGParserExb.dll" => crate::kag_parser_exb::META,
            "k2compat.dll" => crate::k2compat::META,
            "kagexopt.dll" => crate::kagexopt::META,
            "kirikiroid2.dll" => crate::kirikiroid2::META,
            "savestruct.dll" => crate::save_struct::META,
            "scriptsEx.dll" => crate::scripts_ex::META,
            "sqlite3.dll" => crate::sqlite3::META,
            "yuzuex.dll" => crate::yuzuex::META,
            "dmmcloud.dll" => crate::dmmcloud::META,
            "getabout.dll" => crate::get_about::META,
            "getLangName.dll" => crate::get_lang_name::META,
            "httprequest.dll" => crate::http_request::META,
            "krkrsteam.dll" => crate::krkrsteam::META,
            "menu.dll" => crate::menu::META,
            "win32ole.dll" => crate::win32ole::META,
            "util_generic.dll" => crate::util_generic::META,
            "util_graph.dll" => crate::util_graph::META,
            "util_system.dll" => crate::util_system::META,
            "wfBasicEffect.dll" => crate::wf_basic_effect::META,
            "wfTypicalDSP.dll" => crate::wf_typical_dsp::META,
            "wutcwf.dll" => crate::wutcwf::META,
            other => panic!("catalog entry {other} has no module of its own"),
        }
    }

    #[test]
    fn every_entry_carries_the_meta_of_the_module_named_by_its_canonical_name() {
        for entry in CATALOG {
            let module = module_meta(entry.name);
            assert_eq!(
                entry.meta.status, module.status,
                "{}: the entry's status is not its module's META",
                entry.name
            );
            assert_eq!(
                entry.meta.feature, module.feature,
                "{}: the entry's feature is not its module's META",
                entry.name
            );
            assert_eq!(
                entry.meta.notes, module.notes,
                "{}: the entry's notes are not its module's META",
                entry.name
            );
        }
    }

    /// The `META` of a module that still is a `placeholder_plugin!`
    /// invocation says `Missing`, and installing such an entry is what puts
    /// the "not implemented" warning in the log; an entry that is no longer
    /// missing must not report itself. This pins the split from the other
    /// side, through the engine instead of through the table: a module whose
    /// status and behaviour disagree fails here.
    #[test]
    fn every_entry_installs_the_plugin_it_names_and_only_a_placeholder_reports_itself() {
        let mut engine = KrkrEngine::new(krkr_engine::EngineConfig::default()).expect("engine");
        for entry in CATALOG {
            let needle = format!("not implemented: {}", entry.name);
            assert!(
                !engine
                    .host()
                    .linked_plugins()
                    .any(|linked| linked == entry.name),
                "{} was linked before its install",
                entry.name
            );
            let reports_before = engine
                .host()
                .logs()
                .iter()
                .filter(|line| line.contains(&needle))
                .count();

            install_plugin(&mut engine, entry).expect("install catalog entry");

            assert!(
                engine
                    .host()
                    .linked_plugins()
                    .any(|linked| linked == entry.name),
                "{} installs a module that does not register that name",
                entry.name
            );
            let reports = engine
                .host()
                .logs()
                .iter()
                .filter(|line| line.contains(&needle))
                .count()
                - reports_before;
            match entry.meta.status {
                PluginStatus::Missing => assert_eq!(
                    reports, 1,
                    "{}: a missing module must report itself exactly once",
                    entry.name
                ),
                _ => assert_eq!(
                    reports, 0,
                    "{}: an implemented module must not report itself as missing",
                    entry.name
                ),
            }
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
