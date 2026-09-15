//! `emoteplayer.dll` — the E-mote player module, installed with the same TJS
//! surface as [`crate::motion_player`].
//!
//! PARQUET's `motion.tjs` links `emoteplayer.dll` *first* and falls back to
//! `motionplayer.dll` / `motionplayer_nod3d.dll` (`CanLoadPlugin` chain,
//! decompiled at `/tmp/m38/parquet/motion.tjs`); the game does not ship this
//! file, so its E-mote path runs through the motionplayer module. Both binaries
//! register the same classes — `Motion`, `Motion.Player`, `Motion.EmotePlayer`,
//! `Motion.ResourceManager`, `Motion.SeparateLayerAdaptor` (`emote::EP*`
//! RTTI in `motionplayer.dll`, M27 dossier `docs/plugins/motionplayer.md`) —
//! so this module reports the motionplayer surface rather than a placeholder:
//! a game that *does* ship `emoteplayer.dll` gets working `.mtn` playback
//! instead of a missing-member failure in the middle of its boot.
//!
//! The earlier note in this file described the plugin as "MPEG-based emote
//! playback"; that was wrong. E-mote is M2's character-animation SDK (motion
//! data in PSB containers), and no public source exists for either binary.
//!
//! Linking this file also claims `.mtn` in the engine's script image path: the
//! shared implementation registers the graphic loader
//! ([`krkr_engine::plugin_api::graphic`], the reference's
//! `TVPRegisterGraphicLoadingHandler`), so a game whose `CanLoadPlugin` chain
//! starts here gets working `.mtn` script images — PARQUET's title screen
//! (`custom.ks` `*title_start`) loads `title_bg.mtn` as a layer image and the
//! PARQUET logo lives in that motion.
//!
//! What is real and what is not is the motionplayer module's story, verbatim —
//! see [`crate::motion_player`] (the state machine, the storage loading and the
//! layer-path rendering are wired; physics, timelines, mesh deformation,
//! particles, separate-layer mode and `.psb` *model* playback are not, and each
//! such member logs a one-time warning).

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "EmotePlayer / Motion (the E-mote surface, shared with motionplayer.dll)",
    notes: "Same implementation as motionplayer.dll: this module installs the Motion / Motion.Player / \
            Motion.EmotePlayer / Motion.ResourceManager / Motion.SeparateLayerAdaptor surface through \
            crate::motion_player::install_motionplayer_compat, so a game whose CanLoadPlugin chain starts \
            here gets the wired .mtn path (load, play/progress/stop, variables, layer rendering) instead of \
            a missing class. The `.mtn` script-image loader is registered here too (the shared implementation \
            claims the extension the reference's driver does, so Layer.loadImages of a motion yields a live \
            frame). The gaps are motionplayer's: physics, timelines, mesh deformation, particles, \
            separate-layer mode and .psb model playback are stubs that warn once; see crate::motion_player.",
    install: |engine| engine.register_plugin(EmotePlayerPlugin),
};

pub struct EmotePlayerPlugin;

impl KrkrPlugin for EmotePlayerPlugin {
    fn name(&self) -> &str {
        "emoteplayer.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        crate::motion_player::install_motionplayer_compat(runtime);
        Ok(())
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The `.mtn` loader belongs to the shared motionplayer implementation
        // (`crate::motion_player`), which this alias only borrows. Unlinking
        // `emoteplayer.dll` must therefore not tear down the registration while
        // `motionplayer.dll` — the module the loader is named after and carries
        // the code for — is still linked; the shared rule drops it when the
        // last alias goes.
        crate::motion_player::unregister_motion_graphic_loader(runtime, self.name());
        Ok(())
    }
}
