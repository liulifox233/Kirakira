//! extNagano.dll marker plugin.
//!
//! The real plugin provides the transition methods `zoomfade`, `blurfade`,
//! `scanline`, `3duniversal`, `rgbfade`, `spin`, `flutter`, `imagewipe`,
//! `book`, `honeyturn`, `morphing`, and `multiripple`. krkr-core's
//! `TransitionMethod` maps these unknown names to crossfade silently, which is
//! the intended stub behavior: scripts keep working while the visual effect is
//! degraded. The plugin exposes no TJS surface, so registration is a no-op.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

pub struct ExtNaganoPlugin;

impl KrkrPlugin for ExtNaganoPlugin {
    fn name(&self) -> &str {
        "extNagano.dll"
    }

    fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        Ok(())
    }
}
