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

pub struct KagParserExPlugin;

impl KrkrPlugin for KagParserExPlugin {
    fn name(&self) -> &str {
        "KAGParserEx.dll"
    }

    fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        Ok(())
    }
}
