//! lzfs.dll marker plugin.
//!
//! The real plugin adds support for reading lzfs-packed archives. It has no
//! TJS surface — it only extends archive resolution — and the reference game
//! (GINKA) ships no `.lzfs` archives, so there is nothing to register.
//! Registration is a no-op.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

pub struct LzfsPlugin;

impl KrkrPlugin for LzfsPlugin {
    fn name(&self) -> &str {
        "lzfs.dll"
    }

    fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        Ok(())
    }
}
