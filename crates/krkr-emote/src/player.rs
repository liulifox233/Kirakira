//! A live player session over a loaded [`Motion`].
//!
//! [`Motion::draw_list`] samples the file's *static* scene constructor: the
//! authored frame at a tick, with only the variables the caller names. The
//! reference's `MEmotePlayer` is a stateful session instead — it seeds the
//! file's authored variable table, runs the per-tick control pass
//! (`EPEyeControl`'s blink timer, `EPEyebrowControl`, `EPMouthControl`), plays
//! timelines and feeds the *evaluated* variables back into the next scene
//! build. [`MotionPlayer`] is that session, over the same adapted PSB the rest
//! of this crate samples.
//!
//! eluna already carries the recovered machinery (`ElunaPlayer`,
//! `collect_emote_variables`, `collect_emote_timelines`,
//! `collect_emote_runtime_pipeline`); nothing in this repository reached it
//! before this module, so a parameterised layer was always sampled from an
//! empty variable table and the face/control pass never ran.
//!
//! ```no_run
//! use krkr_emote::{Motion, MotionPlayer};
//!
//! fn main() -> Result<(), krkr_emote::MotionError> {
//!     let bytes = std::fs::read("motion/sd102.mtn").expect("read the .mtn file");
//!     let motion = Motion::from_bytes(&bytes)?;
//!     let mut player = MotionPlayer::with_motion(&motion, "SD102AA")?;
//!     player.advance_ticks(1.0 / 60.0 * 60.0);
//!     for item in player.draw_list() {
//!         println!("{} at {:?}", item.texture, item.center);
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # The reference's player-side surface, and what eluna has of it
//!
//! Read from the shipping binaries rather than from documentation: the
//! `motionplayer_nod3d.dll` Ghidra export (one `.c` per function plus
//! `strings.tsv`), the D3D build's disassembly (`motionplayer.dll`, image base
//! `0x10000000`), and eluna's own recovery, whose comments cite the DLL `sub_`
//! addresses per facility. The same DLLs are what the plugin in
//! `crates/krkr-plugins/src/motion_player.rs` was built against, so that
//! module's member surface is the cross-check for the TJS side.
//!
//! **Per tick, in the reference:** `MEmotePlayer::Init` parses the PSB
//! `metadata` once — `variableList`, `instantVariableList`, `eyeControl`,
//! `eyebrowControl`, `mouthControl`, `partsControl`, `bustControl`,
//! `hairControl`, `transitionControl`, `selectorControl`, `loopControl`,
//! `clampControl`, `mirrorControl`, `stereovisionControl`, `timelineControl`
//! (nod3d string pool; eluna's parse is `collect_emote_runtime_pipeline`,
//! `vendor/eluna/crates/eluna/src/runtime.rs:3265-3314`). Then `sub_10268A30`
//! runs each controller's `Step` in a fixed-step substep loop: `EPEyeControl`
//! is `sub_101E2970` with the blink timer, `EPEyebrowControl` is `sub_101E9B00`
//! (nod3d strings `blinkIntervalMin` `0x100e2edc`, `blinkSuspend` `0x100e3720`,
//! `convergeEyeControl` `0x100e3228`), and the controller's output is written
//! into the variable table, so a control *is* a variable the sampler reads.
//! Timelines (`playTimeline`/`setTimelineBlendRatio`/`fadeIn`/`fadeOut`/
//! `stopTimeline`, difference tracks blended over the base) and the physics
//! surface (`initPhysics`, `startWind`/`stopWind`, `setOuterForce`,
//! `setRotate`) run in the same pass.
//!
//! **Per frame, in the reference:** the scene build samples a parameterised
//! layer at its variable's time (`sub_1032FB00`; eluna
//! `vendor/eluna/crates/eluna/src/emote.rs:4620-4706`) and each decoded frame
//! carries `bm`, `bp`, four corner colours, `opa` and the visibility flag
//! (eluna's `EmoteStaticSprite`, `emote.rs:322-359`, from `sub_1033D0E0`).
//! Those two per-sprite fields are what the renderer needs: `bm`'s low nibble
//! selects the blend equation — the D3D build indexes a 7-entry
//! `(BLENDOP, DESTBLEND, SRCBLEND)` table at VA `0x1012a980` with it
//! (`motionplayer.dll` `0x10006c40`, fed by the sprite record's `+0xc4` at
//! `1004731c`), and the no-D3D build selects the same operation as a TVP layer
//! type with the same nibble (`motionplayer_nod3d.dll`,
//! `1003a850_FUN_1003a850.c:393-406`) — while `bm & 0xF0 == 0x10` selects the
//! MODULATE2X texture-colour stage, whose neutral corner colour is `0x808080`
//! (`0x10006c68`; eluna's `native_white_color_fallback`, `emote.rs:4396-4400`).
//! Both tables and the compositing math they imply live on
//! [`crate::render::SpriteBlend`] and in `crate::render`'s module docs.
//!
//! **eluna implements** every one of the above: the metadata parse
//! (`runtime.rs:3265`, `:5484`), the eye/brow/mouth controls with the blink
//! state machine (`evaluate_eye_control` `:4373`, `:4456`, `:4469`), the
//! selector/transition/loop/clamp controllers (`:3795`, `:4858`, `:4649`), the
//! timeline lifecycle (`:1945`, `:2563-2693`), variables (`:1238`, `:2518`,
//! `:2534`), physics/wind/outer force (`:1222`, `:2694`, `:2777`), the frame
//! decode with `bm`/`bp`/corner colours (`emote.rs:322-359`) and the
//! evaluated-variable scene rebuild (`eluna::sdk::EmoteRuntime`, `sdk.rs:389-433`).
//! What a host still gets from `setColor`/`setGrayscale`/`setMeshDivisionRatio`/
//! `setTransformOrderMask` is on the same session through [`MotionPlayer::inner`].
//!
//! **What the DLL has and eluna does not** — the list a follow-up mission
//! needs, since none of it is in this crate's reach either:
//!
//! 1. the **per-part sub-layer model**: `SeparateLayerAdaptor.getSubImageLayers()`
//!    plus `Player.LayerGetter`/`LayerSetter` hand each part to its own TVP
//!    layer (nod3d `100385e0_FUN_100385e0.c`; the game branches on the member
//!    answering void, `AffineSourceMotion.tjs:3237`), while eluna produces one
//!    flat sprite list with no part-to-layer mapping;
//! 2. the **script callbacks** `onAction` / `onSync` / `onFindMotion` (the
//!    `Player` members this crate's plugin registers as declared stubs,
//!    `crates/krkr-plugins/src/motion_player.rs`) — eluna records and replays an
//!    *API log* (`record_api_log`/`replay_api_log_once`) but never invokes a
//!    host callback;
//! 3. the **sync/completion surface** `syncActive` / `syncWaiting` /
//!    `skipToSync` / `completionType` / `independentLayerInherit` / `tags` /
//!    `motionKey` (`eluna::emote_runtime_parity_report`, `sdk.rs:238-275`,
//!    lists the same gap as "binary-compatible IEmotePlayer/PEmotePlayer ABI");
//! 4. the **D3D path** — `useD3D`, `D3DAdaptor`, `alphaOpAdd`,
//!    `pixelateDivision`, `captureCanvas`, the camera and the stereoscopic
//!    display compositor; eluna recovers the metadata and the per-screen
//!    values, the rasteriser stays a host responsibility by design;
//! 5. **thread tasks** — the DLL decodes and updates inside
//!    `TVPBeginThreadTask`/`TVPExecThreadTask`; eluna is single-threaded;
//! 6. **host RNG parity** — the particle spawn's random ranges are recovered,
//!    the host RNG state they draw from is not serialized (`sdk.rs:264-266`);
//! 7. **`.psb` model playback (type 6)** — the state is recovered, loading and
//!    drawing `referenceModelFileList` needs a host 3-D backend.
//!
//! # What PARQUET's corpus actually uses
//!
//! Measured over all 23 `.mtn` members of the game's `data.xp3`
//! (`examples/motion_probe.rs`, `MOTION_PROBE_SCAN=1`): **every**
//! eye/eyebrow/mouth control list, every timeline and every physics control is
//! empty. The only authored variables are `language` (0..3) in five `sd*`
//! members, with one or two parameterised layers each (the localised-text
//! layers); the interesting render state is the per-sprite `bm` (the `0x10`
//! default; `0x00` in `m2logo`/`splash`/`yuzulogo`; an additive `0x01` sprite
//! in `m2logo`; `0x11` in `title_bg`; `0x13` on `sd101`'s haze layer, seven
//! sprite instances over the six sampled ticks in each copy) and the authored
//! corner colours (`m2logo`, `title_bg`, `yuzusourlogo`). So the reference's
//! face/control pipeline has nothing to resolve here, and a frame difference
//! between two SD animations is authored as different face *layers* — which
//! [`Motion::draw_list`] already resolves.

use std::collections::BTreeMap;

use eluna::{
    ElunaPlayer, EmotePlayerControl, EmoteStaticScene, TimelinePlayMode,
    collect_emote_runtime_pipeline, collect_emote_timelines, collect_emote_variables,
};

use crate::error::MotionError;
use crate::model::MotionDrawItem;
use crate::motion::Motion;

/// A live E-mote player: one `.mtn`, one active animation, and the runtime
/// state the reference keeps across ticks.
///
/// The session owns the adapted `PsbFile`/schema pair the rest of the crate
/// uses, so the PARQUET-flavor normalisation applies to everything it samples
/// (`eluna::sdk::EmoteRuntime` cannot be reused here: it re-parses the raw
/// bytes without the adapter's pass).
#[derive(Clone)]
pub struct MotionPlayer {
    schema: eluna::EmoteModelSchema,
    psb: eluna::PsbFile,
    normalized_data: Vec<u8>,
    player: ElunaPlayer,
    motion: String,
    /// The active animation's own time: the tick the scene is sampled at, cut
    /// back to 0 by [`MotionPlayer::set_motion`]. [`ElunaPlayer`] keeps one
    /// clock for both the player's controls/timelines and the motion sample,
    /// so the wrapper carries the animation's time next to it; see
    /// [`MotionPlayer::elapsed_ticks`].
    motion_ticks: f32,
}

impl MotionPlayer {
    /// Opens a session on the file's default animation — the first motion the
    /// schema lists, as `eluna::sdk::EmoteRuntime` opens it.
    pub fn new(motion: &Motion) -> Result<Self, MotionError> {
        let name = motion
            .schema()
            .default_motion_name(motion.psb())?
            .or_else(|| {
                motion
                    .animations()
                    .first()
                    .map(|animation| animation.name.clone())
            })
            .ok_or_else(|| MotionError::MissingAnimation("<default>".to_owned()))?;
        Self::with_motion(motion, &name)
    }

    /// Opens a session playing `animation`.
    pub fn with_motion(motion: &Motion, animation: &str) -> Result<Self, MotionError> {
        if motion.animation(animation).is_none() {
            return Err(MotionError::MissingAnimation(animation.to_owned()));
        }
        let psb = motion.psb().clone();
        let normalized_data = motion.psb_bytes().to_vec();
        let variables = collect_emote_variables(&psb);
        let timelines = collect_emote_timelines(&psb);
        let pipeline = collect_emote_runtime_pipeline(&psb);
        // The reference seeds the authored variable table before the first
        // frame (`EmoteRuntime::from_bytes`, `vendor/eluna/crates/eluna/src/sdk.rs:310-315`).
        let initial: BTreeMap<String, f32> = variables
            .iter()
            .map(|variable| (variable.name.clone(), variable.default_value))
            .collect();
        let scene = motion
            .schema()
            .build_motion_scene_at_with_resources_and_variables(
                &psb,
                &normalized_data,
                animation,
                0.0,
                &initial,
            )?;
        let player = ElunaPlayer::from_scene_variables_timelines_runtime(
            scene, variables, timelines, pipeline,
        );
        let mut session = Self {
            schema: motion.schema().clone(),
            psb,
            normalized_data,
            player,
            motion: animation.to_owned(),
            motion_ticks: 0.0,
        };
        session.rebuild()?;
        Ok(session)
    }

    /// The animation this session plays.
    pub fn motion(&self) -> &str {
        &self.motion
    }

    /// Switches animation the way the reference switches motions: the new one
    /// starts at its own time 0, and the session's player state — variables,
    /// control timers, timelines — keeps running.
    ///
    /// The switch is `play(name, flags)` in the reference, not a property
    /// write: the game only ever *reads* `_player.motion`
    /// (`AffineSourceMotion.tjs:545,560`; decompiled from the game's archives,
    /// as the rest of this file's script citations are) and changes motion with
    /// `_player.play(a0.motion, l2)` (`:2564`, and `:256`), and this crate's
    /// plugin implements `play` as "put the player at tick 0 and start it"
    /// (`crates/krkr-plugins/src/motion_player.rs:2152-2156`), keeping the
    /// player's variables — which is what this method does.
    ///
    /// *Verified*: the game's call shape and the plugin's tick-0 semantics.
    /// *Inferred*: that the native player likewise keeps one clock for the
    /// player and another for the motion's own time — eluna exposes a single
    /// `elapsed_ticks`, so this wrapper carries the animation's time itself.
    pub fn set_motion(&mut self, animation: &str) -> Result<(), MotionError> {
        if self
            .schema
            .motion_infos(&self.psb)?
            .iter()
            .all(|info| info.name != animation)
        {
            return Err(MotionError::MissingAnimation(animation.to_owned()));
        }
        self.motion = animation.to_owned();
        self.motion_ticks = 0.0;
        self.player.skip();
        self.rebuild()
    }

    /// How long the active animation runs, in ticks.
    pub fn duration_ticks(&self) -> f32 {
        self.schema
            .motion_infos(&self.psb)
            .ok()
            .and_then(|infos| {
                infos
                    .into_iter()
                    .find(|info| info.name == self.motion)
                    .map(|info| info.duration_ticks)
            })
            .unwrap_or(0.0)
    }

    /// How far into the active animation the session is, in ticks — the time
    /// its frames are sampled at. [`MotionPlayer::set_motion`] puts it back to
    /// 0; the session's control timers and timelines are *not* rewound (they
    /// run on the player's own clock, as in the reference).
    pub fn elapsed_ticks(&self) -> f32 {
        self.motion_ticks
    }

    /// Advances the session by `delta_ticks` on the reference's 1/60 s tick
    /// axis: the player's clock, its controls and every playing timeline move
    /// together, and the scene is re-sampled from the evaluated variables.
    pub fn advance_ticks(&mut self, delta_ticks: f32) -> Result<(), MotionError> {
        let delta = if delta_ticks.is_finite() {
            delta_ticks.max(0.0)
        } else {
            0.0
        };
        self.motion_ticks += delta;
        self.player.progress_ticks_without_physics(delta);
        self.rebuild_with_physics(delta)
    }

    /// [`MotionPlayer::advance_ticks`] from milliseconds, capped the way the
    /// official JS driver caps one animation-frame delta
    /// (`EMOTE_UPDATE_MS_CAP`).
    pub fn advance_milliseconds(&mut self, delta_ms: f32) -> Result<(), MotionError> {
        let capped = if delta_ms.is_finite() {
            delta_ms.clamp(0.0, eluna::EMOTE_UPDATE_MS_CAP)
        } else {
            0.0
        };
        self.advance_ticks(eluna::milliseconds_to_emote_ticks(capped))
    }

    /// [`MotionPlayer::advance_ticks`] from seconds.
    pub fn advance_seconds(&mut self, delta_seconds: f32) -> Result<(), MotionError> {
        self.advance_ticks(delta_seconds * eluna::EMOTE_TICKS_PER_SECOND)
    }

    /// The scene the last tick produced.
    pub fn scene(&self) -> &EmoteStaticScene {
        self.player.scene()
    }

    /// The current frame's draw list, in draw order.
    pub fn draw_list(&self) -> Vec<MotionDrawItem> {
        self.scene()
            .sprites
            .iter()
            .map(MotionDrawItem::from_sprite)
            .collect()
    }

    /// Every variable the player knows, with its *evaluated* value — the
    /// authored default until a control, a timeline or a `setVariable` moves
    /// it, and the control output after that.
    pub fn variables(&self) -> BTreeMap<String, f32> {
        self.player.evaluated_variable_values()
    }

    /// One evaluated variable value.
    pub fn variable(&self, name: &str) -> Option<f32> {
        self.player.variable_value(name)
    }

    /// `SetVariable(name, value)` — the reference's immediate write.
    pub fn set_variable(&mut self, name: &str, value: f32) -> Result<(), MotionError> {
        self.player.set_variable_immediate(name, value);
        self.rebuild()
    }

    /// `SetVariable(name, value, time, easing)` — a timed transition.
    pub fn set_variable_timed(
        &mut self,
        name: &str,
        value: f32,
        time_ticks: f32,
        easing: f32,
    ) -> Result<(), MotionError> {
        self.player
            .set_variable_timed(name, value, time_ticks, easing);
        self.rebuild()
    }

    /// The file's timeline names, in file order.
    pub fn timeline_names(&self) -> Vec<&str> {
        self.player.timelines().keys().map(String::as_str).collect()
    }

    /// `PlayTimeline(name, flags)`.
    pub fn play_timeline(&mut self, name: &str, mode: TimelinePlayMode) -> Result<(), MotionError> {
        if !self.player.timelines().contains_key(name) {
            return Err(MotionError::MissingTimeline(name.to_owned()));
        }
        self.player.play_timeline(name, mode);
        self.rebuild()
    }

    /// `StopTimeline(name)`.
    pub fn stop_timeline(&mut self, name: &str) -> Result<(), MotionError> {
        self.player.stop_timeline(name);
        self.rebuild()
    }

    /// Whether a timeline is currently playing.
    pub fn is_timeline_playing(&self, name: &str) -> bool {
        self.player.is_timeline_playing(name)
    }

    /// The timelines the reference calls the "main" ones (non-difference).
    pub fn main_timeline_labels(&self) -> Vec<&str> {
        self.player.main_timeline_labels()
    }

    /// The difference timelines — the face/costume variants a player blends
    /// over the main one.
    pub fn diff_timeline_labels(&self) -> Vec<&str> {
        self.player.diff_timeline_labels()
    }

    /// The underlying eluna session, for the parts of the player surface this
    /// wrapper does not name (physics, stereovision, mirroring, …).
    pub fn inner(&self) -> &ElunaPlayer {
        &self.player
    }

    /// Mutable access to the underlying eluna session — for the parts of the
    /// player surface this wrapper does not name (physics, stereovision,
    /// mirroring, …).
    ///
    /// One caveat for the session-shaping knobs: `ElunaPlayer::set_paused(true)`
    /// freezes the control pass inside eluna (`progress_ticks_internal` returns
    /// before `evaluate_runtime_pipeline`,
    /// `vendor/eluna/crates/eluna/src/runtime.rs:2313-2315`), while
    /// [`MotionPlayer::advance_ticks`] still moves the animation clock — so a
    /// paused session keeps drawing new frames of the animation with its face
    /// controllers frozen, not a frozen picture. Nothing in this repository
    /// pauses a session today; a plugin that wires `Player.pause` has to decide
    /// between pausing the whole session (stop calling `advance_ticks`) and
    /// eluna's narrower freeze.
    pub fn inner_mut(&mut self) -> &mut ElunaPlayer {
        &mut self.player
    }

    /// Re-samples the scene from the current player state, exactly as the
    /// reference's per-tick rebuild does (`EmoteRuntime::rebuild_scene`,
    /// `vendor/eluna/crates/eluna/src/sdk.rs:385-433`): the previous scene
    /// travels along because nested motions and type-0 HOLD frames read their
    /// previous positions from it.
    fn rebuild(&mut self) -> Result<(), MotionError> {
        self.rebuild_with_physics(0.0)
    }

    fn rebuild_with_physics(&mut self, physics_delta_ticks: f32) -> Result<(), MotionError> {
        let previous = self.player.scene().clone();
        let build = |player: &ElunaPlayer,
                     schema: &eluna::EmoteModelSchema,
                     psb: &eluna::PsbFile,
                     data: &[u8],
                     motion: &str,
                     motion_ticks: f32,
                     previous: &EmoteStaticScene| {
            schema.build_motion_scene_at_with_resources_variables_previous_scene_and_ground_hook(
                psb,
                data,
                motion,
                motion_ticks,
                &player.evaluated_variable_values(),
                previous,
                None,
            )
        };
        let scene = build(
            &self.player,
            &self.schema,
            &self.psb,
            &self.normalized_data,
            &self.motion,
            self.motion_ticks,
            &previous,
        )?;
        self.player.replace_scene(scene);
        if physics_delta_ticks > 0.0 && self.player.is_physics_enabled() {
            self.player
                .evaluate_physics_for_current_scene(physics_delta_ticks);
            let scene = build(
                &self.player,
                &self.schema,
                &self.psb,
                &self.normalized_data,
                &self.motion,
                self.motion_ticks,
                &previous,
            )?;
            self.player.replace_scene(scene);
        }
        Ok(())
    }
}
