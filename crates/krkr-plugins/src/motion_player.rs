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
//! flags)` → `progress(ticks)` per frame → `draw(workLayer)` → the game's KAG
//! wrapper copies the work layer onto the visible layer.
//!
//! # What is real here
//!
//! * **Loading** — `ResourceManager.load` / `Motion.load` read the storage file
//!   and parse it through [`krkr_emote::Motion`] (eluna's PSB reader plus the
//!   PARQUET-flavor adaptation). The handle the game gets back carries
//!   `.metadata`, which its wrapper requires (`!l2.metadata === void`).
//! * **The state machine** — `play`/`stop`/`progress`/`skip`/`skipToSync`,
//!   `speed`, `tickCount`, `playing`, `loopTime`/`lastTime`, and
//!   `setVariable(name, value[, time, easing])` with linear/smoothstep timed
//!   writes are driven by the adapter's own model; a motion that does not loop
//!   stops at its duration and clears `playing`.
//! * **Rendering** — `draw(layer)` samples the current tick, applies the
//!   player's `setCoord`/`setRotate`/`setScale`/`setDrawAffineTranslateMatrix`
//!   transform and its colour filter, and composites the draw list into the
//!   layer's bitmap through the engine's plugin-facing layer path
//!   ([`krkr_engine::plugin_api::layer`]). Icon resources are RL-decoded and
//!   palettes expanded by [`krkr_emote`].
//! * **The separate-layer canvas** — `new Motion.SeparateLayerAdaptor(owner)`
//!   answers a real drawable `Layer` (the engine's plugin canvas seam), sized
//!   like its owner layer, so the game's `drawAffine` sequence works end to
//!   end: `Motion.Player.clear(adaptor, neutralColor)` and
//!   `Motion.Player.draw(adaptor)` write its bitmap, and the owner publishes
//!   it with `Layer.assignImages(adaptor)`. `getSubImageLayers()` stays `void`,
//!   which is what makes the game take its single-canvas path.
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
use std::sync::Arc;

use krkr_emote::{Motion, MotionDrawItem, TextureCache, Tint, render_draw_list_into};
use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{create_canvas_layer, fit_canvas_layer, layer_bitmap_write},
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
            flavor adaptation + RL/palette texture decode), Player.play/progress/stop/skip/speed/tickCount and \
            timed setVariable(name, value, time, easing) drive the model's own clock, and draw(layer) composites \
            the sampled draw list into the layer bitmap through plugin_api::layer with the player's coord/rotate/\
            scale/affine transform and colour filter, and Motion.SeparateLayerAdaptor(owner) is a real drawable \
            Layer (a plugin_api canvas) sized like its owner, so the game's clear/draw/Layer.assignImages publish \
            path works. Not implemented, and honest: physics (initPhysics, wind/pend), \
            timelines, mesh deformation, particles \
            and EmotePlayer's `.psb` model playback; each such member is \
            registered and logs a one-time warning on first call instead of returning a silent success. \
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
    /// The class-level pools behind `Motion.load`/`unload`/`findMotion`.
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

/// The reference's `Motion` registration (`FUN_1005a530`, dossier §TJS surface).
const MOTION_METHODS: &[(&str, NativeArgCount)] = &[
    ("loadSource", NativeArgCount::AtLeast(1)),
    ("clearCache", NativeArgCount::Any),
    ("bufLayer", NativeArgCount::AtLeast(1)),
    ("load", NativeArgCount::AtLeast(1)),
    ("unload", NativeArgCount::AtLeast(1)),
    ("unloadAll", NativeArgCount::Any),
    ("isExistMotion", NativeArgCount::AtLeast(1)),
    ("findMotion", NativeArgCount::AtLeast(1)),
    ("findSource", NativeArgCount::AtLeast(1)),
    ("random", NativeArgCount::Any),
    ("requireLayerId", NativeArgCount::AtLeast(1)),
    ("releaseLayerId", NativeArgCount::AtLeast(1)),
];

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
                  args: Vec<Variant>| { motion_class_method(runtime, member, &args) },
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

/// `Motion.load` and friends act on the class-level pool; the game's own
/// wrapper only uses the `ResourceManager` instance methods, so these stay
/// thin.
fn motion_class_method(
    runtime: &mut Runtime<KrkrHost>,
    member: &str,
    args: &[Variant],
) -> Result<Variant> {
    match member {
        "load" => {
            let path = args
                .first()
                .map(Variant::to_tjs_string)
                .transpose()?
                .unwrap_or_default();
            Ok(load_into(runtime, &GLOBAL_POOL, &path))
        }
        "unload" => {
            let path = args
                .first()
                .map(Variant::to_tjs_string)
                .transpose()?
                .unwrap_or_default();
            unload_from(&GLOBAL_POOL, &path);
            Ok(Variant::Void)
        }
        "unloadAll" | "clearCache" => {
            GLOBAL_POOL.with(|pool| pool.borrow_mut().loaded.clear());
            Ok(Variant::Void)
        }
        "isExistMotion" | "findMotion" => {
            let name = args
                .first()
                .map(Variant::to_tjs_string)
                .transpose()?
                .unwrap_or_default();
            let exists = GLOBAL_POOL.with(|pool| {
                pool.borrow()
                    .loaded
                    .values()
                    .any(|loaded| loaded.motion.animation(&name).is_some())
            });
            Ok(Variant::Integer(i64::from(exists)))
        }
        "findSource" => {
            let name = args
                .first()
                .map(Variant::to_tjs_string)
                .transpose()?
                .unwrap_or_default();
            let exists = GLOBAL_POOL.with(|pool| {
                pool.borrow()
                    .loaded
                    .values()
                    .any(|loaded| loaded.motion.source(&name).is_some())
            });
            Ok(Variant::Integer(i64::from(exists)))
        }
        "loadSource" | "bufLayer" | "random" | "requireLayerId" | "releaseLayerId" => {
            warn_unsupported(runtime, &format!("Motion.{member}"));
            Ok(Variant::Integer(0))
        }
        other => {
            warn_unsupported(runtime, &format!("Motion.{other}"));
            Ok(Variant::Void)
        }
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

/// `MotionResourceManager` in the game's own wrapper calls these five
/// (`motion.tjs`, decompiled); the reference's `loadResource`/`unloadResource`
/// spellings stay for scripts written against the older census.
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
        NativeArgCount::Any,
        resource_manager_clear_cache,
    );
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
/// [`create_canvas_layer`] — sized like the owner, and not a plain object.
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
    runtime.register_object_native_with_arg_count(
        handle,
        "play",
        NativeArgCount::AtLeast(1),
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
    runtime.register_object_native_with_arg_count(
        handle,
        "contains",
        NativeArgCount::AtLeast(1),
        player_contains,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "draw",
        NativeArgCount::AtLeast(1),
        player_draw,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "clear",
        NativeArgCount::AtLeast(1),
        player_clear,
    );
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
        "outline",
        "tags",
        "onAction",
        "onSync",
        "onFindMotion",
        "animating",
        "play",
        "stop",
        "progress",
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

fn numeric_player_member(state: &PlayerState, name: &str) -> Option<f64> {
    match name {
        "speed" => Some(state.speed),
        "tickCount" => Some(state.tick),
        "completionType" => Some(state.completion_type as f64),
        "loopTime" => Some(state.loop_time.unwrap_or(-1.0)),
        "lastTime" => Some(
            state
                .motion
                .as_ref()
                .and_then(|motion| motion.animation(&state.animation))
                .map_or(0.0, |animation| f64::from(animation.duration_ticks)),
        ),
        "opacity" => Some(state.opacity),
        "colorWeight" => Some(state.colour_weight as f64),
        "maskMode" => Some(state.mask_mode as f64),
        "coordinate" => Some(state.coordinate),
        // The frame-granularity counters the reference exposes next to the
        // tick ones; this model has one timeline, so they mirror it.
        "frameTickCount" => Some(state.tick),
        "frameLastTime" => numeric_player_member(state, "lastTime"),
        "frameLoopTime" => numeric_player_member(state, "loopTime"),
        _ => None,
    }
}

fn set_numeric_player_member(state: &mut PlayerState, name: &str, value: f64) {
    match name {
        "speed" => state.speed = value,
        "tickCount" => state.tick = value.max(0.0),
        "completionType" => state.completion_type = value as i64,
        "loopTime" => state.loop_time = Some(value),
        "opacity" => state.opacity = value.clamp(0.0, 255.0),
        "colorWeight" => state.colour_weight = value as u32,
        "maskMode" => state.mask_mode = value as i64,
        "coordinate" => state.coordinate = value,
        _ => {}
    }
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

/// `progress(ticks)`: advances the model's clock by `ticks * speed` and ends a
/// non-looping motion at its duration.
///
/// This is the per-frame update the game's wrapper drives (`fix()` calls
/// `progress(1)`; `_drawAffine` passes the layer's accumulated interval).
fn player_progress(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let ticks = args
        .first()
        .map(Variant::to_real)
        .transpose()?
        .unwrap_or(0.0);
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

/// Advances one player; returns whether a non-looping motion just finished.
fn advance_player(state: &mut PlayerState, ticks: f64) -> bool {
    let duration = state
        .motion
        .as_ref()
        .and_then(|motion| motion.animation(&state.animation))
        .map(|animation| f64::from(animation.duration_ticks))
        .filter(|duration| *duration > 0.0);
    let loop_time = state
        .loop_time
        .or_else(|| {
            state
                .motion
                .as_ref()
                .and_then(|motion| motion.animation(&state.animation))
                .and_then(|animation| animation.loop_time)
                .map(f64::from)
        })
        .unwrap_or(-1.0);

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
            // The reference loops back to the motion's `loopTime`; PARQUET
            // writes `loopTime: 0` on every looping motion (measured over all
            // six animations of `sd101.mtn`), so wrapping at the duration
            // lands on the same tick today. A file with a mid-animation loop
            // point would need this to loop to `loop_time` instead.
            state.tick = state.tick.rem_euclid(duration);
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
    use std::sync::Arc;

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine};

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

    /// A one-icon, one-layer `idle` motion in the PARQUET source flavor.
    fn motion_bytes(icons: Vec<(&'static str, [u8; 4])>, layer: Value, loop_time: i64) -> Vec<u8> {
        let mut writer = PsbWriter::default();
        let mut icon_fields = Vec::new();
        for (name, colour) in icons {
            let pixel = writer.add_resource(block(colour));
            icon_fields.push((name, icon(&pixel)));
        }
        let root = object(vec![
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
                                    ("lastTime", int(60)),
                                    ("loopTime", int(loop_time)),
                                    ("layer", list(vec![layer])),
                                ]),
                            )]),
                        ),
                    ]),
                )]),
            ),
        ]);
        writer.finish(4, &root)
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
    /// the game calls, and the `Motion` class surface.
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
                .execute_expression("surface.tjs", &format!("typeof Motion.{name} != \"void\""))
                .expect("read")
                .is_truthy();
            assert!(found, "Motion.{name} is registered");
        }
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
            .execute_script("tick.tjs", "player.progress(10);")
            .expect("progress");
        assert_eq!(integer(&mut engine, "player.tickCount"), 10);

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

    /// `play`/`stop`/`progress` are a real state machine: a non-looping motion
    /// ends at its duration, `stop` freezes the position, `progress(0)` (the
    /// game's paused frame) does not advance, and `play` restarts.
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
        engine
            .execute_script("play.tjs", "player.play(\"idle\");")
            .expect("play");
        engine
            .execute_script("run.tjs", "player.progress(20); player.progress(0);")
            .expect("progress");
        assert_eq!(
            integer(&mut engine, "player.tickCount"),
            20,
            "progress(0) pauses"
        );
        assert_eq!(integer(&mut engine, "player.playing"), 1);

        engine
            .execute_script("speed.tjs", "player.speed = 2; player.progress(5);")
            .expect("speed");
        assert_eq!(
            integer(&mut engine, "player.tickCount"),
            30,
            "speed scales the step"
        );

        engine
            .execute_script("finish.tjs", "player.progress(60);")
            .expect("finish");
        assert_eq!(
            integer(&mut engine, "player.tickCount"),
            60,
            "clamped at the duration"
        );
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
            integer(&mut engine, "player.tickCount"),
            60,
            "stop keeps the position"
        );

        engine
            .execute_script("replay.tjs", "player.speed = 1; player.play(\"idle\");")
            .expect("replay");
        assert_eq!(integer(&mut engine, "player.tickCount"), 0, "play restarts");
        assert_eq!(integer(&mut engine, "player.playing"), 1);
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
            .execute_script("first.tjs", "player.play(\"idle\"); player.draw(layer);")
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
                 player.progress(35); player.draw(layer);",
            )
            .expect("second frame");
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(15, 15)"),
            0x00ff_0000,
            "the second keyframe draws after progress"
        );
        assert_eq!(integer(&mut engine, "player.tickCount"), 35);
    }

    /// A looping motion keeps playing and wraps at its duration.
    #[test]
    fn a_looping_motion_wraps_and_keeps_playing() {
        let mut engine = engine_with(&[(
            MOTION_STORAGE,
            motion_bytes(
                vec![("white", [255, 255, 255, 255])],
                single_frame_layer("src/hero/white", [8, 8], 255),
                0,
            ),
        )]);
        engine.execute_script("setup.tjs", SETUP).expect("setup");
        engine
            .execute_script("loop.tjs", "player.play(\"idle\"); player.progress(90);")
            .expect("loop");
        assert_eq!(integer(&mut engine, "player.playing"), 1);
        assert_eq!(
            integer(&mut engine, "player.tickCount"),
            30,
            "wrapped at 60 ticks"
        );
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
            .execute_script("play.tjs", "player.play(\"idle\");")
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

        // A timed write eases over the ticks the player advances through.
        engine
            .execute_script(
                "timed.tjs",
                "player.play(\"idle\"); player.setVariable(\"x\", 0); \
                 player.setVariable(\"x\", 1, 30, 0); player.progress(15);",
            )
            .expect("timed");
        let halfway = real(&mut engine, "player.getVariable(\"x\")");
        assert!(
            (halfway - 0.5).abs() < 0.01,
            "the timed write is halfway after 15 of 30 ticks: {halfway}"
        );
        engine
            .execute_script("finish.tjs", "player.progress(15);")
            .expect("finish");
        assert_eq!(real(&mut engine, "player.getVariable(\"x\")"), 1.0);
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
            .execute_script("play.tjs", "player.play(\"nope\");")
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
                 player.play(\"idle\"); player.draw(layer);",
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
                 player.play(\"idle\"); player.draw(layer);",
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
                "player.play(\"idle\"); \
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
}
