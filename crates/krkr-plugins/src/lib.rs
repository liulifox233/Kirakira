mod add_font;
mod alpha_movie;
mod extnagano;
mod extrans;
mod get_sample;
mod json;
mod kag_parser_ex;
mod layer_ex_draw;
mod lzfs;
mod motion_player;
mod packinone;
mod psb_file;
mod text_render;
mod win32_dialog;
mod window_ex;

use std::collections::BTreeSet;

use krkr_engine::KrkrEngine;

pub use add_font::AddFontPlugin;
pub use alpha_movie::AlphaMoviePlugin;
pub use extnagano::ExtNaganoPlugin;
pub use extrans::ExtransPlugin;
pub use get_sample::GetSamplePlugin;
pub use json::JsonPlugin;
pub use kag_parser_ex::KagParserExPlugin;
pub use layer_ex_draw::LayerExDrawPlugin;
pub use lzfs::LzfsPlugin;
pub use motion_player::MotionPlayerPlugin;
pub use packinone::PackinOnePlugin;
pub use psb_file::PsbFilePlugin;
#[doc(hidden)]
pub use psb_file::{PsbValue, debug_parse_psb};
pub use text_render::TextRenderPlugin;
pub use win32_dialog::Win32DialogPlugin;
pub use window_ex::WindowExPlugin;

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

pub const IMPLEMENTED_MAPPINGS: &[PluginMapping] = &[
    PluginMapping {
        feature: "System.addFont",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("addFont.dll"),
        notes: "Registers fonts from game storage through the addFont compatibility plugin.",
    },
    PluginMapping {
        feature: "Motion / Motion.Player / Motion.EmotePlayer",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("motionplayer.dll"),
        notes: "Provides the current motionplayer compatibility shims that previously lived in krkr-engine.",
    },
    PluginMapping {
        feature: "WIN32Dialog",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("win32dialog.dll"),
        notes: "No-op Win32 dialog classes (WIN32Dialog plus Header/Items/Bitmap/SolidBrush/DrawItem/Notify/Blob subclasses) with the constant surface scripts reference.",
    },
    PluginMapping {
        feature: "Window/MenuItem/Pad/Debug.console/System/Scripts extensions",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("windowEx.dll"),
        notes: "No-op windowEx member surface attached to the existing engine classes.",
    },
    PluginMapping {
        feature: "Scripts.evalJSON / evalJSONStorage / saveJSON / toJSONString",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("json.dll"),
        notes: "Functional lenient JSON parser and serializer compatible with wtnbgo/json.",
    },
    PluginMapping {
        feature: "CSVParser, Scripts.loadDataPack, Storages.saveOctet, System.getOSVersion, Layer effects",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("PackinOne.dll"),
        notes: "Subset of the PackinOne bundle that games actually call; the rest is no-op surface.",
    },
    PluginMapping {
        feature: "Layer drawing methods / GdiPlus namespace",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("layerExDraw.dll"),
        notes: "No-op drawing surface; draw* methods return zeroed GdiPlus.RectF instances.",
    },
    PluginMapping {
        feature: "TextRenderBase",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("textrender.dll"),
        notes: "No-op text render surface; layout queries return conservative values.",
    },
    PluginMapping {
        feature: "PSBFile / PSBValueClass",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("psbfile.dll"),
        notes: "No-op PSB document surface; load validates storage readability only.",
    },
    PluginMapping {
        feature: "AlphaMovie",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("AlphaMovie.dll"),
        notes: "No-op alpha movie class; playback immediately reaches the finished state so polling wrappers terminate.",
    },
    PluginMapping {
        feature: "WaveSoundBuffer.getSample / sampleValue / sampleCount / sampleAhead",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("getSample.dll"),
        notes: "Silent-audio stub: sampleValue reads as 0.0 so lip-sync scripts stay idle.",
    },
    PluginMapping {
        feature: "KAGParser tag dictionaries expose taglist",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("KAGParserEx.dll"),
        notes: "The engine emits KAGParserEx-style taglist member lists unconditionally; the plugin itself is a marker.",
    },
    PluginMapping {
        feature: "wave / mosaic / turn / rotatezoom / rotatevanish / rotateswap / ripple transitions",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("extrans.dll"),
        notes: "Transition names currently degrade to crossfade in krkr-core; the plugin itself is a marker.",
    },
    PluginMapping {
        feature: "zoomfade / blurfade / scanline / 3duniversal / rgbfade / spin / flutter / imagewipe / book / honeyturn / morphing / multiripple transitions",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("extNagano.dll"),
        notes: "Transition names currently degrade to crossfade in krkr-core; the plugin itself is a marker.",
    },
    PluginMapping {
        feature: "lzfs archive support",
        provider: PluginOrigin::Plugin,
        plugin_name: Some("lzfs.dll"),
        notes: "No TJS surface; games without .lzfs archives need nothing. Marker only.",
    },
    PluginMapping {
        feature: "System / Storages / Scripts / KAGParser / Layer / Window",
        provider: PluginOrigin::Native,
        plugin_name: None,
        notes: "Core TVP/KRKR runtime objects stay in krkr-engine and are not modeled as external plugins.",
    },
];

pub fn register_reference_plugins(engine: &mut KrkrEngine) -> krkr_tjs2::Result<()> {
    register_profile_plugins(engine, &GameProfile::all())
}

/// Declares the compatibility capabilities a game actually needs. Hosts can
/// construct this from a package manifest or a known title profile instead of
/// linking/initialising every marker plugin on every platform.
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
    pub fn all() -> Self {
        Self {
            plugins: default_plugin_names().map(str::to_string).collect(),
            allow_all: true,
        }
    }

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

    pub fn enables(&self, plugin_name: &str) -> bool {
        self.allow_all || self.plugins.contains(plugin_name)
    }

    pub fn plugins(&self) -> impl Iterator<Item = &str> {
        self.plugins.iter().map(String::as_str)
    }
}

pub fn register_profile_plugins(
    engine: &mut KrkrEngine,
    profile: &GameProfile,
) -> krkr_tjs2::Result<()> {
    macro_rules! register_if {
        ($name:literal, $plugin:expr) => {
            if profile.enables($name) {
                engine.register_plugin($plugin)?;
            }
        };
    }
    register_if!("addFont.dll", AddFontPlugin);
    register_if!("motionplayer.dll", MotionPlayerPlugin);
    register_if!("win32dialog.dll", Win32DialogPlugin);
    register_if!("windowEx.dll", WindowExPlugin);
    register_if!("json.dll", JsonPlugin);
    register_if!("PackinOne.dll", PackinOnePlugin);
    register_if!("layerExDraw.dll", LayerExDrawPlugin);
    register_if!("textrender.dll", TextRenderPlugin);
    register_if!("psbfile.dll", PsbFilePlugin);
    register_if!("AlphaMovie.dll", AlphaMoviePlugin);
    register_if!("getSample.dll", GetSamplePlugin);
    register_if!("KAGParserEx.dll", KagParserExPlugin);
    register_if!("extrans.dll", ExtransPlugin);
    register_if!("extNagano.dll", ExtNaganoPlugin);
    register_if!("lzfs.dll", LzfsPlugin);
    Ok(())
}

pub fn default_plugin_names() -> impl Iterator<Item = &'static str> {
    IMPLEMENTED_MAPPINGS
        .iter()
        .filter_map(|mapping| mapping.plugin_name)
}

#[cfg(test)]
mod tests {
    use super::GameProfile;

    #[test]
    fn empty_profile_is_explicitly_empty_while_default_keeps_compatibility() {
        assert!(GameProfile::default().enables("json.dll"));
        assert!(!GameProfile::only(["addFont.dll"]).enables("json.dll"));
        assert!(!GameProfile::only(std::iter::empty::<&str>()).enables("json.dll"));
    }
}
