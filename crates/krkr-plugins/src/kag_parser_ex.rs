//! KAGParserEx.dll marker plugin.
//!
//! The real plugin extends KAG tag dictionaries with extra parser metadata.
//! The only part games actually consume — the insertion-ordered `taglist`
//! member listing the tag name followed by every attribute name — is already
//! emitted unconditionally by the engine: see `tag_to_dictionary` in
//! `crates/krkr-engine/src/kag.rs`, which attaches `taglist` to every KAG tag
//! dictionary it builds. The remaining KAGParserEx surface (`paramMacros`,
//! `pmacro`, `multiLineTagEnabled`) is intentionally unimplemented because the
//! reference game (GINKA) only reads `taglist`. Registration is a no-op.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "KAGParser tag dictionaries expose taglist",
    notes: "Marker: the engine already attaches the ordered taglist to every KAG tag dictionary; paramMacros/pmacro/multiLineTagEnabled are not implemented.",
    install: |engine| engine.register_plugin(KagParserExPlugin),
};

pub struct KagParserExPlugin;

impl KrkrPlugin for KagParserExPlugin {
    fn name(&self) -> &str {
        "KAGParserEx.dll"
    }

    fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        Ok(())
    }
}
