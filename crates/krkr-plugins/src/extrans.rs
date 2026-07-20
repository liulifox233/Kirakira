//! extrans.dll marker plugin.
//!
//! The real plugin provides the transition methods `wave`, `mosaic`, `turn`,
//! `rotatezoom`, `rotatevanish`, `rotateswap`, and `ripple`. Those names are
//! already recognized by krkr-core's `TransitionMethod`, and any transition
//! name without a dedicated implementation maps to crossfade silently — the
//! intended stub behavior, so scripts keep working while the visual effect is
//! degraded. The plugin exposes no TJS surface, so registration is a no-op.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

pub struct ExtransPlugin;

impl KrkrPlugin for ExtransPlugin {
    fn name(&self) -> &str {
        "extrans.dll"
    }

    fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        Ok(())
    }
}
