//! Native (Rust) replacements for the KRKR plugins games load with
//! `Plugins.link`, plus the [`catalog`] of every plugin name the engine knows
//! about.
//!
//! Registration goes through [`register_reference_plugins`] (installs every
//! catalog entry) or [`register_profile_plugins`] with a [`GameProfile`] that
//! selects a subset by any spelling of a plugin's name. Each plugin lives in
//! its own module together with its implementation state
//! ([`catalog::PluginMeta`], the module's `META`), so implementing one is a
//! single-file change; a plugin that has no implementation yet is a module
//! that installs no TJS surface at all and reports itself through the engine
//! log — an unimplemented plugin stays visible instead of silently absent.

pub mod catalog;

mod add_font;
mod alpha_movie;
mod csv_parser;
mod dirlist;
mod dmmcloud;
mod draw_device_d3d;
mod draw_device_d3dz;
mod emoteplayer;
mod expat;
mod ext_kag_parser;
mod extnagano;
mod extrans;
mod fft_graph;
mod fstat;
mod gamepad;
mod get_about;
mod get_lang_name;
mod get_sample;
mod gfx_effect;
mod glitch_effect;
mod http_request;
mod json;
mod k2compat;
mod kag_parser_ex;
mod kag_parser_exb;
mod kagexopt;
mod kaicho_trans;
mod kirikiroid2;
mod krkrsteam;
mod krmovie;
mod layer_ex_alpha;
mod layer_ex_area_average;
mod layer_ex_btoa;
mod layer_ex_draw;
mod layer_ex_image;
mod layer_ex_long_exposure;
mod layer_ex_movie;
mod layer_ex_perspective;
mod layer_ex_raster;
mod layer_ex_save;
mod layer_ex_shimmer;
mod lzfs;
mod menu;
mod minizip;
mod motion_player;
mod multi_image;
mod packinone;
mod placeholder;
mod psb_file;
mod psd;
mod save_struct;
mod scripts_ex;
mod shrink_copy;
mod sqlite3;
mod steam_draw_device;
mod text_render;
mod util_generic;
mod util_graph;
mod util_system;
mod varfile;
mod wf_basic_effect;
mod wf_typical_dsp;
mod win32_dialog;
mod win32ole;
mod window_ex;
mod wuopus;
mod wutcwf;
mod wuvorbis;
mod xp3_filter;
mod yuzuex;

use std::collections::BTreeSet;

use krkr_engine::KrkrEngine;

pub use add_font::AddFontPlugin;
pub use alpha_movie::AlphaMoviePlugin;
pub use catalog::{
    CATALOG, GIST_PLUGIN_NAMES, PARQUET_PLUGIN_FILES, PluginEntry, PluginFamily, PluginMeta,
    PluginStatus, canonical_name, install_plugin, is_same_plugin, missing_plugins,
    parquet_plugin_names, plugin_mappings, resolve,
};
pub use csv_parser::CsvParserPlugin;
pub use dirlist::DirlistPlugin;
pub use dmmcloud::DmmCloudPlugin;
pub use draw_device_d3d::DrawDeviceD3DPlugin;
pub use draw_device_d3dz::DrawDeviceD3DZPlugin;
pub use emoteplayer::EmotePlayerPlugin;
pub use expat::ExpatPlugin;
pub use ext_kag_parser::ExtKagParserPlugin;
pub use extnagano::ExtNaganoPlugin;
pub use extrans::ExtransPlugin;
pub use fft_graph::FftGraphPlugin;
pub use fstat::FstatPlugin;
pub use gamepad::GamepadPlugin;
pub use get_about::GetAboutPlugin;
pub use get_lang_name::GetLangNamePlugin;
pub use get_sample::GetSamplePlugin;
pub use gfx_effect::GfxEffectPlugin;
pub use glitch_effect::GlitchEffectPlugin;
pub use http_request::HttpRequestPlugin;
pub use json::JsonPlugin;
pub use k2compat::K2CompatPlugin;
pub use kag_parser_ex::KagParserExPlugin;
pub use kag_parser_exb::KagParserExbPlugin;
pub use kagexopt::KagexOptPlugin;
pub use kagexopt::{OptionCategory, OptionDesc, OptionKind, OptionValue};
pub use kaicho_trans::KaichoTransPlugin;
pub use kirikiroid2::Kirikiroid2Plugin;
pub use krkrsteam::KrkrSteamPlugin;
pub use krmovie::KrmoviePlugin;
pub use layer_ex_alpha::LayerExAlphaPlugin;
pub use layer_ex_area_average::LayerExAreaAveragePlugin;
pub use layer_ex_btoa::LayerExBtoaPlugin;
pub use layer_ex_draw::LayerExDrawPlugin;
pub use layer_ex_image::LayerExImagePlugin;
pub use layer_ex_long_exposure::LayerExLongExposurePlugin;
pub use layer_ex_movie::LayerExMoviePlugin;
pub use layer_ex_perspective::LayerExPerspectivePlugin;
pub use layer_ex_raster::LayerExRasterPlugin;
pub use layer_ex_save::LayerExSavePlugin;
pub use layer_ex_shimmer::LayerExShimmerPlugin;
pub use lzfs::LzfsPlugin;
pub use menu::MenuPlugin;
pub use minizip::MinizipPlugin;
pub use motion_player::MotionPlayerPlugin;
pub use multi_image::MultiImagePlugin;
pub use packinone::PackinOnePlugin;
pub use psb_file::PsbFilePlugin;
#[doc(hidden)]
pub use psb_file::{PsbValue, debug_parse_psb};
pub use psd::PsdPlugin;
pub use save_struct::SaveStructPlugin;
pub use scripts_ex::ScriptsExPlugin;
pub use shrink_copy::ShrinkCopyPlugin;
pub use sqlite3::Sqlite3Plugin;
pub use steam_draw_device::SteamDrawDevicePlugin;
pub use text_render::TextRenderPlugin;
pub use util_generic::UtilGenericPlugin;
pub use util_graph::UtilGraphPlugin;
pub use util_system::UtilSystemPlugin;
pub use varfile::VarfilePlugin;
pub use wf_basic_effect::WfBasicEffectPlugin;
pub use wf_typical_dsp::WfTypicalDspPlugin;
pub use win32_dialog::Win32DialogPlugin;
pub use win32ole::Win32OlePlugin;
pub use window_ex::WindowExPlugin;
pub use wuopus::WuOpusPlugin;
pub use wutcwf::WutcwfPlugin;
pub use wuvorbis::WuVorbisPlugin;
pub use xp3_filter::Xp3FilterPlugin;
pub use yuzuex::YuzuExPlugin;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginOrigin {
    Native,
    Plugin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginMapping {
    pub feature: &'static str,
    pub provider: PluginOrigin,
    pub plugin_name: Option<&'static str>,
    pub notes: &'static str,
}

/// Engine-owned runtime objects that are not plugins: every game gets them
/// from krkr-engine, so no `.dll` name selects them.
pub const NATIVE_MAPPINGS: &[PluginMapping] = &[PluginMapping {
    feature: "System / Storages / Scripts / KAGParser / Layer / Window",
    provider: PluginOrigin::Native,
    plugin_name: None,
    notes: "Core TVP/KRKR runtime objects stay in krkr-engine and are not modeled as external plugins.",
}];

pub fn register_reference_plugins(engine: &mut KrkrEngine) -> krkr_tjs2::Result<()> {
    register_profile_plugins(engine, &GameProfile::all())
}

/// Declares the compatibility capabilities a game actually needs. Hosts can
/// construct this from a package manifest or a known title profile instead of
/// initialising every plugin on every platform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameProfile {
    plugins: BTreeSet<String>,
    allow_all: bool,
}

impl Default for GameProfile {
    fn default() -> Self {
        Self {
            plugins: BTreeSet::new(),
            allow_all: true,
        }
    }
}

impl GameProfile {
    /// Every catalog entry.
    pub fn all() -> Self {
        Self {
            plugins: default_plugin_names().map(str::to_string).collect(),
            allow_all: true,
        }
    }

    /// The plugins PARQUET itself ships (see
    /// [`catalog::PARQUET_PLUGIN_FILES`]).
    pub fn parquet() -> Self {
        Self::only(parquet_plugin_names())
    }

    /// Only the named plugins, in any spelling (see
    /// [`catalog::resolve`]).
    pub fn only<I, S>(plugins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            plugins: plugins.into_iter().map(Into::into).collect(),
            allow_all: false,
        }
    }

    /// True when `plugin_name` is enabled. Names resolve through the catalog,
    /// so any spelling of a plugin — canonical name or alias, any case —
    /// selects the same plugin.
    pub fn enables(&self, plugin_name: &str) -> bool {
        self.allow_all
            || self
                .plugins
                .iter()
                .any(|selected| is_same_plugin(selected, plugin_name))
    }

    pub fn plugins(&self) -> impl Iterator<Item = &str> {
        self.plugins.iter().map(String::as_str)
    }
}

/// Installs every catalog entry the profile enables, in catalog order.
pub fn register_profile_plugins(
    engine: &mut KrkrEngine,
    profile: &GameProfile,
) -> krkr_tjs2::Result<()> {
    for entry in CATALOG {
        if profile.enables(entry.name) {
            install_plugin(engine, entry)?;
        }
    }
    Ok(())
}

pub fn default_plugin_names() -> impl Iterator<Item = &'static str> {
    CATALOG.iter().map(|entry| entry.name)
}

/// The plugins whose DLLs ship option descriptors (`GetOptionDesc`), with the
/// category list each one declares — the crate's stand-in for the resource
/// `IDR_OPTION_DESC_JSON` that `TVPGetPluginCommandDesc` reads out of a loaded
/// DLL. `yuzuex.dll` embeds the same three categories as `kagexopt.dll`; it is
/// deliberately absent here because it does not carry a second copy of the
/// table (`yuzuex.rs` consumes this one), which the no-duplicate-category test
/// below pins.
pub const OPTION_DESCRIPTOR_PROVIDERS: &[(&str, &[OptionCategory])] = &[
    ("kagexopt.dll", kagexopt::OPTION_CATEGORIES),
    ("krmovie.dll", krmovie::OPTION_CATEGORIES),
];

/// Every option category the engine's **linked** plugins declare, in provider
/// order, de-duplicated by category name the way the reference engine merges
/// the DLLs' JSON (`TVPMargeCommandDesc`,
/// `krkrz/msg/win32/ReadOptionDesc.cpp:260-288`, which appends a
/// second declaration's options to the first category of that name).
///
/// Linking is what gates a descriptor in the reference too: the option dialog
/// only sees the DLLs it finds on disk (`ConfigFormUnit::LoadPluginOptionDesc`,
/// `krkrz/environ/win32/ConfigFormUnit.cpp:127-145`). A profile that leaves
/// `kagexopt.dll` out (`GameProfile::only`) gets no 拡張ウィンドウ制御 group, so
/// a shell driving its option surface from here cannot offer — or apply — an
/// option the game's install does not ship.
pub fn linked_option_categories(engine: &KrkrEngine) -> Vec<&'static OptionCategory> {
    let linked: BTreeSet<&str> = engine.host().linked_plugins().collect();
    let mut categories: Vec<&'static OptionCategory> = Vec::new();
    for (plugin, declared) in OPTION_DESCRIPTOR_PROVIDERS {
        if !linked.contains(*plugin) {
            continue;
        }
        for category in *declared {
            if categories.iter().any(|seen| seen.name == category.name) {
                continue;
            }
            categories.push(category);
        }
    }
    categories
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_profile_is_explicitly_empty_while_default_keeps_compatibility() {
        assert!(GameProfile::default().enables("json.dll"));
        assert!(!GameProfile::only(["addFont.dll"]).enables("json.dll"));
        assert!(!GameProfile::only(std::iter::empty::<&str>()).enables("json.dll"));
    }

    #[test]
    fn a_profile_selects_a_plugin_by_any_of_its_names() {
        for alias in [
            "textrender.dll",
            "textRender.dll",
            "TextRender.dll",
            "TEXTRENDER.DLL",
        ] {
            assert!(
                GameProfile::only([alias]).enables("textrender.dll"),
                "{alias} does not select the textrender entry"
            );
            assert!(
                GameProfile::only(["textrender.dll"]).enables(alias),
                "textrender.dll does not select {alias}"
            );
        }
        assert!(GameProfile::only(["motionplayer_nod3d.dll"]).enables("motionplayer.dll"));
        assert!(GameProfile::only(["saveStruct.dll"]).enables("savestruct.dll"));
    }

    #[test]
    fn the_parquet_profile_covers_every_shipped_dll() {
        let profile = GameProfile::parquet();
        for name in PARQUET_PLUGIN_FILES {
            let entry = resolve(name).expect("catalog entry");
            assert!(
                profile.enables(entry.name),
                "{name} is shipped by PARQUET but not enabled by the parquet profile"
            );
        }
        assert!(!profile.enables("sqlite3.dll"));
        assert!(profile.enables("krmovie.dll"));
    }

    #[test]
    fn the_full_profile_covers_the_whole_catalog() {
        let profile = GameProfile::all();
        for name in default_plugin_names() {
            assert!(
                profile.enables(name),
                "{name} missing from the full profile"
            );
        }
        assert_eq!(default_plugin_names().count(), CATALOG.len());
    }

    /// The merge in [`linked_option_categories`] keeps the first category of a
    /// name, while `TVPMargeCommandDesc` appends a second declaration's
    /// options to it. With one declaration per category name the two agree;
    /// this pins that they still do, so a future provider that ships a
    /// duplicate category cannot silently lose its options.
    #[test]
    fn no_two_providers_declare_the_same_category_name() {
        let mut seen: Vec<&str> = Vec::new();
        for (plugin, categories) in OPTION_DESCRIPTOR_PROVIDERS {
            for category in *categories {
                assert!(
                    !seen.contains(&category.name),
                    "{plugin} declares a second `{}` category",
                    category.name
                );
                seen.push(category.name);
            }
        }
    }

    #[test]
    fn linked_option_categories_follow_what_the_profile_linked() {
        let mut engine = KrkrEngine::new(krkr_engine::EngineConfig::default()).expect("engine");
        // Nothing linked: the option dialog in the reference sees no DLL, so
        // there is nothing to render or apply.
        assert!(linked_option_categories(&engine).is_empty());

        register_profile_plugins(&mut engine, &GameProfile::only(["krmovie.dll"]))
            .expect("plugins");
        let categories = linked_option_categories(&engine);
        assert_eq!(
            categories
                .iter()
                .map(|category| category.name)
                .collect::<Vec<_>>(),
            ["デバッグ"],
            "kagexopt.dll is not linked, so its three categories must not appear"
        );

        register_reference_plugins(&mut engine).expect("plugins");
        assert_eq!(
            linked_option_categories(&engine)
                .iter()
                .map(|category| category.name)
                .collect::<Vec<_>>(),
            ["ゲーム全般", "ムービー", "拡張ウィンドウ制御", "デバッグ"],
            "the full profile links both descriptor providers, in provider order"
        );
    }
}
