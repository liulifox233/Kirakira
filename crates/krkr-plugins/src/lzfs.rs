//! lzfs.dll marker plugin.
//!
//! The real plugin adds support for reading lzfs-packed archives. It has no
//! TJS surface — it only extends archive resolution — and the reference game
//! (GINKA) ships no `.lzfs` archives, so there is nothing to register.
//! Registration is a no-op.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "lzfs archive support",
    notes: "Marker: no TJS surface, and the engine has no lzfs reader yet, so .lzfs archives stay unreadable.",
    install: |engine| engine.register_plugin(LzfsPlugin),
};

pub struct LzfsPlugin;

impl KrkrPlugin for LzfsPlugin {
    fn name(&self) -> &str {
        "lzfs.dll"
    }

    fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        Ok(())
    }
}
