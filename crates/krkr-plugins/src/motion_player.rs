//! `motionplayer.dll` — the E-mote "kiri-motion" krkr driver, wired to the
//! `krkr-emote` adapter.
//!
//! Real plugin: M2 Co., Ltd.'s E-mote player for KiriKiri (`emote::EP*` RTTI,
//! embedded PSB reader, no public source). The reference tables this module is
//! built from are the M27 dossier (`docs/plugins/motionplayer.md`) and the
//! game-side call sites decompiled in M38
//! (`/tmp/m38/AffineSourceMotion.decomp.tjs`, `/tmp/m38/parquet/motion.tjs`).
//! PARQUET drives it as: `Motion.ResourceManager(path, cacheSize)` →
//! `load(file)` → `new Motion.Player(resourceManager)` → `play(motionName,
//! flags)` → `progress(milliseconds)` per frame → `draw(workLayer)` → the
//! game's KAG wrapper copies the work layer onto the visible layer.
//!
//! # What is real here
//!
//! * **Loading** — `ResourceManager.load` (and the class-object spelling
//!   `Motion.ResourceManager.load`) read the storage file
//!   and parse it through [`krkr_emote::Motion`] (eluna's PSB reader plus the
//!   PARQUET-flavor adaptation). The handle the game gets back carries
//!   `.metadata`, which its wrapper requires (`!l2.metadata === void`).
//! * **The `.mtn` graphic loader** — the module claims `.mtn` in the engine's
//!   script image path ([`krkr_engine::plugin_api::graphic`], the counterpart
//!   of the reference's `TVPRegisterGraphicLoadingHandler`), so
//!   `Layer.loadImages`/`System.touchImages` of a motion resolve to a real
//!   bitmap instead of "The image format could not be determined". The frame is
//!   the file's own root `screenSize` with the content centred on it, and it
//!   stays **live**: the engine's frame clock advances the animation and swaps
//!   the pixels of every layer showing it. PARQUET's title screen is exactly
//!   that — `custom.ks` `*title_start`'s motion branch loads `title_bg.mtn` as
//!   a layer image, and the logo lives in that motion.
//! * **The state machine** — `play`/`stop`/`progress`/`frameProgress`/`skip`/
//!   `skipToSync`, `speed`, `tickCount`, `playing`, `loopTime`/`lastTime`, and
//!   `setVariable(name, value[, time, easing])` with linear/smoothstep timed
//!   writes are driven by the adapter's own model; a motion that does not loop
//!   stops at its duration and clears `playing`.
//! * **The time model (settled against the DLL)** — the plain time members are
//!   milliseconds-facing and the `frame*` family is raw 1/60 s ticks, exactly
//!   like the reference: `progress`'s handler converts with `×60/1000`
//!   (`0x10030290`), `frameProgress` takes the raw value (`0x10030370`),
//!   `tickCount`'s getter/setter are `pos×1000/60` / `v×60/1000`
//!   (`FUN_10045c00`/`FUN_10045ba0`) and `lastTime`/`loopTime` report through
//!   `FUN_10045ea0`/`FUN_10045ec0` while `frameTickCount`/`frameLastTime`/
//!   `frameLoopTime` stay raw. The game's own chain is milliseconds end to end
//!   (`EventIntf.cpp:951,991-997` `TVPGetTickCount` → `addContinuousHandler` →
//!   `MainWindow` → `AffineLayer` → `_player.progress(_interval)`).
//! * **The variables** — `setVariable`/`getVariable`/`contains` and the
//!   `variableKeys` listing the game's `_getOptions` iterates are real: the
//!   reference's `variableKeys` (handler `FUN_10015690`) collects the player's
//!   variable-key strings into a TJS array, and this port answers the same
//!   array shape from the names its `setVariable` writes created.
//! * **Rendering** — `draw(layer)` samples the current tick, applies the
//!   player's `setCoord`/`setRotate`/`setScale`/`setDrawAffineTranslateMatrix`
//!   transform and its colour filter, and composites the draw list into the
//!   layer's bitmap through the engine's plugin-facing layer path
//!   ([`krkr_engine::plugin_api::layer`]). Icon resources are RL-decoded and
//!   palettes expanded by [`krkr_emote`].
//! * **The separate-layer canvas** — `new Motion.SeparateLayerAdaptor(owner)`
//!   answers a real drawable `Layer` (the engine's plugin canvas seam), sized
//!   like its owner and attached under it as a visible child with
//!   `hitThreshold = 0x100`, so the game's `drawAffine` sequence works end to
//!   end: `Motion.Player.clear(adaptor, neutralColor)` and
//!   `Motion.Player.draw(adaptor)` write its bitmap, the owner publishes it
//!   with `Layer.assignImages(adaptor)`, and the child still draws once
//!   `drawAffine` restores the owner's `ltBinder` type. The DLL's child model
//!   is the placement's evidence — the adaptor creates host `Layer`s parented
//!   to its `targetLayer`, visible as soon as they have pixels, with
//!   `hitThreshold = 0x100` (`FUN_1000d280`, disasm `0x1000d93d`/`0x1000d96c`)
//!   — and the game's `entryOwner` rewrites an `ltAlpha` owner to `ltBinder`
//!   right after constructing it, so only a visible child can reach the
//!   screen. `getSubImageLayers()` stays `void`, which is what makes the game
//!   take its single-canvas path (the reference has no such member at all).
//!
//! # What is not, and says so
//!
//! Physics (`initPhysics`, wind/pend controls), timelines
//! (`playTimeline`/`setTimelineBlendRatio`/`fadeOutTimeline` and the
//! `*Timeline*` listings beyond the loaded animation names), mesh deformation
//! (`LayerMeshSupport`, `meshDivisionRatio`, `processedMeshVerticesNum`),
//! particles, the D3D camera
//! members, and `EmotePlayer`'s `.psb` *model* playback (the `.mtn` motion path
//! is the one wired to the adapter). Every such member is still registered, so
//! a script never dies on `MemberNotFound`, and its first call logs a warning
//! naming the member — the surface is reachable, the behaviour is not faked.
//! `Motion.Player.useD3D` is deliberately *absent*: the game probes
//! `typeof Motion.Player.useD3D == "Object"` and falling into the catch sets
//! `Motion.enableD3D = 0`, exactly what the no-D3D reference build does
//! (`AffineSourceMotion.tjs`, decompiled at `/tmp/m38/AffineSourceMotion.decomp.tjs:3447-3457`).
//!
//! # The `opa` scale (settled)
//!
//! `motionplayer_nod3d.dll` `FUN_1001d000` reads a frame content's `opa` with
//! its integer getter and keeps one byte of it (`movzbl %al, %ecx; movl %ecx,
//! 0x48(%ebx)`), with that state byte initialised to `0xff` — so `opa` is a
//! 0..255 alpha where an absent field is fully opaque, `192` is ≈0.75 and `0`
//! is transparent. [`krkr_emote::normalize`] rescales the parsed tree to the
//! scale eluna divides by, and the same fix drops PARQUET's `parameterize:
//! null` so a non-parameterised layer follows its timeline instead of freezing
//! at local time 0 (without it, the `opa: 192` frame never activates at all).
//! `crates/krkr-emote/tests/parquet.rs` measures both on `sd101.mtn`.
//!
//! # Seeing a motion play
//!
//! ```text
//! cargo test -p krkr-plugins --lib motion_player -- --nocapture
//!     # synthetic end-to-end: load → play → progress → draw, asserted on
//!     # layer.getMainPixel; the state machine, variables and opa too
//! cargo test -p krkr-emote --test parquet -- --nocapture
//!     # the game's own sd101.mtn: icon decode, the opa fade at tick 90 and a
//!     # 1500x900 render (skips itself when /Users/ruri/Downloads/PARQUET is
//!     # absent; override the directory with KRKR_EMOTE_PARQUET_DIR)
//! ```

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use krkr_emote::{
    EMOTE_TICKS_PER_SECOND, Motion, MotionDrawItem, TextureCache, Tint, render_draw_list_into,
};
use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::{
        graphic::{
            GraphicFrame, GraphicLoader, GraphicSource, LiveGraphic, LoadedGraphic,
            register_graphic_loader, unregister_graphic_loader,
        },
        layer::{attach_canvas_layer, create_canvas_layer, fit_canvas_layer, layer_bitmap_write},
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Motion / Motion.Player / Motion.EmotePlayer (E-mote `.mtn` motion playback)",
    notes: "The `.mtn` motion path is real: ResourceManager.load parses the file through krkr-emote (PSB + PARQUET \
            flavor adaptation + RL/palette texture decode), the time model is milliseconds-facing like the reference \
            (progress converts ×60/1000, frameProgress is raw, tickCount/lastTime/loopTime report as ms while the \
            frame* family stays raw ticks, loops wrap to loopTime), play/progress/stop/skip/speed and \
            timed setVariable(name, value, time, easing) plus getVariable/contains/variableKeys drive the model's \
            own clock and variable state, and draw(layer) composites the sampled draw list into the layer bitmap \
            through plugin_api::layer with the player's coord/rotate/scale/affine transform and colour filter. \
            The module also claims `.mtn` in the engine's script image path (plugin_api::graphic, the reference's \
            TVPRegisterGraphicLoadingHandler), so Layer.loadImages of a motion yields a real bitmap sized by the \
            file's screenSize and the engine keeps it animating frame by frame - PARQUET's title_bg.mtn (the title \
            screen's decoration and PARQUET logo) loads exactly that way. \
            Motion.SeparateLayerAdaptor(owner) is a real drawable Layer (a plugin_api canvas) sized like its owner \
            and attached as its visible child, which is how the reference's adaptor reaches the screen under a \
            ltBinder owner; the game's clear/draw/Layer.assignImages publish path works on top of that. \
            Not implemented, and honest: physics (initPhysics, wind/pend), \
            timelines, mesh deformation, particles \
            and EmotePlayer's `.psb` model playback; each such member is \
            registered and logs a one-time warning on first call instead of returning a silent success. \
            The recovered ResourceManager member table (loadSource/load/unload/unloadAll/isExistMotion/ \
            findMotion/findSource/random/requireLayerId/releaseLayerId/clearCache/bufLayer) is registered on \
            Motion.ResourceManager, where the reference's own table lives - the reference Motion class carries \
            only its constants, doAlphaMaskOperation and the sub-class items - and every declared floor is the \
            reference's ArgsCount (ncbind rejects short calls, drops surplus arguments). \
            Motion.Player.useD3D is absent on purpose (the game's probe then sets Motion.enableD3D = 0, the nod3d \
            reference behaviour). The `opa` scale is settled against motionplayer_nod3d.dll's frame parser and \
            applied in krkr-emote's normalization (0..255 byte, absent = 0xff).",
    install: |engine| engine.register_plugin(MotionPlayerPlugin),
};

pub struct MotionPlayerPlugin;

impl KrkrPlugin for MotionPlayerPlugin {
    fn name(&self) -> &str {
        "motionplayer.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The pools are keyed by TJS object handles, which a *fresh* runtime
        // hands out from zero again: a second engine in the same process would
        // otherwise resolve an old handle to a stale manager, player or motion.
        // Registration happens once per engine (the host installs a module on
        // its first `Plugins.link`), so this is the per-engine reset.
        MANAGERS.with(|managers| managers.borrow_mut().clear());
        PLAYERS.with(|players| players.borrow_mut().clear());
        GLOBAL_POOL.with(|pool| pool.borrow_mut().loaded.clear());
        WARNED.with(|warned| warned.borrow_mut().clear());
        install_motionplayer_compat(runtime);
        Ok(())
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The `.mtn` loader is one registration shared with `emoteplayer.dll`
        // (the same driver in another build): unlinking this alias leaves it in
        // place while the other one is still linked, and drops it when this was
        // the last.
        unregister_motion_graphic_loader(runtime, self.name());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Plugin state
// ---------------------------------------------------------------------------

/// One loaded motion file, as the reference's `ResourceManager` holds it.
struct LoadedMotion {
    /// How many `load` calls are outstanding, so one wrapper's `unload` does
    /// not pull the file out from under another's player.
    refs: usize,
    motion: Arc<Motion>,
    /// The `.metadata` member the game's wrapper reads off the load handle
    /// (`AffineSourceMotion.tjs:366-367`), built once per load.
    metadata: Variant,
}

/// A resource manager object's loaded files, keyed by their storage names.
#[derive(Default)]
struct ManagerState {
    loaded: BTreeMap<String, LoadedMotion>,
}

/// One variable write that eases over time (`setVariable` with a duration).
struct VariableTween {
    name: String,
    from: f32,
    to: f32,
    start_tick: f64,
    duration_ticks: f64,
    easing: f64,
}

/// Everything a `Motion.Player` / `Motion.EmotePlayer` instance carries.
#[derive(Default)]
struct PlayerState {
    /// The resource manager this player was constructed with, kept for
    /// `play`'s motion-name lookup and for the `resourceManager` member.
    manager: Option<ObjectHandle>,
    chara: String,
    /// The animation the player was told to play (`motion` in scripts).
    animation: String,
    /// The loaded model that animation lives in. A player keeps its own strong
    /// reference, like the reference's player does.
    motion: Option<Arc<Motion>>,
    motion_storage: Option<String>,
    texture_cache: Option<TextureCache>,
    /// Current position, in 1/60 s ticks.
    tick: f64,
    speed: f64,
    playing: bool,
    completion_type: i64,
    /// Script override for `loopTime`; `None` uses the animation's own value.
    loop_time: Option<f64>,
    variables: BTreeMap<String, f32>,
    tweens: Vec<VariableTween>,
    coord: [f64; 2],
    /// The `coordinate` member: the draw layer's z hint (used by the
    /// separate-layer path, kept for scripts here).
    coordinate: f64,
    scale: f64,
    rotation: f64,
    affine: Option<[f64; 6]>,
    colour: u32,
    colour_weight: u32,
    /// `opacity` (0..255), multiplied into every sprite's alpha when drawing.
    opacity: f64,
    visible: bool,
    mask_mode: i64,
    /// `outline` is script data in the reference; kept verbatim here.
    outline: Option<Variant>,
    /// The plain string members (`project`, `motionKey`, `stealthChara`,
    /// `stealthMotion`), kept so a script that writes one reads it back.
    strings: BTreeMap<String, String>,
    /// The reference's plain instance data (`preview`, the camera/mesh values
    /// and the rest): stored verbatim so a script that writes one reads it
    /// back, with a one-time warning that no behaviour consumes it here.
    values: BTreeMap<String, Variant>,
}

impl PlayerState {
    fn new(manager: Option<ObjectHandle>) -> Self {
        Self {
            manager,
            speed: 1.0,
            scale: 1.0,
            colour: NEUTRAL_COLOUR,
            colour_weight: NEUTRAL_COLOUR,
            opacity: 255.0,
            visible: true,
            ..Self::default()
        }
    }
}

/// `setColor(0xFF808080)` is the game's "no filter" value
/// (`AffineSourceMotion.tjs:884`); see [`Tint::from_emote_colour`] for the
/// neutral-centred reading of the RGB bytes.
const NEUTRAL_COLOUR: u32 = 0xFF80_8080;

thread_local! {
    /// Loaded files per resource-manager object.
    static MANAGERS: RefCell<BTreeMap<ObjectHandle, ManagerState>> =
        const { RefCell::new(BTreeMap::new()) };
    /// Players by object handle.
    static PLAYERS: RefCell<BTreeMap<ObjectHandle, PlayerState>> =
        const { RefCell::new(BTreeMap::new()) };
    /// The class-level pool behind the class-object spellings
    /// (`Motion.ResourceManager.load`/`unload`/`findMotion`, …), which reach the
    /// same members as an instance call but have no manager of their own.
    static GLOBAL_POOL: RefCell<ManagerState> = const { RefCell::new(ManagerState {
        loaded: BTreeMap::new(),
    }) };
    /// Members whose "not implemented" warning already fired, so a per-frame
    /// call does not flood the log.
    static WARNED: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

/// Logs a member's warning once per `Class.member`.
fn warn_once(runtime: &mut Runtime<KrkrHost>, key: &str, message: &str) {
    let first = WARNED.with(|warned| warned.borrow_mut().insert(key.to_owned()));
    if first {
        runtime.host_mut().log(message);
    }
}

/// Logs a member's "not implemented" warning once per `Class.member`: the call
/// reaches nothing.
fn warn_unsupported(runtime: &mut Runtime<KrkrHost>, member: &str) {
    warn_once(
        runtime,
        member,
        &format!(
            "WARN motionplayer.dll: {member} is not implemented yet — the call is ignored \
             (see crates/krkr-plugins/src/motion_player.rs for what is wired)"
        ),
    );
}

/// Logs the softer warning of a member the port *stores* but nothing consumes.
fn warn_stored_but_unused(runtime: &mut Runtime<KrkrHost>, member: &str) {
    warn_once(
        runtime,
        member,
        &format!(
            "WARN motionplayer.dll: {member} is stored but nothing consumes it in this port \
             (the camera/mesh subsystems are not implemented)"
        ),
    );
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// The reference's `ResourceManager` class table (`motionplayer.dll:0x1005a530`,
/// `motionplayer_nod3d.dll:0x10050d40`) beyond the three members with real
/// behaviour here (`load`/`unload`/`clearCache`, registered separately).
///
/// The table's *placement* is the `ResourceManager` class, not `Motion`: every
/// member object it installs is a
/// `ncbNativeClassMethod<InvokeCommand<ResourceManager, …>>` (e.g.
/// `0x10099240` = `void (ResourceManager::*)(tTJSString)` for `unload`), and
/// the class builder that calls the table (`0x1008b690` → `0x1008f690`) is the
/// `ResourceManager` wrapper. The reference's `Motion` class registers only its
/// constants, `getD3DAvailable` (D3D build) and `doAlphaMaskOperation`
/// (`0x1007c040`, `0x100900d0`), plus the sub-class items (`Motion.Player`,
/// `Motion.EmotePlayer`, `Motion.ResourceManager`, `Motion.SeparateLayerAdaptor`,
/// `0x100897e0`/`0x10089860`/`0x100899e0`/`0x10089a60`).
///
/// The arities are the reference's `ArgsCount` in ncbind terms — the member
/// pointer's parameter count, checked as `numparams < ArgsCount` → so the
/// reference accepts `ArgsCount` **or more** (extra arguments are dropped), and
/// `AtLeast(n)` is the faithful port. A `0`-argument member therefore accepts
/// any call (surplus arguments discarded), which is what `Any` spells here.
///
/// Each entry carries its `file:func@addr` anchor from the M170 disassembly pass
/// (`motionplayer.dll`, image base `0x10000000`); the ctor named after the
/// *preceding* `FUN_10097d30(ResourceManager, name)` is the one registered under
/// that name — the table builds the member object first and hands it over in
/// `ESI`, so M166's "name → the object constructed after it" pairing is shifted
/// by one. The game's own call shapes (`data.xp3/system/motion.tjs`:
/// `resourceManager.load(prefix + a0)`, `unload(prefix + a0)`, `clearCache()`)
/// agree with this ordering, and `getVariable(name)` (1 arg) on
/// `Motion.EmotePlayer` is the decisive one: the shifted reading gave it 6.
const RESOURCE_MANAGER_METHODS: &[(&str, NativeArgCount)] = &[
    // `tTJSVariant (SourceCache::*)(tTJSVariant, tTJSVariant)` @0x10099060.
    ("loadSource", NativeArgCount::AtLeast(2)),
    // A property in the reference (get/set `tTJSVariant (void)` @0x1009f3e0);
    // kept callable here so a `rm.bufLayer()` probe does not die on
    // "not callable", with the warning telling the truth.
    ("bufLayer", NativeArgCount::AtLeast(0)),
    // `void (ResourceManager::*)(void)` @0x100992e0 — 0 args, so any call.
    ("unloadAll", NativeArgCount::AtLeast(0)),
    // `bool (ResourceManager::*)(tTJSVariant, tTJSVariant)` @0x10099380.
    ("isExistMotion", NativeArgCount::AtLeast(2)),
    // `tTJSVariant (ResourceManager::*)(tTJSVariant, tTJSVariant)` @0x10099420.
    ("findMotion", NativeArgCount::AtLeast(2)),
    // `tTJSVariant (ResourceManager::*)(tTJSString, tTJSString)` @0x100994c0.
    ("findSource", NativeArgCount::AtLeast(2)),
    // `double (ResourceManager::*)(void)` @0x10099560 — 0 args, so any call.
    ("random", NativeArgCount::AtLeast(0)),
    // `unsigned int (ResourceManager::*)(void)` @0x10099600 — 0 args. The M166
    // survey paired this name with the *next* object (`void (unsigned int)`),
    // which made our old `AtLeast(1)` look like a match; it would in fact
    // reject the 0-argument call the reference accepts.
    ("requireLayerId", NativeArgCount::AtLeast(0)),
    // `void (ResourceManager::*)(unsigned int)` @0x100996a0 — the arity M166
    // could not recover because it read the name as the table's last entry.
    ("releaseLayerId", NativeArgCount::AtLeast(1)),
];

/// The reference `Motion` class's own member besides the constants and the
/// sub-class items: a free function taking eleven arguments
/// (`?$InvokeCommand@VMotion@@P6AXVtTJSVariant@@HH0HHHHHII@Z`, the only
/// `Motion` member RTTI in either build; ctor `0x100900d0` nod3d /
/// `0x1009a960` D3D). Registered so the member exists, with the port's usual
/// first-call warning: the alpha-mask operation itself is not implemented.
const MOTION_METHODS: &[(&str, NativeArgCount)] =
    &[("doAlphaMaskOperation", NativeArgCount::AtLeast(11))];

/// Class constants recovered from the reference's string pool. Only
/// `PlayFlagForce` and `MaskModeAlpha` are used by the game; the values of the
/// other flag bits were not recovered (they are bit flags by construction), so
/// the powers of two after `1` are this port's assignment.
const MOTION_CONSTANTS: &[(&str, i64)] = &[
    ("PlayFlagForce", 1),
    ("PlayFlagJoin", 2),
    ("PlayFlagChain", 4),
    ("PlayFlagAsCan", 8),
    ("PlayFlagStealth", 16),
    ("MaskModeAlpha", 1),
    ("MaskModeStencil", 2),
    // The D3D-only build's `enableD3D`; the game rewrites it from its own
    // `Motion.Player.useD3D` probe, which lands on 0 here (no `useD3D`).
    ("enableD3D", 0),
];

/// The reference's 81-member `Player` registration, in dossier order
/// (`docs/plugins/motionplayer.md`). The names are the checklist: every one is
/// reachable on `Motion.Player`; `register_player_members` gives the ones with
/// real behaviour their implementation and warns for the rest.
const PLAYER_MEMBERS: &[&str] = &[
    "preview",
    "priorDraw",
    "chara",
    "stealthChara",
    "motion",
    "stealthMotion",
    "tags",
    "motionKey",
    "project",
    "completionType",
    "speed",
    "tickCount",
    "frameTickCount",
    "cameraActive",
    "outline",
    "meshline",
    "maskMode",
    "colorWeight",
    "syncActive",
    "independentLayerInherit",
    "defaultTransformOrder",
    "defaultSyncActive",
    "transformOrder",
    "coordinate",
    "zFactor",
    "stereovisionActive",
    "resourceManager",
    "cameraTarget",
    "cameraPosition",
    "cameraFOV",
    "cameraAlive",
    "bounds",
    "loopTime",
    "lastTime",
    "playing",
    "allplaying",
    "syncWaiting",
    "skipToSync",
    "frameLastTime",
    "frameLoopTime",
    "hasCamera",
    "stop",
    "setCameraOffset",
    "releaseSyncWait",
    "setVariable",
    "modifyRoot",
    "setCoord",
    "left",
    "flipX",
    "flipY",
    "angleDeg",
    "angleRad",
    "setZoom",
    "zoomX",
    "zoomY",
    "setSlant",
    "slantX",
    "slantY",
    "opacity",
    "visible",
    "processedMeshVerticesNum",
    "meshDivisionRatio",
    "getLayerNames",
    "progress",
    "frameProgress",
    "play",
    "clear",
    "draw",
    "drawNitro2D",
    "getLayerMotion",
    "getLayerGetter",
    "getLayerSetter",
    "getLayerGetterList",
    "getCommandList",
    "onAction",
    "onSync",
    "onFindMotion",
    "getVariableRange",
    "variableKeys",
];

/// Members with a plain fixed default: instance data the reference carries and
/// the port seeds with this value, then stores and returns whatever a script
/// writes. No behaviour depends on them here, so the first access warns once.
const PLAYER_PLAIN_DEFAULTS: &[(&str, i64)] = &[
    // `useD3D` is intentionally absent (see the module docs).
    ("preview", 0),
    ("priorDraw", 0),
    ("cameraActive", 0),
    ("meshline", 0),
    ("syncActive", 0),
    ("independentLayerInherit", 0),
    ("defaultSyncActive", 0),
    ("stereovisionActive", 0),
    ("cameraAlive", 0),
    ("hasCamera", 0),
    ("syncWaiting", 0),
    ("zFactor", 0),
];

/// Value-shaped members of the D3D camera / mesh path (`zoomX`, `slantY`, the
/// camera vectors). The reference keeps them as plain instance data, and so
/// does the port: a script write is stored and read back verbatim, while the
/// value reaches no behaviour here (the D3D render path and the mesh
/// subsystem are not implemented), so the first access warns once. The *verbs*
/// among the unimplemented members (`setZoom`, `startWind`, `playTimeline`, …)
/// are registered as methods instead, because a script calls them the way it
/// calls the reference's.
const PLAYER_CAMERA_MEMBERS: &[&str] = &[
    "cameraTarget",
    "cameraPosition",
    "cameraFOV",
    "zoomX",
    "zoomY",
    "slantX",
    "slantY",
    "meshSyncChildMask",
];

/// Installs the E-mote surface. Shared with `emoteplayer.dll`
/// ([`crate::emoteplayer`]), which registers the same classes.
pub(crate) fn install_motionplayer_compat(runtime: &mut Runtime<KrkrHost>) {
    let motion = match runtime.global_member("Motion") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "Motion");
            runtime.set_global_member("Motion", Variant::Object(handle));
            handle
        }
    };

    for (name, value) in MOTION_CONSTANTS {
        runtime.set_object_member(motion, *name, Variant::Integer(*value));
    }
    for (name, arg_count) in MOTION_METHODS {
        let member = *name;
        runtime.register_object_native_with_arg_count(
            motion,
            member,
            *arg_count,
            move |runtime: &mut Runtime<KrkrHost>,
                  _this_obj: Option<ObjectHandle>,
                  _args: Vec<Variant>| {
                warn_unsupported(runtime, &format!("Motion.{member}"));
                Ok(Variant::Void)
            },
        );
    }

    let resource_manager = resource_manager_constructor(runtime);
    let player = player_constructor(runtime, Class::Player);
    let emote_player = player_constructor(runtime, Class::EmotePlayer);
    let separate_adaptor = separate_layer_adaptor_constructor(runtime);
    runtime.set_object_member(motion, "ResourceManager", Variant::Object(resource_manager));
    runtime.set_object_member(motion, "Player", Variant::Object(player));
    runtime.set_object_member(motion, "EmotePlayer", Variant::Object(emote_player));
    runtime.set_object_member(
        motion,
        "SeparateLayerAdaptor",
        Variant::Object(separate_adaptor),
    );

    register_motion_graphic_loader(runtime);
}

// ---------------------------------------------------------------------------
// The `.mtn` graphic loader (the script image path)
// ---------------------------------------------------------------------------

/// The extensions the loader answers. Only `.mtn` is claimed: the `.psb`
/// *model* surface is one of this module's declared stubs (`EmotePlayer`), so
/// a `.psb` asked for as an image keeps the engine's own honest
/// "The image format could not be determined" instead of a half-load.
const MOTION_EXTENSIONS: &[&str] = &[".mtn"];

/// The module's graphic loader, kept in one place so the engine's double
/// `register` (boot, then the first `Plugins.link`) hands back the same `Arc`
/// and the registry treats the second call as a no-op.
fn motion_graphic_loader() -> &'static Arc<dyn GraphicLoader> {
    static LOADER: OnceLock<Arc<dyn GraphicLoader>> = OnceLock::new();
    LOADER.get_or_init(|| Arc::new(MotionGraphicLoader) as Arc<dyn GraphicLoader>)
}

fn register_motion_graphic_loader(runtime: &mut Runtime<KrkrHost>) {
    if let Err(error) = register_graphic_loader(runtime, Arc::clone(motion_graphic_loader())) {
        runtime.host_mut().log(&format!(
            "motionplayer.dll: the .mtn graphic loader could not be registered: {error}"
        ));
    }
}

/// The module aliases the shared E-mote implementation answers as
/// (`motionplayer.dll` and `emoteplayer.dll` are the same driver in two
/// builds; both register this loader).
const EMOTE_MODULE_ALIASES: [&str; 2] = ["motionplayer.dll", "emoteplayer.dll"];

/// Drops the shared `.mtn` loader once the **last** E-mote alias is unlinked —
/// the counterpart of the reference's `TVPUnregisterGraphicLoadingHandler`
/// (`GraphicsLoaderIntf.cpp:184`), which the module that registered the handler
/// calls.
///
/// One registration serves both aliases, so the ownership rule is "it lives
/// while either alias is linked": a game may link both (probing which build it
/// was shipped with) and unlink one, and that unlink must not take the claim
/// away from the module still linked. `unlinked` is the alias being torn down;
/// any *other* registered alias keeps the loader.
pub(crate) fn unregister_motion_graphic_loader(runtime: &mut Runtime<KrkrHost>, unlinked: &str) {
    let other_linked = runtime.host().linked_plugins().any(|name| {
        !name.eq_ignore_ascii_case(unlinked)
            && EMOTE_MODULE_ALIASES
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    });
    if other_linked {
        return;
    }
    unregister_graphic_loader(runtime, "motionplayer.dll");
}

/// The E-mote driver answering for `.mtn` in the script image path — the
/// reference's `TVPRegisterGraphicLoadingHandler` claim
/// (`visual/GraphicsLoaderIntf.cpp:170`, dispatch `:1506`/`:1509`, the
/// missing-extension suggestion walk `:1480-1503`), which is what
/// makes `Layer.loadImages("motion/title_bg.mtn")` and `System.touchImages`
/// reach the plugin instead of the built-in decoders. PARQUET's title layer
/// (`custom.ks` `*title_start`, the `GetTitleImageFile.UseMotion()` branch)
/// loads its motion exactly this way.
///
/// The file's own data decides how the frame is laid out:
///
/// * **Animation** — the animation named like the storage file's stem when the
///   file carries one (`logoflash.mtn` → `logoflash`), otherwise the file's
///   first animation. A `.mtn` used as a scene image is authored around one
///   entry motion (PARQUET's `title_bg.mtn` opens with `char_move`, the
///   curtains-and-logo sweep the title uses), and the game names the animation
///   itself only on the `Motion.ResourceManager` path, which has the storage
///   name to match on.
/// * **Canvas** — the file's root `screenSize` (`width`, `height`,
///   `originX`, `originY`): the model space is the authored screen, centred on
///   the origin (`title_bg.mtn` declares 1920x1080, and its mirrored sprite
///   pairs — `bgframe1` at (+774,-489) and its flipped twin at (-774,+489) —
///   confirm the origin is the screen centre). The frame is the screen, and the
///   content is translated by `width/2 - originX`, `height/2 - originY`.
///   A file without `screenSize` falls back to the bounds of the first
///   non-empty frame, translated by `-min`, which keeps the frame the content
///   itself asks for.
/// * **Playback** — the load's bitmap is tick 0 and the graphic stays live; the
///   engine's frame clock drives it from there (see [`MotionGraphic`]).
struct MotionGraphicLoader;

impl GraphicLoader for MotionGraphicLoader {
    fn name(&self) -> &str {
        "motionplayer.dll"
    }

    fn extensions(&self) -> &[&str] {
        MOTION_EXTENSIONS
    }

    fn load(&self, source: GraphicSource<'_>) -> std::result::Result<LoadedGraphic, String> {
        let motion = Motion::from_bytes(source.bytes).map_err(|error| error.to_string())?;
        let animation = graphic_animation(&motion, source.storage).ok_or_else(|| {
            format!(
                "`{}` carries no animation (the file has no motion to play)",
                source.storage
            )
        })?;
        let canvas = MotionCanvas::of(&motion, &animation)?;

        let motion = Arc::new(motion);
        let mut textures = TextureCache::new(Arc::clone(&motion));
        let frame = canvas.render(&motion, &animation, 0.0, &mut textures);
        let playback = MotionPlayback::new(&motion, &animation);
        Ok(LoadedGraphic {
            frame,
            live: Some(Arc::new(MotionGraphic {
                motion,
                animation,
                canvas,
                textures: Mutex::new(textures),
                playback: Mutex::new(playback),
            })),
        })
    }
}

/// One live `.mtn` image: the parsed motion, the animation it plays, the
/// canvas it draws into and the frame clock the engine advances.
///
/// The engine asks for a frame with the time since the load
/// ([`LiveGraphic::frame`]) and swaps every layer showing the image, which is
/// this port's way of doing what the reference's handler does when it keeps
/// drawing into the layer bitmap it was handed. A motion that plays once holds
/// its last drawn frame — the animation's own tail (PARQUET's `title_bg.mtn`
/// clears its priorities at `lastTime`) must not erase the layer the title
/// screen is showing.
struct MotionGraphic {
    motion: Arc<Motion>,
    animation: String,
    canvas: MotionCanvas,
    textures: Mutex<TextureCache>,
    playback: Mutex<MotionPlayback>,
}

/// The frame clock of one live graphic: where the animation is, whether it
/// loops, and whether it has ended with its last frame standing.
struct MotionPlayback {
    duration_ticks: f64,
    /// `Some(point)` loops back to tick 0 at `point`; `None` plays once.
    loop_ticks: Option<f64>,
    /// The last tick a frame was rendered at, so a display refresh faster than
    /// the animation's own 1/60 s resolution does not re-rasterise the scene.
    rendered_tick: Option<f64>,
    /// The animation ended (or ended without a frame) and the frame already
    /// handed over stands.
    held: bool,
}

impl MotionPlayback {
    fn new(motion: &Motion, animation: &str) -> Self {
        let (duration_ticks, loop_time) = motion
            .animation(animation)
            .map(|animation| (f64::from(animation.duration_ticks), animation.loop_time))
            .unwrap_or((0.0, None));
        let loop_ticks = loop_time
            .map(f64::from)
            .filter(|point| point.is_finite() && *point > 0.0);
        Self {
            duration_ticks,
            loop_ticks,
            rendered_tick: None,
            held: false,
        }
    }
}

impl LiveGraphic for MotionGraphic {
    fn frame(&self, elapsed: Duration) -> Option<GraphicFrame> {
        let mut playback = self
            .playback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if playback.held {
            return None;
        }
        let elapsed_ticks = elapsed.as_secs_f64() * f64::from(EMOTE_TICKS_PER_SECOND);
        let tick = match playback.loop_ticks {
            Some(loop_ticks) => elapsed_ticks % loop_ticks,
            None => {
                if elapsed_ticks > playback.duration_ticks {
                    playback.held = true;
                    return None;
                }
                elapsed_ticks
            }
        };
        if let Some(rendered) = playback.rendered_tick
            && (tick - rendered).abs() < 0.5
        {
            return None;
        }
        let mut textures = self
            .textures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let frame = self
            .canvas
            .render(&self.motion, &self.animation, tick as f32, &mut textures);
        playback.rendered_tick = Some(tick);
        // An animation that plays **once** may end on a tick it draws nothing
        // at (PARQUET's `title` and `logoflash` do): the last frame the layer
        // already shows stands rather than the layer being wiped. A motion with
        // a loop point wraps instead (`:757`), so an all-transparent sample
        // there is part of the animation and is delivered like any other —
        // holding it would stop the loop dead on one empty tick.
        if playback.loop_ticks.is_none() && frame.rgba.iter().all(|byte| *byte == 0) {
            playback.held = true;
            return None;
        }
        Some(frame)
    }
}

/// The animation a `.mtn` opens with: the one named like the storage file's
/// stem when it exists, else the file's first animation.
fn graphic_animation(motion: &Motion, storage: &str) -> Option<String> {
    let stem = storage
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(storage)
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(storage);
    if let Some(animation) = motion.animation(stem) {
        return Some(animation.name.clone());
    }
    motion
        .animations()
        .first()
        .map(|animation| animation.name.clone())
}

/// The bitmap a motion draws into: the file's authored screen, centred on the
/// model origin.
#[derive(Clone, Copy, Debug)]
struct MotionCanvas {
    width: u32,
    height: u32,
    offset_x: f32,
    offset_y: f32,
}

impl MotionCanvas {
    fn of(motion: &Motion, animation: &str) -> std::result::Result<Self, String> {
        let root = &motion.psb().root;
        if let Some(screen_size) = root.field("screenSize") {
            let width = screen_size.field_u32("width").unwrap_or(0);
            let height = screen_size.field_u32("height").unwrap_or(0);
            if width > 0 && height > 0 {
                let origin_x = screen_size.field_f32("originX").unwrap_or(0.0);
                let origin_y = screen_size.field_f32("originY").unwrap_or(0.0);
                return Ok(Self {
                    width,
                    height,
                    // The model origin is the screen centre: the authored
                    // `origin` is the model point the screen's own origin
                    // sits on, so the translation is half the screen minus it.
                    offset_x: width as f32 / 2.0 - origin_x,
                    offset_y: height as f32 / 2.0 - origin_y,
                });
            }
        }

        // No `screenSize`: frame what the animation actually draws at its
        // first non-empty tick.
        for tick in [0.0f32, 1.0, 10.0, 30.0] {
            let Ok(items) = motion.draw_list(animation, tick) else {
                continue;
            };
            let mut bounds: Option<(f32, f32, f32, f32)> = None;
            for item in items.iter().filter(|item| item.visible) {
                let width = (item.size[0] * item.scale[0]).abs() * 0.5;
                let height = (item.size[1] * item.scale[1]).abs() * 0.5;
                let x = item.center[0] + item.world_transform[4];
                let y = item.center[1] + item.world_transform[5];
                let entry = bounds.get_or_insert((x - width, y - height, x + width, y + height));
                entry.0 = entry.0.min(x - width);
                entry.1 = entry.1.min(y - height);
                entry.2 = entry.2.max(x + width);
                entry.3 = entry.3.max(y + height);
            }
            if let Some((min_x, min_y, max_x, max_y)) = bounds
                && max_x > min_x
                && max_y > min_y
            {
                return Ok(Self {
                    width: (max_x - min_x).ceil().max(1.0) as u32,
                    height: (max_y - min_y).ceil().max(1.0) as u32,
                    offset_x: -min_x,
                    offset_y: -min_y,
                });
            }
        }
        Err(format!(
            "`{animation}` draws nothing this loader can size a frame from"
        ))
    }

    fn render(
        &self,
        motion: &Motion,
        animation: &str,
        ticks: f32,
        textures: &mut TextureCache,
    ) -> GraphicFrame {
        let mut frame = GraphicFrame::transparent(self.width, self.height);
        let Ok(mut items) = motion.draw_list(animation, ticks) else {
            return frame;
        };
        for item in items.iter_mut() {
            item.world_transform[4] += self.offset_x;
            item.world_transform[5] += self.offset_y;
        }
        render_draw_list_into(
            &mut frame.rgba,
            self.width,
            self.height,
            &items,
            textures,
            Tint::default(),
        );
        frame
    }
}

/// Which of the two player classes an instance belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Class {
    Player,
    EmotePlayer,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Player => "Player",
            Class::EmotePlayer => "EmotePlayer",
        }
    }

    /// The `animating` member exists on `EmotePlayer` (the game reads it to
    /// tell "animation finished" from "model idle"); `Player` reports through
    /// `playing`.
    fn has_animating(self) -> bool {
        self == Class::EmotePlayer
    }
}

/// The `ResourceManager` table's members that have no behaviour here (all of
/// [`RESOURCE_MANAGER_METHODS`] bar the implemented `load`/`unload`/
/// `clearCache`). The pool they report against is the instance's own when the
/// call arrives on a live manager, and the class-level pool when the script
/// calls `Motion.ResourceManager.unloadAll()` on the class object.
fn resource_manager_member_method(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    member: &str,
    args: &[Variant],
) -> Result<Variant> {
    let manager = resource_manager_this(runtime, this_obj);
    match member {
        "unloadAll" => {
            match manager {
                Some(manager) => MANAGERS.with(|managers| {
                    if let Some(state) = managers.borrow_mut().get_mut(&manager) {
                        state.loaded.clear();
                    }
                }),
                None => GLOBAL_POOL.with(|pool| pool.borrow_mut().loaded.clear()),
            }
            Ok(Variant::Void)
        }
        "isExistMotion" | "findMotion" => {
            let name = args
                .first()
                .map(Variant::to_tjs_string)
                .transpose()?
                .unwrap_or_default();
            let exists = any_loaded_motion(manager, |motion| motion.animation(&name).is_some());
            Ok(Variant::Integer(i64::from(exists)))
        }
        "findSource" => {
            let name = args
                .first()
                .map(Variant::to_tjs_string)
                .transpose()?
                .unwrap_or_default();
            let exists = any_loaded_motion(manager, |motion| motion.source(&name).is_some());
            Ok(Variant::Integer(i64::from(exists)))
        }
        "loadSource" | "bufLayer" | "random" | "requireLayerId" | "releaseLayerId" => {
            warn_unsupported(runtime, &format!("ResourceManager.{member}"));
            Ok(Variant::Integer(0))
        }
        other => {
            warn_unsupported(runtime, &format!("ResourceManager.{other}"));
            Ok(Variant::Void)
        }
    }
}

/// The manager object a `ResourceManager` method call acts on: its own when
/// `this` is a live manager instance, otherwise none (the class object and any
/// other receiver fall back to the class-level pool).
fn resource_manager_this(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    let this = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))?;
    MANAGERS
        .with(|managers| managers.borrow().contains_key(&this))
        .then_some(this)
}

/// Whether any file loaded behind `manager` (or in the class-level pool when
/// the call arrived on the class object) satisfies `predicate`.
fn any_loaded_motion(manager: Option<ObjectHandle>, predicate: impl Fn(&Motion) -> bool) -> bool {
    match manager {
        Some(manager) => MANAGERS.with(|managers| {
            managers.borrow().get(&manager).is_some_and(|state| {
                state
                    .loaded
                    .values()
                    .any(|loaded| predicate(&loaded.motion))
            })
        }),
        None => GLOBAL_POOL.with(|pool| {
            pool.borrow()
                .loaded
                .values()
                .any(|loaded| predicate(&loaded.motion))
        }),
    }
}

/// [`krkr_emote::Motion::from_bytes`] read from project storage into a pool.
///
/// A storage miss and a parse failure are *not* silent: the reference returns
/// a null handle and the game's wrapper raises its own exception
/// (`motion.tjs`, decompiled: `load(a0)` passes `resourceManager.load(...)`
/// straight through, and `loadResource` wraps it in
/// `モーション用リソースの読み込みに失敗しました`), so an unreadable file
/// logs the reason and yields `Null`.
fn load_into(
    runtime: &mut Runtime<KrkrHost>,
    pool: &'static std::thread::LocalKey<RefCell<ManagerState>>,
    path: &str,
) -> Variant {
    let already = pool.with(|pool| {
        let mut pool = pool.borrow_mut();
        match pool.loaded.get_mut(path) {
            Some(loaded) => {
                loaded.refs += 1;
                Some(loaded.metadata.clone())
            }
            None => None,
        }
    });
    if let Some(metadata) = already {
        return load_handle(runtime, path, metadata);
    }

    let bytes = match runtime.host().read_binary_storage(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            runtime
                .host_mut()
                .log(&format!("motionplayer.dll: cannot load {path:?}: {error}"));
            return Variant::Null;
        }
    };
    let motion = match Motion::from_bytes(&bytes) {
        Ok(motion) => Arc::new(motion),
        Err(error) => {
            runtime
                .host_mut()
                .log(&format!("motionplayer.dll: cannot parse {path:?}: {error}"));
            return Variant::Null;
        }
    };
    let metadata = motion_metadata(runtime, &motion);
    pool.with(|pool| {
        pool.borrow_mut().loaded.insert(
            path.to_owned(),
            LoadedMotion {
                refs: 1,
                motion,
                metadata: metadata.clone(),
            },
        );
    });
    load_handle(runtime, path, metadata)
}

fn unload_from(pool: &'static std::thread::LocalKey<RefCell<ManagerState>>, path: &str) {
    pool.with(|pool| {
        let mut pool = pool.borrow_mut();
        if let Some(loaded) = pool.loaded.get_mut(path) {
            loaded.refs = loaded.refs.saturating_sub(1);
            if loaded.refs == 0 {
                pool.loaded.remove(path);
            }
        }
    });
}

/// The handle `load` returns: an object whose `.metadata` the game's wrapper
/// reads, plus the storage name for diagnostics.
fn load_handle(runtime: &mut Runtime<KrkrHost>, path: &str, metadata: Variant) -> Variant {
    let handle = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(handle, "MotionResource");
    runtime.set_object_member(handle, "name", Variant::String(path.to_owned()));
    runtime.set_object_member(handle, "metadata", metadata);
    Variant::Object(handle)
}

/// The `.metadata` member: the file's root `metadata` value when it has one,
/// otherwise `Null` (which the game's wrapper treats as "no metadata").
fn motion_metadata(runtime: &mut Runtime<KrkrHost>, motion: &Motion) -> Variant {
    let Some(metadata) = motion.psb().root.field("metadata") else {
        return Variant::Null;
    };
    match psb_value_to_variant(runtime, metadata) {
        Some(value) => value,
        None => Variant::Null,
    }
}

/// Converts one PSB value into TJS data for the `.metadata` handle (objects
/// become dictionaries, lists arrays, strings stay strings; resource and
/// null values stay as they are).
fn psb_value_to_variant(
    runtime: &mut Runtime<KrkrHost>,
    value: &krkr_emote::PsbValue,
) -> Option<Variant> {
    use krkr_emote::PsbValue;
    match value {
        PsbValue::Null => Some(Variant::Null),
        PsbValue::Int(value) => Some(Variant::Integer(*value)),
        PsbValue::Float(value) => Some(Variant::Real(f64::from(*value))),
        PsbValue::Double(value) => Some(Variant::Real(*value)),
        PsbValue::String(text) => Some(Variant::String(text.clone())),
        PsbValue::Bool(value) => Some(Variant::Integer(i64::from(*value))),
        PsbValue::List(values) => {
            let mut converted = Vec::with_capacity(values.len());
            for value in values {
                converted.push(psb_value_to_variant(runtime, value)?);
            }
            Some(Variant::Object(runtime.alloc_array_object(converted)))
        }
        PsbValue::Object(fields) => {
            let handle = runtime.alloc_dictionary_object();
            for (name, value) in fields {
                let Some(value) = psb_value_to_variant(runtime, value) else {
                    continue;
                };
                runtime.set_object_member(handle, name.clone(), value);
            }
            Some(Variant::Object(handle))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Motion.ResourceManager
// ---------------------------------------------------------------------------

fn resource_manager_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor_with_arg_count(
        NativeArgCount::Any,
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, "ResourceManager");
            MANAGERS.with(|managers| {
                managers.borrow_mut().entry(instance).or_default();
            });
            install_resource_manager_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "ResourceManager");
    install_resource_manager_members(runtime, handle);
    handle
}

/// The `ResourceManager` class's member table. The three members with real
/// behaviour are `MotionResourceManager`'s five call shapes from the game's own
/// wrapper (`motion.tjs`, decompiled: `load(prefix + a0)`, `unload(prefix + a0)`,
/// `clearCache()`); the reference's `loadResource`/`unloadResource` spellings
/// stay for scripts written against the older census, and
/// [`RESOURCE_MANAGER_METHODS`] adds the recovered names that have no behaviour
/// here.
// `result_large_err` is the crate-wide `TjsError` size lint every native
// handler closure carries (`http_request.rs`/`sqlite3.rs` allow it too).
#[allow(clippy::result_large_err)]
fn install_resource_manager_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native_with_arg_count(
        handle,
        "load",
        NativeArgCount::AtLeast(1),
        resource_manager_load,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "unload",
        NativeArgCount::AtLeast(1),
        resource_manager_unload,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "clearCache",
        NativeArgCount::AtLeast(0),
        resource_manager_clear_cache,
    );
    for (name, arg_count) in RESOURCE_MANAGER_METHODS {
        let member = *name;
        runtime.register_object_native_with_arg_count(
            handle,
            member,
            *arg_count,
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  args: Vec<Variant>| {
                resource_manager_member_method(runtime, this_obj, member, &args)
            },
        );
    }
    runtime.register_object_native(handle, "loadResource", resource_manager_load);
    runtime.register_object_native(handle, "unloadResource", resource_manager_unload);
    runtime.register_object_native(handle, "addRef", return_this);
    runtime.register_object_native(handle, "release", native_void);
    runtime.register_object_native(handle, "finalize", resource_manager_finalize);
}

/// `ResourceManager.load(name)`: reads and caches one motion file.
fn resource_manager_load(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let path = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let manager = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| MANAGERS.with(|managers| managers.borrow().contains_key(handle)));
    let Some(manager) = manager else {
        // A class-level call (`Motion.ResourceManager.load`): the global pool.
        return Ok(load_into(runtime, &GLOBAL_POOL, &path));
    };
    let variant = load_into_manager(runtime, manager, &path);
    Ok(variant)
}

fn load_into_manager(
    runtime: &mut Runtime<KrkrHost>,
    manager: ObjectHandle,
    path: &str,
) -> Variant {
    let already = MANAGERS.with(|managers| {
        let mut managers = managers.borrow_mut();
        let state = managers.get_mut(&manager)?;
        match state.loaded.get_mut(path) {
            Some(loaded) => {
                loaded.refs += 1;
                Some(loaded.metadata.clone())
            }
            None => None,
        }
    });
    if let Some(metadata) = already {
        return load_handle(runtime, path, metadata);
    }

    let bytes = match runtime.host().read_binary_storage(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            runtime
                .host_mut()
                .log(&format!("motionplayer.dll: cannot load {path:?}: {error}"));
            return Variant::Null;
        }
    };
    let motion = match Motion::from_bytes(&bytes) {
        Ok(motion) => Arc::new(motion),
        Err(error) => {
            runtime
                .host_mut()
                .log(&format!("motionplayer.dll: cannot parse {path:?}: {error}"));
            return Variant::Null;
        }
    };
    let metadata = motion_metadata(runtime, &motion);
    MANAGERS.with(|managers| {
        if let Some(state) = managers.borrow_mut().get_mut(&manager) {
            state.loaded.insert(
                path.to_owned(),
                LoadedMotion {
                    refs: 1,
                    motion,
                    metadata: metadata.clone(),
                },
            );
        }
    });
    load_handle(runtime, path, metadata)
}

fn resource_manager_unload(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let path = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let manager = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle));
    match manager {
        Some(manager) if MANAGERS.with(|m| m.borrow().contains_key(&manager)) => {
            MANAGERS.with(|managers| {
                let mut managers = managers.borrow_mut();
                if let Some(state) = managers.get_mut(&manager)
                    && let Some(loaded) = state.loaded.get_mut(&path)
                {
                    loaded.refs = loaded.refs.saturating_sub(1);
                    if loaded.refs == 0 {
                        state.loaded.remove(&path);
                    }
                }
            });
        }
        _ => unload_from(&GLOBAL_POOL, &path),
    }
    Ok(Variant::Void)
}

fn resource_manager_clear_cache(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let manager = this_obj.filter(|handle| MANAGERS.with(|m| m.borrow().contains_key(handle)));
    match manager {
        Some(manager) => {
            MANAGERS.with(|managers| {
                if let Some(state) = managers.borrow_mut().get_mut(&manager) {
                    state.loaded.clear();
                }
            });
        }
        None => GLOBAL_POOL.with(|pool| pool.borrow_mut().loaded.clear()),
    }
    Ok(Variant::Void)
}

fn resource_manager_finalize(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(manager) = this_obj {
        MANAGERS.with(|managers| {
            managers.borrow_mut().remove(&manager);
        });
    }
    Ok(Variant::Void)
}

// ---------------------------------------------------------------------------
// Motion.Player / Motion.EmotePlayer
// ---------------------------------------------------------------------------

fn player_constructor(runtime: &mut Runtime<KrkrHost>, class: Class) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor_with_arg_count(
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj, class.name());
            let manager = args.first().and_then(Variant::object_handle);
            PLAYERS.with(|players| {
                players
                    .borrow_mut()
                    .insert(instance, PlayerState::new(manager));
            });
            install_player_members(runtime, instance, class);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, class.name());
    install_player_members(runtime, handle, class);
    handle
}

/// Where an adaptor records the layer it stands in for (the constructor's
/// argument), so `clear`/`draw` can keep its canvas that layer's size.
const ADAPTOR_OWNER_MEMBER: &str = "__motionSeparateAdaptorOwner";

/// `new Motion.SeparateLayerAdaptor(owner)` — a drawable canvas layer.
///
/// The reference class (registered by `motionplayer.dll`/`nod3d`, constructed
/// by the game as `new Motion.SeparateLayerAdaptor(owner incontextof
/// global.Layer)` in `system/AffineSourceMotion.tjs` `entryOwner`) is the
/// canvas the KAG motion layer draws each frame into: `drawAffine` hands it to
/// `Motion.Player.clear`/`draw`, reads the drawn pixels back and publishes
/// them onto the owner with `Layer.assignImages` (the game's `a0 instanceof
/// "Layer"` publish branch).  It is therefore a real `Layer` — see
/// [`create_canvas_layer`] — sized like the owner and attached under it as a
/// *visible child* ([`attach_canvas_layer`]), which is the only placement that
/// keeps drawing once the game restores the owner's `ltBinder` type at the end
/// of `drawAffine` (see the module docs).
///
/// The constructor argument arrives as the `incontextof` closure the VM built
/// (its closure object *is* the owner; the this-object slot only carries the
/// class context), and the owner is recorded on the adaptor so
/// [`refit_adaptor_canvas`] can follow later resizes of it.
fn separate_layer_adaptor_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor_with_arg_count(
        NativeArgCount::Any,
        |runtime: &mut Runtime<KrkrHost>, _this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = create_canvas_layer(runtime)?;
            runtime.add_object_class_info(instance, "SeparateLayerAdaptor");
            if let Some(owner) = adaptor_owner(args.first()) {
                runtime.set_object_member(instance, ADAPTOR_OWNER_MEMBER, Variant::Object(owner));
                fit_canvas_layer(runtime, instance, owner)?;
                attach_canvas_layer(runtime, instance, owner)?;
            }
            runtime.register_object_native_with_arg_count(
                instance,
                "getSubImageLayers",
                NativeArgCount::Any,
                get_sub_image_layers,
            );
            runtime.register_object_native(instance, "addRef", return_this);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "SeparateLayerAdaptor");
    runtime.register_object_native_with_arg_count(
        handle,
        "getSubImageLayers",
        NativeArgCount::Any,
        get_sub_image_layers,
    );
    runtime.register_object_native(handle, "addRef", return_this);
    handle
}

/// The adaptor constructor's parent-layer argument.
///
/// The game spells it `owner incontextof global.Layer`, which the VM
/// materializes as a closure whose closure object is the owner itself
/// (`change_this`, `vm/dispatch.rs`), so the owner is the closure's object and
/// not something the constructor could unwrap by calling it.
fn adaptor_owner(arg: Option<&Variant>) -> Option<ObjectHandle> {
    match arg? {
        Variant::Closure(closure) => Some(closure.object),
        Variant::Object(handle) => Some(*handle),
        _ => None,
    }
}

/// Keeps an adaptor canvas the size of the layer it was created for.
///
/// The game constructs the adaptor while its owner layer exists but need not
/// have its final rect yet (`system/AffineLayer.tjs` runs `entryOwner` before
/// its `onResize`), so every write into the adaptor re-fits it;
/// [`fit_canvas_layer`] does nothing once the sizes match.  A target that is
/// not an adaptor (no owner member) is left to the engine's own sizing.
fn refit_adaptor_canvas(runtime: &mut Runtime<KrkrHost>, target: ObjectHandle) -> Result<()> {
    let Some(owner) = runtime
        .object_member(target, ADAPTOR_OWNER_MEMBER)
        .object_handle()
    else {
        return Ok(());
    };
    fit_canvas_layer(runtime, target, owner)
}

/// `SeparateLayerAdaptor.getSubImageLayers()` answers `void` — "no sub-layer
/// model" — which is what makes the game take its single-canvas path
/// (`AffineSourceMotion.tjs:3238-3250` branches on `l4 === void`). Returning
/// an empty array is *not* equivalent: it selects the per-part path and draws
/// nothing.
fn get_sub_image_layers(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_unsupported(runtime, "SeparateLayerAdaptor.getSubImageLayers");
    Ok(Variant::Void)
}

/// Registers the whole `Player` member surface. Members with real behaviour
/// come first; the rest are declared from [`PLAYER_MEMBERS`] so the surface
/// matches the dossier's census exactly, warning on first use.
fn install_player_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle, class: Class) {
    let class_name = class.name();

    // -- Real state, readable and writable -----------------------------------
    register_player_string_property(runtime, handle, "chara");
    register_player_string_property(runtime, handle, "motion");
    register_player_string_property(runtime, handle, "project");
    register_player_string_property(runtime, handle, "motionKey");
    register_player_string_property(runtime, handle, "stealthChara");
    register_player_string_property(runtime, handle, "stealthMotion");
    register_player_number_property(runtime, handle, "speed");
    register_player_number_property(runtime, handle, "tickCount");
    register_player_number_property(runtime, handle, "frameTickCount");
    register_player_number_property(runtime, handle, "completionType");
    register_player_number_property(runtime, handle, "loopTime");
    register_player_number_property(runtime, handle, "lastTime");
    register_player_number_property(runtime, handle, "frameLastTime");
    register_player_number_property(runtime, handle, "frameLoopTime");
    register_player_number_property(runtime, handle, "opacity");
    register_player_number_property(runtime, handle, "colorWeight");
    register_player_number_property(runtime, handle, "maskMode");
    register_player_number_property(runtime, handle, "coordinate");
    register_player_boolean_property(runtime, handle, "visible");
    register_player_readonly_property(runtime, handle, "playing", |state, _runtime| {
        Variant::Integer(i64::from(state.playing))
    });
    register_player_readonly_property(runtime, handle, "allplaying", |state, _runtime| {
        Variant::Integer(i64::from(state.playing))
    });
    register_player_readonly_property(runtime, handle, "resourceManager", |state, _runtime| {
        state.manager.map(Variant::Object).unwrap_or(Variant::Void)
    });
    register_player_variable_keys(runtime, handle);
    register_player_outline_property(runtime, handle);
    register_player_script_member(runtime, handle, "tags");
    register_player_script_member(runtime, handle, "onAction");
    register_player_script_member(runtime, handle, "onSync");
    register_player_script_member(runtime, handle, "onFindMotion");
    if class.has_animating() {
        register_player_readonly_property(runtime, handle, "animating", |state, _runtime| {
            Variant::Integer(i64::from(state.playing))
        });
    }

    // -- Methods -------------------------------------------------------------
    // `play(name, flags)`: `void (tTJSString, unsigned int)` on `EmotePlayer`
    // (ctor `0x100999c0`) and a raw callback that rejects `numparams < 2` on
    // `Player` (`0x1004d140`); the game calls it as
    // `_player.play(motion, Motion.PlayFlagForce)`.
    runtime.register_object_native_with_arg_count(
        handle,
        "play",
        NativeArgCount::AtLeast(2),
        player_play,
    );
    runtime.register_object_native_with_arg_count(handle, "stop", NativeArgCount::Any, player_stop);
    runtime.register_object_native_with_arg_count(
        handle,
        "progress",
        NativeArgCount::AtLeast(1),
        player_progress,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "frameProgress",
        NativeArgCount::AtLeast(1),
        player_frame_progress,
    );
    // `setVariable` accepts 2..4 (`0x1005c400`), `getVariable` is
    // `double (EmotePlayer::*)(tTJSString) const` = one argument
    // (`0x10099a60`) — the game's `_player.getVariable(name)` shape, which the
    // M166 survey's shifted pairing misread as a six-double method.
    runtime.register_object_native_with_arg_count(
        handle,
        "setVariable",
        NativeArgCount::AtLeast(2),
        player_set_variable,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getVariable",
        NativeArgCount::AtLeast(1),
        player_get_variable,
    );
    // `contains` is `bool (EmotePlayer::*)(tTJSString, double, double)` (ctor
    // `0x1009a280`) on the emote class and `bool (Player::*)(double, double)`
    // (ctor `0x10098ac0`) on the plain one — the game calls the emote form as
    // `_player.contains("hit_" + label, x, y)`.
    runtime.register_object_native_with_arg_count(
        handle,
        "contains",
        match class {
            Class::EmotePlayer => NativeArgCount::AtLeast(3),
            Class::Player => NativeArgCount::AtLeast(2),
        },
        player_contains,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "draw",
        NativeArgCount::AtLeast(1),
        player_draw,
    );
    // `clear(layer, colour)` is `void (Player::*)(tTJSVariant, tTJSVariant)`
    // (`0x100987a0`); the reference never registers it on `EmotePlayer` — the
    // game guards its call with `typeof _player.clear != "undefined"` — but the
    // port keeps it on both classes so the emote path's canvas clearing runs.
    runtime.register_object_native_with_arg_count(
        handle,
        "clear",
        NativeArgCount::AtLeast(2),
        player_clear,
    );
    // The EmotePlayer `set*` family, with the reference's accepted argument
    // *ranges* (the raw-callback bodies compare `numparams - k` against the
    // span and reject both ends, so these are ranges, not the `AtLeast(n)`
    // floors ncbind's `NumT` members give): `setColor` 1..3 (`0x1005bc90`),
    // `setCoord` 2..4 (`0x1005b9d0`), `setRotate` 1..3 (`0x1005be40`),
    // `setScale` 1..3 (`0x1005bb60`), `setDrawAffineTranslateMatrix` six
    // doubles exactly (`0x10099b00`). `AtLeast(min)` is the faithful floor: the
    // reference drops arguments beyond its range's end, so a longer call
    // succeeds in both.
    runtime.register_object_native_with_arg_count(
        handle,
        "setColor",
        NativeArgCount::AtLeast(1),
        player_set_colour,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setCoord",
        NativeArgCount::AtLeast(2),
        player_set_coord,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setRotate",
        NativeArgCount::AtLeast(1),
        player_set_rotate,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setScale",
        NativeArgCount::AtLeast(1),
        player_set_scale,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setDrawAffineTranslateMatrix",
        NativeArgCount::AtLeast(6),
        player_set_affine,
    );
    runtime.register_object_native_with_arg_count(handle, "skip", NativeArgCount::Any, player_skip);
    runtime.register_object_native_with_arg_count(
        handle,
        "skipToSync",
        NativeArgCount::Any,
        player_skip,
    );
    runtime.register_object_native_with_arg_count(handle, "pass", NativeArgCount::Any, player_pass);
    runtime.register_object_native_with_arg_count(
        handle,
        "serialize",
        NativeArgCount::Any,
        player_serialize,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "unserialize",
        NativeArgCount::AtLeast(1),
        player_unserialize,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getMainTimelineLabelList",
        NativeArgCount::Any,
        player_label_list,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getDiffTimelineLabelList",
        NativeArgCount::Any,
        player_empty_list,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getPlayingTimelineInfoList",
        NativeArgCount::Any,
        player_playing_timeline_info,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getLoopTimeline",
        NativeArgCount::Any,
        player_zero,
    );

    // -- Declared-but-unimplemented surface ----------------------------------
    let registered: BTreeSet<&str> = [
        // Everything installed above, by member name.
        "chara",
        "motion",
        "project",
        "motionKey",
        "stealthChara",
        "stealthMotion",
        "speed",
        "tickCount",
        "frameTickCount",
        "completionType",
        "loopTime",
        "lastTime",
        "frameLastTime",
        "frameLoopTime",
        "opacity",
        "colorWeight",
        "maskMode",
        "coordinate",
        "visible",
        "playing",
        "allplaying",
        "resourceManager",
        "variableKeys",
        "outline",
        "tags",
        "onAction",
        "onSync",
        "onFindMotion",
        "animating",
        "play",
        "stop",
        "progress",
        "frameProgress",
        "setVariable",
        "getVariable",
        "contains",
        "draw",
        "clear",
        "setColor",
        "setCoord",
        "setRotate",
        "setScale",
        "setDrawAffineTranslateMatrix",
        "skip",
        "skipToSync",
        "pass",
        "serialize",
        "unserialize",
        "getMainTimelineLabelList",
        "getDiffTimelineLabelList",
        "getPlayingTimelineInfoList",
        "getLoopTimeline",
    ]
    .into_iter()
    .collect();

    for (name, default) in PLAYER_PLAIN_DEFAULTS {
        if !registered.contains(name) {
            register_stored_value_member(
                runtime,
                handle,
                name,
                format!("{class_name}.{name}"),
                Variant::Integer(*default),
            );
        }
    }
    for name in PLAYER_MEMBERS {
        if registered.contains(name) {
            continue;
        }
        register_unsupported_player_member(runtime, handle, class_name, name);
    }
    // Members the game calls that the dossier's `Player` list does not carry
    // (`AffineSourceMotion.tjs`'s emote branch). Registered on both classes so
    // a script written for either does not die on `MemberNotFound`.
    for name in ["initPhysics", "startWind", "stopWind"] {
        register_unsupported_player_member(runtime, handle, class_name, name);
    }
}

/// Registers one dossier member with no behaviour behind it: a method that
/// warns on first call and answers `Void`. Members whose reference shape is a
/// plain value (the camera/system group) answer a storable `0` instead.
fn register_unsupported_player_member(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &'static str,
    name: &'static str,
) {
    let member = format!("{class_name}.{name}");
    if PLAYER_CAMERA_MEMBERS.contains(&name) {
        register_stored_value_member(runtime, handle, name, member, Variant::Void);
        return;
    }
    runtime.register_object_native_with_arg_count(
        handle,
        name,
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              _this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            warn_unsupported(runtime, &member);
            Ok(Variant::Void)
        },
    );
}

/// A plain instance-data member: the setter stores what the script writes, the
/// getter returns it (or `default` before any write), and the first access —
/// read or write — logs the one-time "nothing consumes this here" warning.
fn register_stored_value_member(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
    member: String,
    default: Variant,
) {
    runtime.register_object_native_property(
        handle,
        name,
        {
            let member = member.clone();
            move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                warn_stored_but_unused(runtime, &member);
                Ok(
                    with_player(this_obj, |state| state.values.get(name).cloned())
                        .flatten()
                        .unwrap_or_else(|| default.clone()),
                )
            }
        },
        {
            let member = member.clone();
            move |runtime: &mut Runtime<KrkrHost>,
                  this_obj: Option<ObjectHandle>,
                  value: Variant| {
                warn_stored_but_unused(runtime, &member);
                with_player_mut(this_obj, |state| {
                    state.values.insert(name.to_owned(), value);
                });
                Ok(())
            }
        },
    );
}

/// A string member with getter and setter over [`PlayerState`].
fn register_player_string_property(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
) {
    runtime.register_object_native_property(
        handle,
        name,
        move |_runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let value = with_player(this_obj, |state| match name {
                "motion" => state.animation.clone(),
                "chara" => state.chara.clone(),
                _ => state.strings.get(name).cloned().unwrap_or_default(),
            });
            Ok(value.map(Variant::String).unwrap_or(Variant::Void))
        },
        move |_runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            let text = value.to_tjs_string()?;
            with_player_mut(this_obj, |state| match name {
                "motion" => state.animation = text,
                "chara" => state.chara = text,
                _ => {
                    state.strings.insert(name.to_owned(), text);
                }
            });
            Ok(())
        },
    );
}

fn register_player_number_property(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
) {
    runtime.register_object_native_property(
        handle,
        name,
        move |_runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let value = with_player(this_obj, |state| numeric_player_member(state, name)).flatten();
            Ok(match value {
                Some(value) => Variant::Real(value),
                None => Variant::Void,
            })
        },
        move |_runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            let number = value.to_real()?;
            with_player_mut(this_obj, |state| {
                set_numeric_player_member(state, name, number)
            });
            Ok(())
        },
    );
}

fn register_player_boolean_property(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
) {
    runtime.register_object_native_property(
        handle,
        name,
        move |_runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let value = with_player(this_obj, |state| match name {
                "visible" => state.visible,
                _ => false,
            });
            Ok(value.map_or(Variant::Void, |value| Variant::Integer(i64::from(value))))
        },
        move |_runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            let truthy = value.is_truthy();
            with_player_mut(this_obj, |state| {
                if name == "visible" {
                    state.visible = truthy;
                }
            });
            Ok(())
        },
    );
}

/// A read-only property computed from the player state.
fn register_player_readonly_property(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
    getter: fn(&PlayerState, &mut Runtime<KrkrHost>) -> Variant,
) {
    runtime.register_object_native_property(
        handle,
        name,
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(with_player(this_obj, |state| getter(state, runtime)).unwrap_or(Variant::Void))
        },
        move |_runtime: &mut Runtime<KrkrHost>,
              _this_obj: Option<ObjectHandle>,
              _value: Variant| Ok(()),
    );
}

/// `variableKeys` — the array the game's `AffineSourceMotion._getOptions`
/// iterates (`for (i = 0; i < _player.variableKeys.count; i++)` calling
/// `_player.getVariable(variableKeys[i])`, object 59 bytecode 225/234/245).
///
/// The reference's handler (`motionplayer_nod3d.dll` `FUN_10015690`, named by
/// `FUN_10091f10(0x100e2618, …)`) walks the player's own variable records
/// (`+0x2e8`, 0x30-byte entries whose first field is the key string, wrapped
/// into the collection by `FUN_100863b0`) plus its sub-objects
/// (`FUN_10015930`/`FUN_10017890`) and answers a TJS array of those key
/// strings.  The port's variable records are exactly the names its
/// `setVariable` writes create, so this returns those names (the player's
/// `BTreeMap` order) as a TJS array — `.count`, indexing and `getVariable`
/// round-trip the way the game uses them.
fn register_player_variable_keys(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    register_player_readonly_property(runtime, handle, "variableKeys", |state, runtime| {
        let keys: Vec<Variant> = state
            .variables
            .keys()
            .map(|name| Variant::String(name.clone()))
            .collect();
        Variant::Object(runtime.alloc_array_object(keys))
    });
}

/// `outline` holds script data (the reference's outline object); the port keeps
/// and returns whatever was written.
fn register_player_outline_property(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native_property(
        handle,
        "outline",
        |_runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(with_player(this_obj, |state| state.outline.clone())
                .flatten()
                .unwrap_or(Variant::Void))
        },
        |_runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            with_player_mut(this_obj, |state| state.outline = Some(value));
            Ok(())
        },
    );
}

/// A script-assigned member the port stores (callbacks, `tags`) where the
/// reference stores it too. Reading one back returns what was written.
fn register_player_script_member(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
) {
    runtime.set_object_member(handle, name, Variant::Void);
    let _ = name;
}

/// Reads from the player state behind `this`, if any.
fn with_player<R>(
    this_obj: Option<ObjectHandle>,
    read: impl FnOnce(&PlayerState) -> R,
) -> Option<R> {
    let this = this_obj?;
    PLAYERS.with(|players| players.borrow().get(&this).map(read))
}

/// Mutates the player state behind `this`, if any.
fn with_player_mut(this_obj: Option<ObjectHandle>, write: impl FnOnce(&mut PlayerState)) {
    let Some(this) = this_obj else {
        return;
    };
    PLAYERS.with(|players| {
        if let Some(state) = players.borrow_mut().get_mut(&this) {
            write(state);
        }
    });
}

/// The player's animation duration in raw ticks, when a motion is loaded.
fn player_duration_ticks(state: &PlayerState) -> Option<f64> {
    state
        .motion
        .as_ref()
        .and_then(|motion| motion.animation(&state.animation))
        .map(|animation| f64::from(animation.duration_ticks))
}

/// The loop point in raw ticks: a script override, else the animation's own
/// `loopTime`, else `-1` ("does not loop").
fn player_loop_ticks(state: &PlayerState) -> f64 {
    state
        .loop_time
        .or_else(|| {
            state
                .motion
                .as_ref()
                .and_then(|motion| motion.animation(&state.animation))
                .and_then(|animation| animation.loop_time)
                .map(f64::from)
        })
        .unwrap_or(-1.0)
}

/// Reads one plain numeric member.  The plain time members report milliseconds
/// and the `frame*` family reports raw 1/60 s ticks, matching the reference's
/// handlers (`motionplayer_nod3d.dll`: `tickCount` getter `FUN_10045c00` =
/// `pos×1000/60`, `lastTime`/`loopTime` getters `FUN_10045ea0`/`FUN_10045ec0`
/// the same, `frameTickCount` raw at `0x10030230`/`FUN_10045b50`).
fn numeric_player_member(state: &PlayerState, name: &str) -> Option<f64> {
    match name {
        "speed" => Some(state.speed),
        "tickCount" => Some(ticks_to_milliseconds(state.tick)),
        "completionType" => Some(state.completion_type as f64),
        "loopTime" => Some(ticks_to_milliseconds(player_loop_ticks(state))),
        "lastTime" => Some(ticks_to_milliseconds(
            player_duration_ticks(state).unwrap_or(0.0),
        )),
        "opacity" => Some(state.opacity),
        "colorWeight" => Some(state.colour_weight as f64),
        "maskMode" => Some(state.mask_mode as f64),
        "coordinate" => Some(state.coordinate),
        // The frame-granularity counters the reference exposes next to the
        // millisecond ones: raw ticks, and this model has one timeline, so
        // they mirror it.
        "frameTickCount" => Some(state.tick),
        "frameLastTime" => Some(player_duration_ticks(state).unwrap_or(0.0)),
        "frameLoopTime" => Some(player_loop_ticks(state)),
        _ => None,
    }
}

/// Writes one plain numeric member, converting milliseconds to raw ticks for
/// the plain time members (`tickCount` setter `FUN_10045ba0` = `v×60/1000`)
/// while `frameTickCount`/`frameLoopTime` take raw ticks.
fn set_numeric_player_member(state: &mut PlayerState, name: &str, value: f64) {
    match name {
        "speed" => state.speed = value,
        "tickCount" => state.tick = milliseconds_to_ticks(value).max(0.0),
        "frameTickCount" => state.tick = value.max(0.0),
        "completionType" => state.completion_type = value as i64,
        "loopTime" => state.loop_time = Some(milliseconds_to_ticks(value)),
        "frameLoopTime" => state.loop_time = Some(value),
        "opacity" => state.opacity = value.clamp(0.0, 255.0),
        "colorWeight" => state.colour_weight = value as u32,
        "maskMode" => state.mask_mode = value as i64,
        "coordinate" => state.coordinate = value,
        _ => {}
    }
}

/// Milliseconds → the adapter's 1/60 s tick axis (the reference's `progress`
/// handler multiplies by `60/1000`, `0x10030290`; the shared constant lives in
/// the adapter).
fn milliseconds_to_ticks(milliseconds: f64) -> f64 {
    milliseconds * f64::from(EMOTE_TICKS_PER_SECOND) / 1000.0
}

/// The adapter's 1/60 s tick axis → milliseconds (the reference's `tickCount`
/// / `lastTime` / `loopTime` getters multiply by `1000/60`).
fn ticks_to_milliseconds(ticks: f64) -> f64 {
    ticks * 1000.0 / f64::from(EMOTE_TICKS_PER_SECOND)
}

// ---------------------------------------------------------------------------
// Player methods
// ---------------------------------------------------------------------------

/// `play(motion, flags)`: resolves the animation across the player's resource
/// manager (or the global pool when it has none), puts the player at tick 0 and
/// starts it.
///
/// Flags are accepted and ignored apart from `PlayFlagForce`, which is what the
/// game passes at every call site: the reference uses the other bits to join or
/// chain a running timeline, and this port has one timeline per player.
fn player_play(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) else {
        return Ok(Variant::Void);
    };
    let animation = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();

    let manager = with_player(Some(this), |state| state.manager).flatten();
    let found = manager
        .and_then(|manager| find_motion_in_manager(&manager, &animation))
        .or_else(|| find_motion_in_pool(&GLOBAL_POOL, &animation));
    let Some((storage, motion)) = found else {
        runtime.host_mut().log(&format!(
            "motionplayer.dll: play({animation:?}) — no loaded motion has that animation \
             (load it through Motion.ResourceManager first)"
        ));
        with_player_mut(Some(this), |state| {
            state.animation = animation;
            state.motion = None;
            state.texture_cache = None;
            state.playing = false;
        });
        return Ok(Variant::Void);
    };

    with_player_mut(Some(this), |state| {
        if state
            .motion
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(current, &motion))
        {
            state.texture_cache = Some(TextureCache::new(Arc::clone(&motion)));
        }
        state.motion = Some(motion);
        state.motion_storage = Some(storage);
        state.animation = animation;
        state.tick = 0.0;
        state.tweens.clear();
        state.playing = true;
    });
    Ok(Variant::Void)
}

fn find_motion_in_manager(
    manager: &ObjectHandle,
    animation: &str,
) -> Option<(String, Arc<Motion>)> {
    MANAGERS.with(|managers| {
        let managers = managers.borrow();
        let state = managers.get(manager)?;
        state
            .loaded
            .iter()
            .find(|(_, loaded)| loaded.motion.animation(animation).is_some())
            .map(|(name, loaded)| (name.clone(), Arc::clone(&loaded.motion)))
    })
}

fn find_motion_in_pool(
    pool: &'static std::thread::LocalKey<RefCell<ManagerState>>,
    animation: &str,
) -> Option<(String, Arc<Motion>)> {
    pool.with(|pool| {
        let pool = pool.borrow();
        pool.loaded
            .iter()
            .find(|(_, loaded)| loaded.motion.animation(animation).is_some())
            .map(|(name, loaded)| (name.clone(), Arc::clone(&loaded.motion)))
    })
}

/// `stop()`: halts the motion where it is. The reference keeps the current
/// pose (the game calls `progress(1)` before `stop` when it wants the final
/// frame), and so does this port; `play` restarts from tick 0.
fn player_stop(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_player_mut(this_obj, |state| state.playing = false);
    Ok(Variant::Void)
}

/// `progress(milliseconds)`: advances the model's clock by
/// `milliseconds × 60/1000 × speed` ticks and ends a non-looping motion at its
/// duration.
///
/// This is the per-frame update the game's wrapper drives: the engine's
/// `onFlipTimerInterval` chain passes milliseconds end to end
/// (`EventIntf.cpp:951,991-997` `TVPGetTickCount` → `addContinuousHandler` →
/// `MainWindow` → `AffineLayer` → `_player.progress(_interval)`), and the
/// reference's handler converts to the 60 Hz tick axis with `×60/1000`
/// (`0x10030290`).
fn player_progress(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let milliseconds = args
        .first()
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    player_advance(runtime, this_obj, milliseconds_to_ticks(milliseconds))
}

/// `frameProgress(ticks)`: the raw handler (`0x10030370`), which advances by
/// the argument's own 1/60 s ticks without a unit conversion.
fn player_frame_progress(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let ticks = args
        .first()
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    player_advance(runtime, this_obj, ticks)
}

/// The shared body of `progress`/`frameProgress`: advance `this` by `ticks`
/// (already on the raw tick axis) and fire `onSync` when a one-shot ends.
fn player_advance(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    ticks: f64,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) else {
        return Ok(Variant::Void);
    };
    let finished = PLAYERS.with(|players| {
        let mut players = players.borrow_mut();
        let Some(state) = players.get_mut(&this) else {
            return false;
        };
        advance_player(state, ticks)
    });
    if finished && let Some(handler) = script_callback(runtime, this, "onSync") {
        runtime.call_function(handler, Vec::new())?;
    }
    Ok(Variant::Void)
}

/// Advances one player by `ticks` raw ticks; returns whether a non-looping
/// motion just finished.
fn advance_player(state: &mut PlayerState, ticks: f64) -> bool {
    let duration = player_duration_ticks(state).filter(|duration| *duration > 0.0);
    let loop_time = player_loop_ticks(state);

    if !state.playing || ticks == 0.0 {
        apply_variable_tweens(state);
        return false;
    }

    state.tick = (state.tick + ticks * state.speed).max(0.0);
    apply_variable_tweens(state);

    let Some(duration) = duration else {
        return false;
    };
    if loop_time >= 0.0 {
        if state.tick >= duration {
            // The reference wraps to the motion's `loopTime` (+0x150), not to
            // tick 0: a file with a mid-animation loop point re-enters at that
            // point. `(position - duration) % (duration - loopTime)` carries
            // however far past the end the step went.
            let span = duration - loop_time;
            state.tick = if span > 0.0 {
                loop_time + (state.tick - duration) % span
            } else {
                loop_time
            };
        }
        return false;
    }
    if state.tick >= duration {
        state.tick = duration;
        state.playing = false;
        return true;
    }
    false
}

/// Applies every timed `setVariable` write at the player's current position.
fn apply_variable_tweens(state: &mut PlayerState) {
    let tick = state.tick;
    let mut finished = Vec::new();
    for (index, tween) in state.tweens.iter().enumerate() {
        let t = if tween.duration_ticks <= 0.0 {
            1.0
        } else {
            ((tick - tween.start_tick) / tween.duration_ticks).clamp(0.0, 1.0)
        };
        let eased = ease(t, tween.easing);
        state.variables.insert(
            tween.name.clone(),
            tween.from + (tween.to - tween.from) * eased as f32,
        );
        if t >= 1.0 {
            finished.push(index);
        }
    }
    for index in finished.into_iter().rev() {
        state.tweens.remove(index);
    }
}

/// eluna's frame easing (`vendor/eluna/crates/eluna/src/emote.rs:1348-1356`):
/// `0` is linear, positive is smoothstep, negative is its inverse.
fn ease(t: f64, easing: f64) -> f64 {
    if !easing.is_finite() || easing == 0.0 {
        t
    } else if easing > 0.0 {
        t * t * (3.0 - 2.0 * t)
    } else {
        1.0 - (1.0 - t) * (1.0 - t)
    }
}

/// `setVariable(name, value[, time, easing])` — a timed write eases over
/// `time` ticks (the game converts its milliseconds with `* 60 / 1000`), an
/// untimed one lands immediately.
fn player_set_variable(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let value = args
        .get(1)
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0) as f32;
    let time = args
        .get(2)
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    let easing = args
        .get(3)
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    with_player_mut(this_obj, |state| {
        if time <= 0.0 || !time.is_finite() {
            state.variables.insert(name.clone(), value);
            state.tweens.retain(|tween| tween.name != name);
            return;
        }
        let from = state.variables.get(&name).copied().unwrap_or(0.0);
        state.tweens.push(VariableTween {
            name: name.clone(),
            from,
            to: value,
            start_tick: state.tick,
            duration_ticks: time,
            easing,
        });
    });
    Ok(Variant::Void)
}

fn player_get_variable(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let value = with_player(this_obj, |state| state.variables.get(&name).copied()).flatten();
    Ok(value
        .map(|value| Variant::Real(f64::from(value)))
        .unwrap_or(Variant::Void))
}

/// `contains(name)`: whether the player already knows the name — a set
/// variable, or a layer label of the animation being played. (The reference's
/// exact predicate was not recovered; this is the reading the game's usage
/// supports.)
fn player_contains(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let contains = with_player(this_obj, |state| {
        if state.variables.contains_key(&name) {
            return true;
        }
        state
            .motion
            .as_ref()
            .and_then(|motion| motion.animation(&state.animation))
            .is_some_and(|animation| animation.labels().contains(&name.as_str()))
    })
    .unwrap_or(false);
    Ok(Variant::Integer(i64::from(contains)))
}

/// `draw(layer)`: composites the sampled draw list into the layer's bitmap.
///
/// The sample happens before the layer callback (the layer API lends the
/// pixels through a closure), and the decode cache travels with it so a motion
/// decodes each texture once per player.
fn player_draw(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) else {
        return Ok(Variant::Void);
    };
    let layer = args
        .first()
        .and_then(Variant::object_handle)
        .ok_or_else(|| TjsError::runtime("Motion::Player::draw requires a layer"))?;
    refit_adaptor_canvas(runtime, layer)?;

    let prepared = PLAYERS.with(|players| {
        let mut players = players.borrow_mut();
        let state = players.get_mut(&this)?;
        let motion = Arc::clone(state.motion.as_ref()?);
        let animation = state.animation.clone();
        let tick = state.tick as f32;
        let variables = state.variables.clone();
        let Ok(mut items) = motion.draw_list_with_variables(&animation, tick, &variables) else {
            runtime.host_mut().log(&format!(
                "motionplayer.dll: draw: animation {animation:?} cannot be sampled"
            ));
            return None;
        };
        apply_player_transform(&mut items, state);
        let tint = state.drawn_tint();
        let cache = state.texture_cache.take();
        Some((items, tint, cache, motion))
    });
    let Some((items, tint, cache, motion)) = prepared else {
        return Ok(Variant::Void);
    };
    let mut cache = cache.unwrap_or_else(|| TextureCache::new(motion));

    layer_bitmap_write(runtime, layer, |view| {
        render_draw_list_into(
            view.pixels,
            view.bitmap.width,
            view.bitmap.height,
            &items,
            &mut cache,
            tint,
        )
    })?;

    for (resource, error) in cache.errors() {
        runtime.host_mut().log(&format!(
            "WARN motionplayer.dll: a texture of resource {resource} could not be decoded: {error}"
        ));
    }
    PLAYERS.with(|players| {
        if let Some(state) = players.borrow_mut().get_mut(&this) {
            state.texture_cache = Some(cache);
        }
    });
    Ok(Variant::Void)
}

/// Composes the player's placement into every item: `setCoord`/`setRotate`/
/// `setScale` around `setDrawAffineTranslateMatrix`, applied *after* the
/// model's own layer transforms.
fn apply_player_transform(items: &mut [MotionDrawItem], state: &PlayerState) {
    let matrix = player_matrix(state);
    if matrix == IDENTITY {
        return;
    }
    for item in items.iter_mut() {
        let world = item.world_transform.map(f64::from);
        let composed = compose(matrix, world);
        item.world_transform = composed.map(|value| value as f32);
    }
}

/// `T(coord) · R(rotation) · S(scale) · affine`, in the same 2x3 convention as
/// eluna's `EmoteTransform2D` (`x' = m11*x + m12*y + tx`).
fn player_matrix(state: &PlayerState) -> [f64; 6] {
    let affine = state.affine.unwrap_or(IDENTITY);
    let scale = if state.scale.is_finite() {
        state.scale
    } else {
        1.0
    };
    let rotation = if state.rotation.is_finite() {
        state.rotation
    } else {
        0.0
    };
    let (sin, cos) = rotation.sin_cos();
    let scale_rotate = [
        cos * scale,
        sin * scale,
        -sin * scale,
        cos * scale,
        0.0,
        0.0,
    ];
    let placed = compose(scale_rotate, affine);
    let mut out = placed;
    out[4] += state.coord[0];
    out[5] += state.coord[1];
    out
}

const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `outer ∘ inner` in the `[m11, m12, m21, m22, tx, ty]` convention.
fn compose(outer: [f64; 6], inner: [f64; 6]) -> [f64; 6] {
    [
        outer[0] * inner[0] + outer[1] * inner[2],
        outer[0] * inner[1] + outer[1] * inner[3],
        outer[2] * inner[0] + outer[3] * inner[2],
        outer[2] * inner[1] + outer[3] * inner[3],
        outer[0] * inner[4] + outer[1] * inner[5] + outer[4],
        outer[2] * inner[4] + outer[3] * inner[5] + outer[5],
    ]
}

impl PlayerState {
    /// The colour filter `draw` applies: `setColor`'s ARGB (or `colorWeight`)
    /// with the player's `opacity` folded into its alpha.
    fn drawn_tint(&self) -> Tint {
        let mut tint = Tint::from_emote_colour(self.colour);
        tint.alpha *= (self.opacity / 255.0).clamp(0.0, 1.0) as f32;
        if !self.visible {
            tint.alpha = 0.0;
        }
        tint
    }
}

/// `clear(layer[, colour])`: fills the layer's bitmap, transparent by default.
///
/// The game calls this on the work layer before `draw` (`AffineSourceMotion.tjs`
/// `_drawAffine`/`drawAffine`), passing the layer's `neutralColor`.
fn player_clear(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = args
        .first()
        .and_then(Variant::object_handle)
        .ok_or_else(|| TjsError::runtime("Motion::Player::clear requires a layer"))?;
    let argb = args
        .get(1)
        .map(Variant::to_integer)
        .transpose()?
        .map(|value| value as u32)
        .unwrap_or(0);
    refit_adaptor_canvas(runtime, layer)?;
    layer_bitmap_write(runtime, layer, |view| {
        for pixel in view.pixels.as_chunks_mut::<4>().0 {
            pixel[0] = ((argb >> 16) & 0xff) as u8;
            pixel[1] = ((argb >> 8) & 0xff) as u8;
            pixel[2] = (argb & 0xff) as u8;
            pixel[3] = ((argb >> 24) & 0xff) as u8;
        }
    })?;
    Ok(Variant::Void)
}

/// `setColor(argb[, time, easing])`: the character colour filter. The game
/// passes `0xFF808080` for "none" and `0xFF000000 | colour` for a filter, and
/// the two extra arguments of the timed form (milliseconds and easing).
fn player_set_colour(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let colour = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0) as u32;
    with_player_mut(this_obj, |state| {
        state.colour = colour;
        state.colour_weight = colour;
    });
    Ok(Variant::Void)
}

fn player_set_coord(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let x = args
        .first()
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    let y = args
        .get(1)
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    with_player_mut(this_obj, |state| state.coord = [x, y]);
    Ok(Variant::Void)
}

/// `setRotate(radians)` — the game passes `-degrees * PI * 2 / 360`.
fn player_set_rotate(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let rotation = args
        .first()
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
    with_player_mut(this_obj, |state| state.rotation = rotation);
    Ok(Variant::Void)
}

fn player_set_scale(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let scale = args
        .first()
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(1.0);
    with_player_mut(this_obj, |state| state.scale = scale);
    Ok(Variant::Void)
}

/// `setDrawAffineTranslateMatrix(m11, m21, m12, m22, tx, ty)`: the layer
/// matrix that places the model in the target layer.
///
/// The argument order is the game's own, and it is *column* order for the two
/// off-diagonal terms: `AffineSourceMotion.tjs` passes
/// `setDrawAffineTranslateMatrix(a.m11, a.m21, a.m12, a.m22, a.m14, a.m24)` in
/// its `Transform`-matrix branch (`:3143-3151`) and `(a, c, b, d, tx, ty)` in
/// the live branch (`:3172`), where the forward matrix is
/// `[[a, b], [c, d]]` — its `revmtx` inverse is spelled out one line later as
/// `a = d/det, b = -c/det, c = -b/det, d = a/det` with `det = a*d - b*c`, which
/// only holds for that reading. A transposed read applies the inverse rotation
/// and leaves pure scale/translate/mirror matrices correct, which is exactly
/// what `affine_matrix_rotates_by_the_games_argument_order` pins down.
fn player_set_affine(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let mut arg = [0.0f64; 6];
    for (index, slot) in arg.iter_mut().enumerate() {
        *slot = args
            .get(index)
            .map(Variant::to_real)
            .transpose()?
            .unwrap_or(0.0);
    }
    // [m11, m21, m12, m22, tx, ty] in, [m11, m12, m21, m22, tx, ty] stored.
    let matrix = [arg[0], arg[2], arg[1], arg[3], arg[4], arg[5]];
    with_player_mut(this_obj, |state| state.affine = Some(matrix));
    Ok(Variant::Void)
}

/// `skip()` / `skipToSync()`: run the motion out to its end.
///
/// The game uses the pair to finish an animation before stopping it
/// (`stopMovie()` does `skipToSync(), progress(1), stop()`); for a one-shot
/// motion the end is the duration, and for a looping one these are no-ops —
/// there is nothing to skip to.
fn player_skip(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    with_player_mut(this_obj, |state| {
        let duration = state
            .motion
            .as_ref()
            .and_then(|motion| motion.animation(&state.animation))
            .map(|animation| f64::from(animation.duration_ticks));
        let looping = state
            .motion
            .as_ref()
            .and_then(|motion| motion.animation(&state.animation))
            .and_then(|animation| animation.loop_time)
            .is_some_and(|loop_time| loop_time >= 0.0);
        if let Some(duration) = duration
            && !looping
        {
            state.tick = duration;
            state.playing = false;
        }
    });
    Ok(Variant::Void)
}

/// `pass()` is the emote path's "wait for the next sync point"; the `.mtn`
/// motion path has no sync events, so it is a diagnosed no-op.
fn player_pass(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    warn_unsupported(runtime, "EmotePlayer.pass");
    Ok(Variant::Void)
}

/// `serialize()` / `unserialize(value)`: an opaque state payload the game uses
/// to clone a player (`createPlayer(a0)` does
/// `_player.unserialize(a0._player.serialize())`). The reference's binary
/// format is not recovered, so this port round-trips its own text payload.
fn player_serialize(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let payload = with_player(this_obj, |state| {
        format!(
            "{}|{}|{}|{}",
            state.chara, state.animation, state.tick, state.speed
        )
    })
    .unwrap_or_default();
    Ok(Variant::String(payload))
}

fn player_unserialize(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let payload = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let mut parts = payload.split('|');
    let chara = parts.next().unwrap_or_default().to_owned();
    let animation = parts.next().unwrap_or_default().to_owned();
    let tick = parts
        .next()
        .unwrap_or_default()
        .parse::<f64>()
        .unwrap_or(0.0);
    let speed = parts
        .next()
        .unwrap_or_default()
        .parse::<f64>()
        .unwrap_or(1.0);
    with_player_mut(this_obj, |state| {
        state.chara = chara;
        state.animation = animation;
        state.tick = tick;
        state.speed = speed;
    });
    Ok(Variant::Void)
}

/// `getMainTimelineLabelList()`: the loaded motion's animation names — the
/// closest thing a `.mtn` model has to the reference's main timelines.
fn player_label_list(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let names = with_player(this_obj, |state| {
        state
            .motion
            .as_ref()
            .map(|motion| {
                motion
                    .animations()
                    .iter()
                    .map(|animation| animation.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    })
    .unwrap_or_default();
    let values = names.into_iter().map(Variant::String).collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

fn player_empty_list(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

/// `getPlayingTimelineInfoList()`: one `{ label }` entry while a motion plays.
fn player_playing_timeline_info(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let playing = with_player(this_obj, |state| {
        state.playing.then(|| state.animation.clone())
    })
    .flatten();
    let entries = match playing {
        Some(animation) => {
            let entry = runtime.alloc_dictionary_object();
            runtime.set_object_member(entry, "label", Variant::String(animation));
            vec![Variant::Object(entry)]
        }
        None => Vec::new(),
    };
    Ok(Variant::Object(runtime.alloc_array_object(entries)))
}

fn player_zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// The reader returns the property object for a class-level read and the bound
/// instance for an instance read; both spellings must land on the same object.
fn bound_instance(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    class_name: &'static str,
) -> ObjectHandle {
    let instance = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(instance, class_name);
    instance
}

fn return_this(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(this_obj.map(Variant::Object).unwrap_or_default())
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

/// A script callback on the player (`onSync`, `onAction`, `onFindMotion`),
/// when it is a function.
fn script_callback(
    runtime: &Runtime<KrkrHost>,
    player: ObjectHandle,
    name: &str,
) -> Option<Variant> {
    let value = runtime.object_member(player, name);
    (matches!(value, Variant::Closure(_)) || value.object_handle().is_some()).then_some(value)
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use krkr_assets::ProjectStorage;
    use krkr_core::{DrawCommand, FrameInput, Size};
    use krkr_engine::{EngineConfig, EngineInput, KrkrEngine};

    use super::MotionPlayerPlugin;

    /// The M50 synthetic PSB writer, reused verbatim from the adapter crate's
    /// test support so both sides build the same containers.
    mod psb_write {
        // The fixture builder is shared verbatim with the adapter crate's
        // tests; not every helper is used on this side.
        #![allow(dead_code)]
        include!("../../krkr-emote/tests/support/psb_write.rs");
    }

    use psb_write::{PsbWriter, Value, int, list, object, text};

    const MOTION_STORAGE: &str = "motion/hero.mtn";

    /// A 4x4 RGBA block of one colour, written as an uncompressed icon
    /// resource (`compress` absent, so the adapter reads it raw).
    fn block(colour: [u8; 4]) -> Vec<u8> {
        colour.iter().cycle().take(4 * 4 * 4).copied().collect()
    }

    fn icon(pixel: &Value) -> Value {
        object(vec![
            ("pixel", pixel.clone()),
            ("width", int(4)),
            ("height", int(4)),
            ("originX", int(0)),
            ("originY", int(0)),
            ("resolution", int(1)),
            ("attr", int(0)),
        ])
    }

    fn content(src: &'static str, coord: [i64; 2], opa: i64) -> Value {
        object(vec![
            ("src", text(src)),
            ("coord", list(vec![int(coord[0]), int(coord[1]), int(0)])),
            ("ox", int(0)),
            ("oy", int(0)),
            ("opa", int(opa)),
        ])
    }

    /// A one-icon, one-layer `idle` motion in the PARQUET source flavor,
    /// 60 ticks long (the length every fixture except the `ef_moya`-shaped one
    /// uses).
    fn motion_bytes(icons: Vec<(&'static str, [u8; 4])>, layer: Value, loop_time: i64) -> Vec<u8> {
        motion_bytes_timed(icons, layer, 60, loop_time)
    }

    /// [`motion_bytes`] with an explicit `lastTime`.
    fn motion_bytes_timed(
        icons: Vec<(&'static str, [u8; 4])>,
        layer: Value,
        last_time: i64,
        loop_time: i64,
    ) -> Vec<u8> {
        let mut writer = PsbWriter::default();
        let mut icon_fields = Vec::new();
        for (name, colour) in icons {
            let pixel = writer.add_resource(block(colour));
            icon_fields.push((name, icon(&pixel)));
        }
        let root = motion_root(icon_fields, layer, last_time, loop_time, None);
        writer.finish(4, &root)
    }

    /// [`motion_bytes_timed`] for a *scene* motion: the root carries the
    /// `screenSize` a `.mtn` authored for a whole screen declares
    /// (`title_bg.mtn`: `width 1920, height 1080, originX 0, originY 0`), which
    /// is the canvas the script-image graphic loader draws into.
    fn motion_bytes_on_screen(
        icons: Vec<(&'static str, [u8; 4])>,
        layer: Value,
        last_time: i64,
        loop_time: i64,
        screen_size: [i64; 4],
    ) -> Vec<u8> {
        let mut writer = PsbWriter::default();
        let mut icon_fields = Vec::new();
        for (name, colour) in icons {
            let pixel = writer.add_resource(block(colour));
            icon_fields.push((name, icon(&pixel)));
        }
        let root = motion_root(icon_fields, layer, last_time, loop_time, Some(screen_size));
        writer.finish(4, &root)
    }

    /// The PSB root of a one-source, one-`idle`-motion fixture, with the
    /// optional `screenSize` block (`[width, height, originX, originY]`).
    fn motion_root(
        icon_fields: Vec<(&'static str, Value)>,
        layer: Value,
        last_time: i64,
        loop_time: i64,
        screen_size: Option<[i64; 4]>,
    ) -> Value {
        let mut root_fields = vec![
            ("id", text("motion")),
            ("label", text("Synthetic")),
            ("metadata", Value::Null),
            (
                "source",
                object(vec![(
                    "hero",
                    object(vec![("type", int(1)), ("icon", object(icon_fields))]),
                )]),
            ),
            (
                "object",
                object(vec![(
                    "hero",
                    object(vec![
                        ("metadata", Value::Null),
                        (
                            "motion",
                            object(vec![(
                                "idle",
                                object(vec![
                                    ("lastTime", int(last_time)),
                                    ("loopTime", int(loop_time)),
                                    ("layer", list(vec![layer])),
                                ]),
                            )]),
                        ),
                    ]),
                )]),
            ),
        ];
        if let Some([width, height, origin_x, origin_y]) = screen_size {
            root_fields.push((
                "screenSize",
                object(vec![
                    ("width", int(width)),
                    ("height", int(height)),
                    ("originX", int(origin_x)),
                    ("originY", int(origin_y)),
                ]),
            ));
        }
        object(root_fields)
    }

    /// One layer whose single frame carries `src` at `coord`.
    fn single_frame_layer(src: &'static str, coord: [i64; 2], opa: i64) -> Value {
        object(vec![
            ("label", text("body")),
            ("coordinate", int(0)),
            ("children", list(vec![])),
            (
                "frameList",
                list(vec![
                    object(vec![
                        ("content", content(src, coord, opa)),
                        ("time", int(0)),
                        ("type", int(2)),
                    ]),
                    object(vec![("time", int(60)), ("type", int(0))]),
                ]),
            ),
        ])
    }

    /// A parameterised layer: the variable `x` picks between two frames.
    ///
    /// `division` is the parameter-to-time scale: the reference's
    /// `EPParameter::SetValue` maps the value as
    /// `(value - rangeBegin) * division / (rangeEnd - rangeBegin)`
    /// (`vendor/eluna/crates/eluna/src/emote.rs:4527-4531`, which eluna
    /// implements at the fork head), so `x` in 0..1 has to reach the frame
    /// list's 100-tick span through the field. The previous eluna pin scaled
    /// the value over the frame list's own span instead and this fixture
    /// carried no `division`; with `division: 100`, `x = 1` lands on the red
    /// frame at t=100.
    fn parameterised_layer() -> Value {
        object(vec![
            ("label", text("body")),
            ("coordinate", int(0)),
            ("children", list(vec![])),
            (
                "parameterize",
                object(vec![
                    ("id", text("x")),
                    ("rangeBegin", int(0)),
                    ("rangeEnd", int(1)),
                    ("division", int(100)),
                ]),
            ),
            (
                "frameList",
                list(vec![
                    object(vec![
                        ("content", content("src/hero/white", [8, 8], 255)),
                        ("time", int(0)),
                        ("type", int(2)),
                    ]),
                    object(vec![
                        ("content", content("src/hero/red", [16, 16], 255)),
                        ("time", int(100)),
                        ("type", int(3)),
                    ]),
                ]),
            ),
        ])
    }

    fn engine_with(scripts: &[(&str, Vec<u8>)]) -> KrkrEngine {
        let storage = ProjectStorage::from_memory(
            scripts
                .iter()
                .map(|(name, bytes)| ((*name).to_owned(), bytes.clone())),
        );
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(MotionPlayerPlugin).expect("plugin");
        engine
    }

    fn integer(engine: &mut KrkrEngine, expression: &str) -> i64 {
        engine
            .execute_expression("read.tjs", expression)
            .unwrap_or_else(|error| panic!("{expression}: {error}"))
            .to_integer()
            .expect("integer")
    }

    fn real(engine: &mut KrkrEngine, expression: &str) -> f64 {
        engine
            .execute_expression("read.tjs", expression)
            .unwrap_or_else(|error| panic!("{expression}: {error}"))
            .to_real()
            .expect("real")
    }

    /// Sets up the pool, one player and a 32x32 work layer.
    const SETUP: &str = r#"
        global.rm = new Motion.ResourceManager(0, 0);
        global.res = rm.load("motion/hero.mtn");
        global.player = new Motion.Player(rm);
        global.layer = new Layer();
        layer.setImageSize(32, 32);
        layer.fillRect(0, 0, 32, 32, 0x00000000);
    "#;

    /// The dossier's 81 `Player` members plus the `EmotePlayer`-only members
    /// the game calls, and the class surface: the recovered twelve names live
    /// on `Motion.ResourceManager` (their registration table's class), the
    /// `Motion` class object carries its constants, its one own member and the
    /// sub-class items.
    #[test]
    fn surface_matches_the_reference_tables() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        for name in [
            "loadSource",
            "clearCache",
            "bufLayer",
            "load",
            "unload",
            "unloadAll",
            "isExistMotion",
            "findMotion",
            "findSource",
            "random",
            "requireLayerId",
            "releaseLayerId",
        ] {
            let found = engine
                .execute_expression(
                    "surface.tjs",
                    &format!("typeof Motion.ResourceManager.{name} != \"void\""),
                )
                .expect("read")
                .is_truthy();
            assert!(found, "Motion.ResourceManager.{name} is registered");
        }
        // The reference `Motion` class registers no such members: its table is
        // constants + `doAlphaMaskOperation` + the sub-class items, and a
        // script that reads `Motion.loadSource` gets `void` on the real DLL.
        for name in [
            "loadSource",
            "clearCache",
            "bufLayer",
            "load",
            "unload",
            "unloadAll",
            "isExistMotion",
            "findMotion",
            "findSource",
            "random",
            "requireLayerId",
            "releaseLayerId",
        ] {
            let missing = engine
                .execute_expression("surface.tjs", &format!("typeof Motion.{name}"))
                .expect("read")
                .to_tjs_string()
                .expect("string");
            assert_eq!(missing, "undefined", "Motion.{name} must not be registered");
        }
        // The one member the reference `Motion` does register next to its
        // constants (`0x100900d0`, the only `InvokeCommand<Motion, …>` RTTI).
        assert!(
            engine
                .execute_expression(
                    "surface.tjs",
                    "typeof Motion.doAlphaMaskOperation != \"void\""
                )
                .expect("read")
                .is_truthy(),
            "Motion.doAlphaMaskOperation is registered"
        );
        for (name, value) in [("PlayFlagForce", 1), ("MaskModeAlpha", 1)] {
            assert_eq!(
                integer(&mut engine, &format!("Motion.{name}")),
                value,
                "Motion.{name}"
            );
        }
        // The nod3d build has no `useD3D`, and the game's probe depends on it
        // being absent (it catches the read and sets enableD3D = 0).
        let use_d3d = engine
            .execute_expression("surface.tjs", "typeof Motion.Player.useD3D")
            .expect("read")
            .to_tjs_string()
            .expect("string");
        assert_eq!(use_d3d, "undefined");

        // Every dossier member is reachable: a script read of a missing
        // member throws `MemberNotFound` on a class object, so each name is
        // probed through a try/catch.
        for name in super::PLAYER_MEMBERS {
            let script = format!(
                "try {{ Motion.Player.{name}; global.reachable = 1; }} \
                 catch (e) {{ global.reachable = 0; }}"
            );
            engine
                .execute_script("probe.tjs", &script)
                .unwrap_or_else(|error| panic!("probing Motion.Player.{name}: {error}"));
            assert_eq!(
                integer(&mut engine, "reachable"),
                1,
                "Motion.Player.{name} is reachable"
            );
        }
        for name in ["animating", "skip", "pass", "initPhysics", "startWind"] {
            let script = format!(
                "try {{ Motion.EmotePlayer.{name}; global.reachable = 1; }} \
                 catch (e) {{ global.reachable = 0; }}"
            );
            engine
                .execute_script("probe.tjs", &script)
                .unwrap_or_else(|error| panic!("probing Motion.EmotePlayer.{name}: {error}"));
            assert_eq!(
                integer(&mut engine, "reachable"),
                1,
                "Motion.EmotePlayer.{name} is reachable"
            );
        }
    }

    /// Runs `expression` and answers whether it threw ("err") or not ("ok").
    fn call_outcome(engine: &mut KrkrEngine, expression: &str) -> &'static str {
        engine
            .execute_script(
                "call.tjs",
                &format!(
                    "global.outcome = \"ok\"; \
                     try {{ {expression} }} catch (e) {{ global.outcome = \"err\"; }}"
                ),
            )
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        if engine
            .execute_expression("call.tjs", "outcome == \"ok\"")
            .expect("read outcome")
            .is_truthy()
        {
            "ok"
        } else {
            "err"
        }
    }

    /// `ResourceManager`'s argument floors against the reference's `ArgsCount`
    /// (ncbind rejects `numparams < ArgsCount`, so the reference accepts the
    /// count and above; extra arguments are dropped). The anchors per member are
    /// in [`super::RESOURCE_MANAGER_METHODS`] — in particular `requireLayerId`
    /// takes **no** argument in the reference, which the M166 survey read as
    /// `AtLeast(1)`.
    #[test]
    fn resource_manager_floors_are_the_reference_arities() {
        let mut engine = engine_with(&[]);
        engine
            .execute_script("setup.tjs", "global.rm = new Motion.ResourceManager(0, 0);")
            .expect("manager");
        for (minimum, short) in [
            ("rm.load(\"m\")", "rm.load()"),
            ("rm.unload(\"m\")", "rm.unload()"),
            ("rm.loadSource(0, 0)", "rm.loadSource(0)"),
            ("rm.isExistMotion(\"a\", \"b\")", "rm.isExistMotion(\"a\")"),
            ("rm.findMotion(\"a\", \"b\")", "rm.findMotion(\"a\")"),
            ("rm.findSource(\"a\", \"b\")", "rm.findSource(\"a\")"),
            ("rm.releaseLayerId(1)", "rm.releaseLayerId()"),
        ] {
            assert_eq!(
                call_outcome(&mut engine, &format!("{minimum};")),
                "ok",
                "{minimum} is the reference minimum and must be accepted"
            );
            assert_eq!(
                call_outcome(&mut engine, &format!("{short};")),
                "err",
                "{short} is one argument short of the reference floor"
            );
        }
        // `ArgsCount` 0: the reference's check never fires, so any argument
        // count passes and surplus arguments are dropped.
        for call in [
            "rm.clearCache()",
            "rm.clearCache(1, 2)",
            "rm.unloadAll()",
            "rm.bufLayer()",
            "rm.random()",
            "rm.requireLayerId()",
            "rm.requireLayerId(1, 2)",
        ] {
            assert_eq!(
                call_outcome(&mut engine, &format!("{call};")),
                "ok",
                "{call} is a 0-argument reference member"
            );
        }
    }

    /// The `Player`/`EmotePlayer` floors against the reference's member
    /// signatures (`EmotePlayer` ctor anchors beside
    /// [`super::install_player_members`]). `getVariable(name)` is the decisive
    /// one: the M166 survey read it as a six-double method, but the game's
    /// 1-argument call works on the shipped DLL, and the corrected pairing is
    /// `double (EmotePlayer::*)(tTJSString) const` (`0x10099a60`).
    #[test]
    fn player_floors_are_the_reference_arities() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script("emote.tjs", "global.emote = new Motion.EmotePlayer(rm);")
            .expect("emote player");

        for (minimum, short) in [
            (
                "player.play(\"idle\", Motion.PlayFlagForce)",
                "player.play(\"idle\")",
            ),
            ("player.progress(0)", "player.progress()"),
            ("player.frameProgress(0)", "player.frameProgress()"),
            ("player.getVariable(\"x\")", "player.getVariable()"),
            ("player.setVariable(\"x\", 1)", "player.setVariable(\"x\")"),
            ("player.setCoord(1, 2)", "player.setCoord(1)"),
            ("player.setColor(0xFF808080)", "player.setColor()"),
            ("player.setRotate(0)", "player.setRotate()"),
            ("player.setScale(1)", "player.setScale()"),
            (
                "player.setDrawAffineTranslateMatrix(1, 0, 0, 1, 0, 0)",
                "player.setDrawAffineTranslateMatrix(1, 0, 0, 1, 0)",
            ),
            ("player.clear(layer, 0)", "player.clear(layer)"),
            ("player.draw(layer)", "player.draw()"),
            ("player.unserialize(1)", "player.unserialize()"),
            // `contains` is class-shaped: `bool (EmotePlayer::*)(tTJSString,
            // double, double)` on the emote class, `bool (Player::*)(double,
            // double)` on the plain one.
            (
                "emote.contains(\"hit_bust\", 0, 0)",
                "emote.contains(\"hit_bust\", 0)",
            ),
            ("player.contains(0, 0)", "player.contains(0)"),
        ] {
            assert_eq!(
                call_outcome(&mut engine, &format!("{minimum};")),
                "ok",
                "{minimum} is the reference minimum and must be accepted"
            );
            assert_eq!(
                call_outcome(&mut engine, &format!("{short};")),
                "err",
                "{short} is one argument short of the reference floor"
            );
        }
        // 0-argument members (`skip`/`skipToSync`/`pass`/`stop`/`serialize`).
        for call in ["player.serialize()", "player.stop()", "player.skip()"] {
            assert_eq!(
                call_outcome(&mut engine, &format!("{call};")),
                "ok",
                "{call}"
            );
        }
    }

    /// PARQUET's `AffineSourceMotion` emote path, spelled as the decompiled
    /// wrapper spells it (`data.xp3` `system/AffineSourceMotion.tjs`):
    /// `new Motion.ResourceManager(path, cache)` → `rm.load(file)` →
    /// `new Motion.EmotePlayer(rm)` → `play(name, Motion.PlayFlagForce)`,
    /// `setVariable`/`getVariable`/`variableKeys`, `setCoord`, the six-argument
    /// affine matrix, `contains("hit_" + label, x, y)`, `clear(adaptor, colour)`
    /// and `rm.unload(file)`. Every one of these shapes has to resolve and run.
    #[test]
    fn parquet_emote_call_shapes_still_resolve() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        engine
            .execute_script(
                "parquet.tjs",
                r#"
                global.rm = new Motion.ResourceManager("motion", 20971520);
                global.res = rm.load("motion/hero.mtn");
                global.owner = new Layer();
                owner.setPos(0, 0);
                owner.setSize(48, 48);
                global.adaptor = new Motion.SeparateLayerAdaptor(owner incontextof global.Layer);
                global._player = new Motion.EmotePlayer(rm);
                _player.maskMode = Motion.MaskModeAlpha;
                _player.chara = "hero";
                _player.play("idle", Motion.PlayFlagForce);
                _player.initPhysics(%[ "base" => %[ "chara" => "hero", "motion" => "idle" ] ]);
                _player.progress(0);
                _player.setVariable("face_mouth", 1, 0, 0);
                _player.setCoord(3, 4);
                _player.setColor(0xFF808080);
                _player.setDrawAffineTranslateMatrix(1, 0, 0, 1, 0, 0);
                _player.progress(0);
                _player.clear(adaptor, 0xFF808080);
                _player.draw(adaptor);
                _player.setVariable("x", 0.5);
                global.v = _player.getVariable("x");
                global.keys = _player.variableKeys.count;
                global.hit = _player.contains("hit_bust", 10, 10);
                global.serial = _player.serialize();
                _player.unserialize(serial);
                owner.assignImages(adaptor);
                rm.unload("motion/hero.mtn");
                rm.clearCache();
                "#,
            )
            .expect("the game's emote call shapes");
        assert_eq!(real(&mut engine, "v"), 0.5, "variables round-trip");
        assert_eq!(
            integer(&mut engine, "keys"),
            2,
            "variableKeys is an array of both written names"
        );
        // `setCoord(3, 4)` shifts the icon from its authored coord (8, 8): the
        // 4x4 quad covers pixels 9..13 × 10..14 instead of 6..10 × 6..10.
        assert_eq!(
            integer(&mut engine, "adaptor.getMainPixel(11, 12)"),
            0x00ff_ffff,
            "the drawn motion reaches the adaptor the game publishes"
        );
        assert_eq!(
            integer(&mut engine, "owner.getMainPixel(11, 12)"),
            0x00ff_ffff
        );
        assert_eq!(
            integer(&mut engine, "owner.getMainPixel(2, 2)"),
            0x808080,
            "the cleared background travels with the canvas"
        );
    }

    /// The end-to-end path: load, play, progress and draw reaching the layer
    /// bitmap the plugin-facing layer API hands out.
    #[test]
    fn synthetic_motion_draws_into_the_layer_bitmap() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        assert!(
            !engine
                .execute_expression("res.tjs", "res === null")
                .expect("read")
                .is_truthy(),
            "the resource handle is an object"
        );
        assert_eq!(
            integer(&mut engine, "player.playing"),
            0,
            "idle before play"
        );

        engine
            .execute_script("play.tjs", "player.play(\"idle\", Motion.PlayFlagForce);")
            .expect("play");
        assert_eq!(integer(&mut engine, "player.playing"), 1);
        assert_eq!(integer(&mut engine, "player.motion == \"idle\""), 1);

        // Nothing drawn yet: the layer is still transparent.
        assert_eq!(integer(&mut engine, "layer.getMainPixel(7, 7)"), 0);

        engine
            .execute_script("tick.tjs", "player.progress(500);")
            .expect("progress");
        // `progress` takes milliseconds; the tick axis is 60/s.
        assert_eq!(integer(&mut engine, "player.frameTickCount"), 30);
        assert_eq!(integer(&mut engine, "player.tickCount"), 500);

        engine
            .execute_script("draw.tjs", "player.draw(layer);")
            .expect("draw");
        // The 4x4 icon at coord (8, 8) covers pixels 6..10.
        for (x, y) in [(6, 6), (7, 7), (9, 9)] {
            assert_eq!(
                integer(&mut engine, &format!("layer.getMainPixel({x}, {y})")),
                0x00ff_ffff,
                "the white icon reaches the layer at {x},{y}"
            );
        }
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(20, 20)"),
            0,
            "outside the quad stays clear"
        );
    }

    /// `play`/`stop`/`progress` are a real state machine on the millisecond
    /// axis: a non-looping motion ends at its duration, `stop` freezes the
    /// position, `progress(0)` (the game's paused frame) does not advance, and
    /// `play` restarts.
    #[test]
    fn play_stop_progress_is_a_state_machine() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        // The fixture motion is 60 ticks long: 1000 ms on the script axis.
        assert_eq!(integer(&mut engine, "player.lastTime"), 0, "no motion yet");
        engine
            .execute_script("play.tjs", "player.play(\"idle\", 0);")
            .expect("play");
        assert_eq!(integer(&mut engine, "player.lastTime"), 1000);
        assert_eq!(integer(&mut engine, "player.frameLastTime"), 60);
        engine
            .execute_script("run.tjs", "player.progress(250); player.progress(0);")
            .expect("progress");
        assert_eq!(
            integer(&mut engine, "player.frameTickCount"),
            15,
            "progress(0) pauses"
        );
        assert_eq!(integer(&mut engine, "player.tickCount"), 250);
        assert_eq!(integer(&mut engine, "player.playing"), 1);

        engine
            .execute_script("speed.tjs", "player.speed = 2; player.progress(250);")
            .expect("speed");
        assert_eq!(
            integer(&mut engine, "player.frameTickCount"),
            45,
            "speed scales the step"
        );
        assert_eq!(
            integer(&mut engine, "player.tickCount"),
            750,
            "45 raw ticks report as 750 ms"
        );

        engine
            .execute_script("finish.tjs", "player.progress(2000);")
            .expect("finish");
        assert_eq!(
            integer(&mut engine, "player.frameTickCount"),
            60,
            "clamped at the duration"
        );
        assert_eq!(integer(&mut engine, "player.tickCount"), 1000);
        assert_eq!(
            integer(&mut engine, "player.playing"),
            0,
            "a one-shot motion ends"
        );

        engine
            .execute_script("stop.tjs", "player.stop();")
            .expect("stop");
        assert_eq!(integer(&mut engine, "player.playing"), 0);
        assert_eq!(
            integer(&mut engine, "player.frameTickCount"),
            60,
            "stop keeps the position"
        );

        engine
            .execute_script("replay.tjs", "player.speed = 1; player.play(\"idle\", 0);")
            .expect("replay");
        assert_eq!(
            integer(&mut engine, "player.frameTickCount"),
            0,
            "play restarts"
        );
        assert_eq!(integer(&mut engine, "player.tickCount"), 0);
        assert_eq!(integer(&mut engine, "player.playing"), 1);
    }

    /// The discriminating unit test from M128's finding: `progress` takes
    /// milliseconds and lands on the 60 Hz tick axis (`×60/1000`), so a
    /// 1000 ms step is exactly 60 ticks, while `frameProgress` stays raw. The
    /// fixture mirrors `sd101.mtn`'s `ef_moya` shape — 180 ticks long, second
    /// keyframe at tick 90 — so `progress(1500)` has to reach that second
    /// keyframe.
    #[test]
    fn progress_is_milliseconds_and_frame_progress_is_raw_ticks() {
        /// One layer, two keyframes: `white` at (8, 8) until tick 90, then
        /// `red` at (16, 16) until the 180-tick end.
        fn ef_moya_layer() -> Value {
            object(vec![
                ("label", text("body")),
                ("coordinate", int(0)),
                ("children", list(vec![])),
                (
                    "frameList",
                    list(vec![
                        object(vec![
                            ("content", content("src/hero/white", [8, 8], 255)),
                            ("time", int(0)),
                            ("type", int(2)),
                        ]),
                        object(vec![
                            ("content", content("src/hero/red", [16, 16], 255)),
                            ("time", int(90)),
                            ("type", int(3)),
                        ]),
                        object(vec![("time", int(180)), ("type", int(0))]),
                    ]),
                ),
            ])
        }

        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes_timed(
                vec![("white", [255, 255, 255, 255]), ("red", [255, 0, 0, 255])],
                ef_moya_layer(),
                180,
                -1,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script("play.tjs", "player.play(\"idle\", 0);")
            .expect("play");

        engine
            .execute_script("ms.tjs", "player.progress(1000);")
            .expect("progress(1000)");
        assert_eq!(
            integer(&mut engine, "player.frameTickCount"),
            60,
            "1000 ms is 60 ticks"
        );
        assert_eq!(integer(&mut engine, "player.tickCount"), 1000);
        assert_eq!(integer(&mut engine, "player.playing"), 1, "180 ticks long");

        engine
            .execute_script("ms2.tjs", "player.progress(500); player.draw(layer);")
            .expect("progress(1500)");
        assert_eq!(
            integer(&mut engine, "player.frameTickCount"),
            90,
            "1500 ms is 90 ticks"
        );
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(15, 15)"),
            0x00ff_0000,
            "the tick-90 keyframe is the one on screen"
        );

        // `frameProgress` is the raw handler: its argument is already ticks.
        engine
            .execute_script(
                "raw.tjs",
                "player.play(\"idle\", 0); player.frameProgress(60);",
            )
            .expect("frameProgress");
        assert_eq!(integer(&mut engine, "player.frameTickCount"), 60);
        assert_eq!(
            integer(&mut engine, "player.tickCount"),
            1000,
            "60 raw ticks report as 1000 ms"
        );

        // The setter is milliseconds-facing too (`FUN_10045ba0`), and the
        // frame-granularity member takes raw ticks.
        engine
            .execute_script("set.tjs", "player.tickCount = 1500;")
            .expect("tickCount setter");
        assert_eq!(integer(&mut engine, "player.frameTickCount"), 90);
        engine
            .execute_script("setraw.tjs", "player.frameTickCount = 30;")
            .expect("frameTickCount setter");
        assert_eq!(integer(&mut engine, "player.tickCount"), 500);
    }

    /// The per-frame update is real: a two-frame timeline motion draws its
    /// second keyframe once the player has advanced to it.
    #[test]
    fn progress_advances_the_drawn_frame() {
        /// One layer, two keyframes: `white` at (8, 8), then `red` at (16, 16).
        fn two_frame_layer() -> Value {
            object(vec![
                ("label", text("body")),
                ("coordinate", int(0)),
                ("children", list(vec![])),
                (
                    "frameList",
                    list(vec![
                        object(vec![
                            ("content", content("src/hero/white", [8, 8], 255)),
                            ("time", int(0)),
                            ("type", int(2)),
                        ]),
                        object(vec![
                            ("content", content("src/hero/red", [16, 16], 255)),
                            ("time", int(30)),
                            ("type", int(3)),
                        ]),
                        object(vec![("time", int(60)), ("type", int(0))]),
                    ]),
                ),
            ])
        }

        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255]), ("red", [255, 0, 0, 255])],
                two_frame_layer(),
                -1,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script("first.tjs", "player.play(\"idle\", 0); player.draw(layer);")
            .expect("first frame");
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(7, 7)"),
            0x00ff_ffff,
            "the first keyframe draws"
        );

        engine
            .execute_script(
                "second.tjs",
                "layer.fillRect(0, 0, 32, 32, 0x00000000); \
                 player.progress(750); player.draw(layer);",
            )
            .expect("second frame");
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(15, 15)"),
            0x00ff_0000,
            "the second keyframe draws after progress"
        );
        assert_eq!(
            integer(&mut engine, "player.frameTickCount"),
            45,
            "750 ms of the 30-tick keyframe gap is 45 ticks"
        );
        assert_eq!(integer(&mut engine, "player.tickCount"), 750);
    }

    /// A looping motion keeps playing and wraps to the motion's `loopTime`
    /// (+0x150), not to tick 0: the fixture loops back at tick 30 of its
    /// 60-tick span, so a 1250 ms step (75 ticks) re-enters at tick 45 — the
    /// old `rem_euclid(duration)` wrap would have answered 15.
    #[test]
    fn a_looping_motion_wraps_to_loop_time_and_keeps_playing() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                30,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script(
                "loop.tjs",
                "player.play(\"idle\", 0); player.progress(1250);",
            )
            .expect("loop");
        assert_eq!(integer(&mut engine, "player.playing"), 1);
        assert_eq!(
            integer(&mut engine, "player.frameTickCount"),
            45,
            "75 ticks wrap to loopTime 30 + (75-60)%30"
        );
        assert_eq!(integer(&mut engine, "player.tickCount"), 750);
        assert_eq!(
            integer(&mut engine, "player.loopTime"),
            500,
            "loopTime is milliseconds-facing"
        );
        assert_eq!(integer(&mut engine, "player.frameLoopTime"), 30);

        // A second wrap lands from wherever the step re-entered: 45 + 30 ticks
        // is 75 again, so the same loop point answers 45.
        engine
            .execute_script("again.tjs", "player.progress(500);")
            .expect("again");
        assert_eq!(integer(&mut engine, "player.frameTickCount"), 45);
    }

    /// Variable setters change the drawn output: the layer's `x` parameter
    /// picks the frame, and a timed write eases into it as the player advances.
    #[test]
    fn variable_setters_change_the_drawn_frame() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255]), ("red", [255, 0, 0, 255])],
                parameterised_layer(),
                0,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script("play.tjs", "player.play(\"idle\", 0);")
            .expect("play");

        engine
            .execute_script(
                "var0.tjs",
                "player.setVariable(\"x\", 0); player.draw(layer);",
            )
            .expect("x=0");
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(7, 7)"),
            0x00ff_ffff,
            "x=0 draws the first frame"
        );
        assert_eq!(integer(&mut engine, "layer.getMainPixel(15, 15)"), 0);

        engine
            .execute_script(
                "var1.tjs",
                "player.setVariable(\"x\", 1); player.draw(layer);",
            )
            .expect("x=1");
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(15, 15)"),
            0x00ff_0000,
            "x=1 draws the second frame"
        );
        assert_eq!(integer(&mut engine, "player.getVariable(\"x\")"), 1);

        // A timed write eases over the ticks the player advances through; the
        // `time` argument is already authored in ticks by the game
        // (`system_AffineSourceMotion.tjs` converts its milliseconds with
        // `* 60 / 1000`), while `progress` takes milliseconds.
        engine
            .execute_script(
                "timed.tjs",
                "player.play(\"idle\", 0); player.setVariable(\"x\", 0); \
                 player.setVariable(\"x\", 1, 30, 0); player.progress(250);",
            )
            .expect("timed");
        let halfway = real(&mut engine, "player.getVariable(\"x\")");
        assert!(
            (halfway - 0.5).abs() < 0.01,
            "the timed write is halfway after 15 of 30 ticks: {halfway}"
        );
        engine
            .execute_script("finish.tjs", "player.progress(250);")
            .expect("finish");
        assert_eq!(real(&mut engine, "player.getVariable(\"x\")"), 1.0);
    }

    /// `variableKeys` answers the TJS array `AffineSourceMotion._getOptions`
    /// iterates (`_player.variableKeys.count`, indexing, then
    /// `_player.getVariable(key)` per entry — object 59 bytecode 225/234/245).
    /// The stub this replaces was a native *function*, so the game's `.count`
    /// read threw `Member "count" does not exist` and killed PARQUET at
    /// custom.ks:105.
    #[test]
    fn variable_keys_lists_the_players_variables() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script("play.tjs", "player.play(\"idle\", 0);")
            .expect("play");

        assert_eq!(
            integer(&mut engine, "player.variableKeys.count"),
            0,
            "no variables yet"
        );
        engine
            .execute_script(
                "vars.tjs",
                "player.setVariable(\"face_eye_open\", 0.5); \
                 player.setVariable(\"face_mouth\", 0.25);",
            )
            .expect("setVariable");

        // The iteration `_getOptions` runs, spelled the way its bytecode does.
        engine
            .execute_script(
                "options.tjs",
                r#"
                global.keys = player.variableKeys;
                global.variables = new Dictionary();
                for (var i = 0; i < keys.count; i++) {
                    variables[keys[i]] = player.getVariable(keys[i]);
                }
                "#,
            )
            .expect("_getOptions-style iteration");

        assert_eq!(integer(&mut engine, "keys.count"), 2);
        assert_eq!(
            integer(
                &mut engine,
                "keys[0] == \"face_eye_open\" && keys[1] == \"face_mouth\""
            ),
            1,
            "the names the player holds, in the map's order"
        );
        assert_eq!(
            real(&mut engine, "variables[\"face_eye_open\"]"),
            0.5,
            "every key reads back through getVariable"
        );
        assert_eq!(real(&mut engine, "variables[\"face_mouth\"]"), 0.25);
        assert_eq!(
            integer(
                &mut engine,
                "typeof player.variableKeys == \"Object\" && typeof player.variableKeys != \"Function\""
            ),
            1,
            "a value-shaped member, not the old method stub"
        );
    }

    /// The reference's failure shapes: an unreadable file yields a null handle
    /// (the game's wrapper raises its own exception around it), an unknown
    /// motion leaves the player stopped, and a non-layer draw fails.
    #[test]
    fn missing_resources_fail_the_way_the_reference_fails() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        engine
            .execute_script(
                "missing.tjs",
                "global.rm = new Motion.ResourceManager(0, 0); \
                 global.missing = rm.load(\"motion/missing.mtn\");",
            )
            .expect("load a missing file");
        assert!(
            engine
                .execute_expression("missing.tjs", "missing === null")
                .expect("read")
                .is_truthy(),
            "a missing file yields null, the reference's failed-load shape"
        );
        let logs = engine.host().logs().join("\n");
        assert!(
            logs.contains("cannot load \"motion/missing.mtn\""),
            "the reason is logged: {logs}"
        );

        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script("play.tjs", "player.play(\"nope\", 0);")
            .expect("unknown motion");
        assert_eq!(integer(&mut engine, "player.playing"), 0);
        assert!(
            engine
                .host()
                .logs()
                .join("\n")
                .contains("no loaded motion has that animation"),
            "the unknown animation is diagnosed"
        );

        let error = engine
            .execute_expression("draw.tjs", "player.draw(0)")
            .expect_err("drawing into a non-layer");
        assert!(
            error.to_string().contains("layer"),
            "the draw target is diagnosed: {error}"
        );
    }

    /// The `opa` byte is alpha on the file's 0..255 scale: half transparent art
    /// blends halfway between the layer's background and its own colour.
    #[test]
    fn opa_is_an_eight_bit_alpha() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 128),
                -1,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script(
                "draw.tjs",
                "layer.fillRect(0, 0, 32, 32, 0xff000000); \
                 player.play(\"idle\", 0); player.draw(layer);",
            )
            .expect("draw");
        let half = integer(&mut engine, "layer.getMainPixel(7, 7)");
        let channels = [(half >> 16) & 0xff, (half >> 8) & 0xff, half & 0xff];
        assert_eq!(channels[0], channels[1], "channels blend alike: {half:#x}");
        assert_eq!(channels[1], channels[2], "channels blend alike: {half:#x}");
        assert!(
            (0x7d..=0x83).contains(&channels[0]),
            "opa 128 over black is ~50% white, got {half:#x}"
        );

        let mut opaque = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        opaque.execute_script("setup.tjs", SETUP).expect("setup");
        opaque
            .execute_script(
                "draw.tjs",
                "layer.fillRect(0, 0, 32, 32, 0xff000000); \
                 player.play(\"idle\", 0); player.draw(layer);",
            )
            .expect("draw");
        assert_eq!(
            integer(&mut opaque, "layer.getMainPixel(7, 7)"),
            0x00ff_ffff,
            "opa 255 is fully opaque"
        );
    }

    /// `SeparateLayerAdaptor.getSubImageLayers()` must answer `void`: the game
    /// branches on `=== void` to pick its single-canvas path.
    #[test]
    fn separate_layer_adaptor_reports_no_sub_layers() {
        let mut engine = engine_with(&[]);
        engine
            .execute_script(
                "adaptor.tjs",
                "global.adaptor = new Motion.SeparateLayerAdaptor(0); \
                 global.sub = adaptor.getSubImageLayers();",
            )
            .expect("adaptor");
        assert!(
            engine
                .execute_expression("adaptor.tjs", "sub === void")
                .expect("read")
                .is_truthy(),
            "void selects the game's single-canvas path"
        );
    }

    /// The adaptor is a real drawable canvas: construct it with the owner
    /// layer the way the game does (`new Motion.SeparateLayerAdaptor(owner
    /// incontextof global.Layer)`), clear and draw into it through
    /// `Motion.Player`, then publish it with `Layer.assignImages`.
    ///
    /// This is the game's `drawAffine` sequence (`system/AffineSourceMotion.tjs`
    /// object 65: `_player.clear(a0, neutralColor)` → `_drawAffine` →
    /// `a0 instanceof "Layer"` → `l2.assignImages(a0)`).  The old plain-object
    /// stub answered "Not drawable layer type" on the clear and killed PARQUET
    /// at ~frame 15060 of the ev scene.
    #[test]
    fn separate_layer_adaptor_is_a_drawable_canvas_the_owner_can_publish() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        engine
            .execute_script(
                "adaptor.tjs",
                r#"
                global.owner = new Layer();
                owner.setPos(0, 0);
                owner.setSize(48, 48);
                global.adaptor = new Motion.SeparateLayerAdaptor(owner incontextof global.Layer);
                global.rm = new Motion.ResourceManager(0, 0);
                global.res = rm.load("motion/hero.mtn");
                global.player = new Motion.Player(rm);
                "#,
            )
            .expect("adaptor");

        // The identity and shape the game relies on: a `Layer` with its own
        // bitmap, sized like the owner (not the 32x32 holder).
        assert_eq!(integer(&mut engine, "adaptor instanceof \"Layer\""), 1);
        assert_eq!(integer(&mut engine, "adaptor.width"), 48);
        assert_eq!(integer(&mut engine, "adaptor.height"), 48);
        assert_eq!(integer(&mut engine, "adaptor.imageWidth"), 48);
        assert_eq!(integer(&mut engine, "adaptor.imageHeight"), 48);

        // The placement contract: a visible child of its owner — the shape the
        // reference's adaptor has (`FUN_1000d280` parents each host `Layer` to
        // `targetLayer`) and the only one that keeps drawing under the
        // `ltBinder` owner this game produces (see the module docs). The hit
        // threshold is the reference's `0x100`: the canvas must not swallow
        // mouse hits on the art it draws.
        assert_eq!(integer(&mut engine, "adaptor.parent === owner"), 1);
        assert_eq!(integer(&mut engine, "adaptor.visible"), 1);
        assert_eq!(integer(&mut engine, "adaptor.hitThreshold"), 0x100);
        assert_eq!(integer(&mut engine, "owner.children.count"), 1);
        assert_eq!(integer(&mut engine, "owner.children[0] === adaptor"), 1);

        // `_player.clear(adaptor, neutralColor)` lands pixels.
        engine
            .execute_script("clear.tjs", "player.clear(adaptor, 0xff3366cc);")
            .expect("clear");
        assert_eq!(integer(&mut engine, "adaptor.getMainPixel(4, 4)"), 0x3366cc);

        // `_player.draw(adaptor)` composites the motion into the same canvas:
        // the 4x4 white icon at coord (8, 8) covers pixels 6..10.
        engine
            .execute_script(
                "draw.tjs",
                "player.play(\"idle\", Motion.PlayFlagForce); player.draw(adaptor);",
            )
            .expect("draw");
        assert_eq!(
            integer(&mut engine, "adaptor.getMainPixel(7, 7)"),
            0x00ff_ffff
        );

        // The publish branch: the owner copies the canvas's image with
        // `Layer.assignImages`, and the engine's own accessor reads the copy.
        engine
            .execute_script("publish.tjs", "owner.assignImages(adaptor);")
            .expect("publish");
        assert_eq!(
            integer(&mut engine, "owner.getMainPixel(7, 7)"),
            0x00ff_ffff,
            "the drawn motion reaches the owner"
        );
        assert_eq!(
            integer(&mut engine, "owner.getMainPixel(4, 4)"),
            0x3366cc,
            "the cleared background travels with it"
        );
    }

    /// The adaptor must reach the screen under exactly the owner shape the game
    /// produces: `entryOwner` rewrites an `ltAlpha` owner to `ltBinder` right
    /// after constructing the adaptor (`system/AffineSourceMotion.tjs` object
    /// 25 bytecode 79-96), and `drawAffine` restores that type at the end of
    /// every frame — so the owner itself never carries an image and the canvas
    /// is the only drawable in the subtree.  This drives the frame output with
    /// that shape and checks the canvas is composited at the owner's origin
    /// (a root draw would sit at (0, 0) instead) with its pixels uploaded.
    #[test]
    fn separate_layer_adaptor_draws_through_a_binder_owner() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                -1,
            ),
        )]);
        engine
            .execute_script(
                "adaptor.tjs",
                r#"
                global.owner = new Layer();
                owner.setPos(5, 5);
                owner.setSize(48, 48);
                owner.visible = true;
                global.adaptor = new Motion.SeparateLayerAdaptor(owner incontextof global.Layer);
                owner.type = 0;                      // ltBinder, as entryOwner does
                global.rm = new Motion.ResourceManager(0, 0);
                global.res = rm.load("motion/hero.mtn");
                global.player = new Motion.Player(rm);
                player.play("idle", Motion.PlayFlagForce);
                player.clear(adaptor, 0x00000000);
                player.draw(adaptor);
                "#,
            )
            .expect("draw into the adaptor");

        assert_eq!(
            integer(&mut engine, "owner.type"),
            0,
            "the game's binder owner"
        );
        assert_eq!(
            integer(&mut engine, "owner.hasImage"),
            0,
            "a binder frees the owner's own image"
        );
        assert_eq!(
            integer(&mut engine, "adaptor.getMainPixel(7, 7)"),
            0x00ff_ffff,
            "the motion landed in the canvas"
        );

        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(96.0, 96.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("frame");
        let image = frame
            .output
            .draw_commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Image(image) => Some(image),
                _ => None,
            })
            .expect("the canvas is in the frame's draw commands");
        assert_eq!(
            (
                image.rect.x,
                image.rect.y,
                image.rect.width,
                image.rect.height
            ),
            (5.0, 5.0, 48.0, 48.0),
            "the canvas is positioned by its owner, not as a root"
        );
        assert!(
            frame.output.image_uploads.iter().any(|upload| {
                upload.width == 48
                    && upload.rgba.get((7 * 48 + 7) * 4..(7 * 48 + 7) * 4 + 4)
                        == Some(&[255, 255, 255, 255][..])
            }),
            "the canvas's pixels are uploaded for the renderer"
        );
    }

    /// The game constructs the adaptor before its owner has its final rect
    /// (`system/AffineLayer.tjs` runs `entryOwner` before its `onResize`), so
    /// the canvas re-fits to the owner on the next write instead of staying at
    /// the 32x32 `Layer` default — which would clip a full-screen motion.
    #[test]
    fn separate_layer_adaptor_follows_a_later_owner_resize() {
        let mut engine = engine_with(&[]);
        engine
            .execute_script(
                "setup.tjs",
                "global.owner = new Layer(); \
                 global.adaptor = new Motion.SeparateLayerAdaptor(owner); \
                 global.p = new Motion.Player(0);",
            )
            .expect("setup");
        assert_eq!(
            integer(&mut engine, "adaptor.imageWidth"),
            32,
            "the owner is still at the default size"
        );

        engine
            .execute_script("resize.tjs", "owner.setPos(0, 0); owner.setSize(64, 64);")
            .expect("resize");
        engine
            .execute_script("clear.tjs", "p.clear(adaptor, 0xff112233);")
            .expect("clear");
        assert_eq!(integer(&mut engine, "adaptor.width"), 64);
        assert_eq!(integer(&mut engine, "adaptor.imageHeight"), 64);
        assert_eq!(
            integer(&mut engine, "adaptor.getMainPixel(60, 60)"),
            0x112233,
            "the pixel outside the old 32x32 canvas is addressable"
        );
    }

    /// The unimplemented members are present and diagnosed, not silent.
    #[test]
    fn unsupported_members_warn_once() {
        let mut engine = engine_with(&[]);
        engine
            .execute_script(
                "setup.tjs",
                "global.p = new Motion.Player(0); p.initPhysics(0);",
            )
            .expect("call");
        engine
            .execute_script("again.tjs", "p.initPhysics(0);")
            .expect("call again");
        let warnings = engine
            .host()
            .logs()
            .iter()
            .filter(|line| line.contains("Player.initPhysics is not implemented"))
            .count();
        assert_eq!(warnings, 1, "the first call warns, later ones only count");

        // The verb-shaped stubs are methods a script can call, and the
        // value-shaped camera members are readable, so neither shape dies on
        // "not callable" / "does not exist".
        engine
            .execute_script(
                "shapes.tjs",
                "p.setZoom(1); p.startWind(0, 1, 1, 0, 1); p.getLayerNames(); \
                 p.meshDivisionRatio = 1; \
                 p.zoomX = 3.5; global.zoom = p.zoomX; \
                 p.preview = 7; global.preview = p.preview; \
                 p.cameraTarget = 0;",
            )
            .expect("the verb-shaped stubs are callable");
        // The value-shaped members keep what a script writes, exactly as the
        // reference's plain instance data does.
        assert_eq!(real(&mut engine, "zoom"), 3.5);
        assert_eq!(integer(&mut engine, "preview"), 7);
    }

    /// `setDrawAffineTranslateMatrix` takes the game's argument order
    /// `(m11, m21, m12, m22, tx, ty)` — column order for the off-diagonal pair
    /// (`AffineSourceMotion.tjs:3143-3151` and the live branch at `:3172`,
    /// pinned by the `revmtx` inverse one line later). A transposed read
    /// applies the inverse rotation, which every diagonal-only matrix (scale,
    /// translate, mirror — all the other tests use) hides.
    #[test]
    fn affine_matrix_rotates_by_the_games_argument_order() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [12, 12], 255),
                -1,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script(
                "affine.tjs",
                // 90° (`cos 0, sin 1`) about the origin, then translate by
                // (32, 0), spelled the way the game spells it.
                "player.play(\"idle\", 0); \
                 player.setDrawAffineTranslateMatrix(0, 1, -1, 0, 32, 0); \
                 player.draw(layer);",
            )
            .expect("draw through the affine");
        // The sprite covers (10..14, 10..14); +90° about the origin maps that
        // to (18..22, 10..14) and the translation slides it there. The
        // transposed read would send y negative and draw nothing at all.
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(20, 12)"),
            0x00ff_ffff,
            "the rotation lands where the game's argument order puts it"
        );
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(12, 12)"),
            0,
            "the un-rotated position is empty"
        );
    }

    /// One layer whose 4x4 icon sits at `first` from tick 0 and at `second`
    /// from tick 30 on: the animation the script-image loader keeps drawing.
    fn moving_layer(first: [i64; 2], second: [i64; 2]) -> Value {
        object(vec![
            ("label", text("body")),
            ("coordinate", int(0)),
            ("children", list(vec![])),
            (
                "frameList",
                list(vec![
                    object(vec![
                        ("content", content("src/hero/white", first, 255)),
                        ("time", int(0)),
                        ("type", int(2)),
                    ]),
                    object(vec![
                        ("content", content("src/hero/white", second, 255)),
                        ("time", int(30)),
                        ("type", int(2)),
                    ]),
                    object(vec![("time", int(600)), ("type", int(0))]),
                ]),
            ),
        ])
    }

    /// How many white pixels the 4x4 icon covers around `(cx, cy)` on the
    /// layer, sampled one pixel at a time.
    fn white_near(engine: &mut KrkrEngine, cx: i64, cy: i64) -> usize {
        let mut count = 0;
        for y in (cy - 4)..=(cy + 4) {
            for x in (cx - 4)..=(cx + 4) {
                if integer(engine, &format!("layer.getMainPixel({x}, {y})")) == 0x00ff_ffff {
                    count += 1;
                }
            }
        }
        count
    }

    /// The `.mtn` graphic loader: `Layer.loadImages` of a motion resolves
    /// through the plugin, into a bitmap the file's own `screenSize` sizes, and
    /// the engine keeps it drawing frame by frame — the seam PARQUET's title
    /// layer needs (`custom.ks` `*title_start`'s motion branch loads
    /// `title_bg.mtn` as an image; without the loader that load is "The image
    /// format could not be determined").
    #[test]
    fn a_mtn_script_image_load_is_a_live_motion_frame() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes_on_screen(
                vec![("white", [255, 255, 255, 255])],
                moving_layer([0, 0], [40, 20]),
                600,
                -1,
                [100, 80, 0, 0],
            ),
        )]);

        engine
            .execute_script(
                "load.tjs",
                r#"
                global.layer = new Layer(0, 0, 100, 80);
                layer.loadImages("motion/hero.mtn");
                "#,
            )
            .expect("the motion loads as a script image");

        // The bitmap is the motion's authored screen, not the decoder's
        // "unknown format" failure.
        assert_eq!(integer(&mut engine, "layer.imageWidth"), 100);
        assert_eq!(integer(&mut engine, "layer.imageHeight"), 80);
        // The model space is screen-centred, so the icon at coord (0, 0)
        // covers canvas pixels 48..52 around the canvas centre.
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(50, 40)"),
            0x00ff_ffff,
            "the motion's content lands at the canvas centre"
        );
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(0, 0)"),
            0,
            "away from the content the frame stays transparent"
        );

        // The graphic is live: the engine's frame clock advances the motion and
        // the layer follows it, without any script call.
        assert!(
            white_near(&mut engine, 50, 40) > 0,
            "the icon starts centred"
        );
        assert_eq!(
            white_near(&mut engine, 90, 60),
            0,
            "the second pose is still empty"
        );
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(1280.0, 720.0), 0.0), Vec::new()),
                Duration::from_secs(1),
            )
            .expect("frame");
        assert!(
            white_near(&mut engine, 90, 60) > 0,
            "the second pose arrives on the engine's own clock"
        );
        assert_eq!(
            white_near(&mut engine, 50, 40),
            0,
            "the icon left the centre"
        );
        assert_eq!(
            integer(&mut engine, "layer.imageWidth"),
            100,
            "a frame swap keeps the loaded size"
        );
    }

    /// Loads the fixture motion as a 100x80 script image into `global.layer`.
    const LOAD_AS_IMAGE: &str = r#"
        global.layer = new Layer(0, 0, 100, 80);
        layer.loadImages("motion/hero.mtn");
    "#;

    /// One frame of engine time (`advance`'s `delta` reaches the live graphic
    /// through `KrkrEngine::advance`).
    fn advance(engine: &mut KrkrEngine, delta: Duration) {
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(1280.0, 720.0), 0.0), Vec::new()),
                delta,
            )
            .expect("frame");
    }

    /// One layer whose 4x4 icon is drawn at tick 0 and gone from `empty_from`
    /// on: an animation whose own tail samples all-transparent.
    fn vanishing_layer(empty_from: i64, end: i64) -> Value {
        object(vec![
            ("label", text("body")),
            ("coordinate", int(0)),
            ("children", list(vec![])),
            (
                "frameList",
                list(vec![
                    object(vec![
                        ("content", content("src/hero/white", [0, 0], 255)),
                        ("time", int(0)),
                        ("type", int(2)),
                    ]),
                    object(vec![("time", int(empty_from)), ("type", int(2))]),
                    object(vec![("time", int(end)), ("type", int(0))]),
                ]),
            ),
        ])
    }

    /// An all-transparent sample means "the animation is over" for a motion
    /// that plays once — the layer keeps the frame it already shows — but a
    /// looping motion must deliver it: one empty tick inside the loop is part
    /// of the animation, and holding it would freeze the loop forever.
    ///
    /// Fails before the hold guard was scoped to one-shots: the looping half
    /// kept the white icon (`white_near` stayed > 0) instead of clearing.
    #[test]
    fn a_looping_motion_is_not_held_on_an_empty_tick() {
        let icons = || vec![("white", [255, 255, 255, 255])];

        // `loopTime = -1`: the icon drawn at tick 0 stands past the empty tail.
        let mut one_shot = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes_on_screen(icons(), vanishing_layer(20, 400), 400, -1, [100, 80, 0, 0]),
        )]);
        one_shot
            .execute_script("load.tjs", LOAD_AS_IMAGE)
            .expect("load");
        assert!(white_near(&mut one_shot, 50, 40) > 0, "drawn at load");
        advance(&mut one_shot, Duration::from_millis(420));
        assert!(
            white_near(&mut one_shot, 50, 40) > 0,
            "the one-shot holds its last drawn frame"
        );

        // `loopTime = 40`: tick 25.2 of the second loop draws nothing, and the
        // layer must follow the loop rather than freeze on the icon.
        let mut looping = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes_on_screen(icons(), vanishing_layer(20, 400), 400, 40, [100, 80, 0, 0]),
        )]);
        looping
            .execute_script("load.tjs", LOAD_AS_IMAGE)
            .expect("load");
        assert!(white_near(&mut looping, 50, 40) > 0, "drawn at load");
        advance(&mut looping, Duration::from_millis(420));
        assert_eq!(
            white_near(&mut looping, 50, 40),
            0,
            "the loop's empty tick is delivered, not held"
        );
    }

    /// The shared `.mtn` loader lives while **either** E-mote alias is linked
    /// and drops only when the last one is unlinked — in either order. The
    /// loader is one registration serving two module aliases, so an unlink must
    /// not take the claim away from the alias still linked.
    ///
    /// Driven through `Plugins.unlink` rather than the trait method, because
    /// that is the path whose ordering matters: the host runs `unregister`
    /// *before* the name leaves `linked_plugins` (`native/plugins.rs:62-70`),
    /// so the rule can only look at the other alias.
    #[test]
    fn the_mtn_loader_survives_unlinking_one_emote_alias() {
        use krkr_engine::plugin_api::graphic::graphic_loader_names;

        fn loader_registered(engine: &KrkrEngine) -> bool {
            graphic_loader_names(engine.tjs_runtime())
                .iter()
                .any(|name| name == "motionplayer.dll")
        }

        fn unlink(engine: &mut KrkrEngine, script: &str, name: &str) {
            engine
                .execute_script(script, &format!("Plugins.unlink(\"{name}\");"))
                .unwrap_or_else(|error| panic!("unlink {name}: {error}"));
        }

        // `engine_with` installs `motionplayer.dll`; add the shared alias.
        let mut emote_first = engine_with(&[]);
        emote_first
            .register_plugin(crate::EmotePlayerPlugin)
            .expect("emoteplayer");
        assert!(loader_registered(&emote_first), "the claim is registered");
        unlink(&mut emote_first, "unlink_emote.tjs", "emoteplayer.dll");
        assert!(
            loader_registered(&emote_first),
            "motionplayer.dll still owns the claim"
        );
        unlink(&mut emote_first, "unlink_motion.tjs", "motionplayer.dll");
        assert!(
            !loader_registered(&emote_first),
            "the last alias drops the claim"
        );

        // The other order: motionplayer first, emoteplayer last.
        let mut motion_first = engine_with(&[]);
        motion_first
            .register_plugin(crate::EmotePlayerPlugin)
            .expect("emoteplayer");
        unlink(&mut motion_first, "unlink_motion.tjs", "motionplayer.dll");
        assert!(
            loader_registered(&motion_first),
            "emoteplayer.dll still owns the claim"
        );
        unlink(&mut motion_first, "unlink_emote.tjs", "emoteplayer.dll");
        assert!(!loader_registered(&motion_first), "the last alias drops it");
    }
}
