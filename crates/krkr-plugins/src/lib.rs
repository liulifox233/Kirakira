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
        feature: "CSVParser, Storages.saveOctet, System.getOSVersion, Layer effects",
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
    engine.register_plugin(AddFontPlugin)?;
    engine.register_plugin(MotionPlayerPlugin)?;
    engine.register_plugin(Win32DialogPlugin)?;
    engine.register_plugin(WindowExPlugin)?;
    engine.register_plugin(JsonPlugin)?;
    engine.register_plugin(PackinOnePlugin)?;
    engine.register_plugin(LayerExDrawPlugin)?;
    engine.register_plugin(TextRenderPlugin)?;
    engine.register_plugin(PsbFilePlugin)?;
    engine.register_plugin(AlphaMoviePlugin)?;
    engine.register_plugin(GetSamplePlugin)?;
    engine.register_plugin(KagParserExPlugin)?;
    engine.register_plugin(ExtransPlugin)?;
    engine.register_plugin(ExtNaganoPlugin)?;
    engine.register_plugin(LzfsPlugin)?;
    Ok(())
}

pub fn default_plugin_names() -> impl Iterator<Item = &'static str> {
    IMPLEMENTED_MAPPINGS
        .iter()
        .filter_map(|mapping| mapping.plugin_name)
}
