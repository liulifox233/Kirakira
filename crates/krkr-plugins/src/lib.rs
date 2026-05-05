mod add_font;
mod motion_player;

use krkr_engine::KrkrEngine;

pub use add_font::AddFontPlugin;
pub use motion_player::MotionPlayerPlugin;

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
        feature: "System / Storages / Scripts / KAGParser / Layer / Window",
        provider: PluginOrigin::Native,
        plugin_name: None,
        notes: "Core TVP/KRKR runtime objects stay in krkr-engine and are not modeled as external plugins.",
    },
];

pub fn register_reference_plugins(engine: &mut KrkrEngine) -> krkr_tjs2::Result<()> {
    engine.register_plugin(AddFontPlugin)?;
    engine.register_plugin(MotionPlayerPlugin)?;
    Ok(())
}

pub fn default_plugin_names() -> impl Iterator<Item = &'static str> {
    IMPLEMENTED_MAPPINGS
        .iter()
        .filter_map(|mapping| mapping.plugin_name)
}
