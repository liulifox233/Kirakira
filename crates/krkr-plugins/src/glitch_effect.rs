//! `GlitchEffect.dll` — the `Layer` glitch members and the `glitch` /
//! `fadeglitch` / `loopglitch` transition names.
//!
//! The DLL ships without source, so every claim below is read out of the
//! shipped image (`/Users/ruri/Downloads/PARQUET/plugin/GlitchEffect.dll`,
//! PE32 i386, 169 984 B, MD5 `b331a75239ee3f03eac957449cba6fd1`) and the
//! addresses in the comments are that image's virtual addresses. The dossier
//! is `docs/plugins/GlitchEffect.md`; `docs/plugins/transitions.md` §4 holds
//! the surface survey this module works from.
//!
//! # What the DLL registers
//!
//! * `V2Link` → `onV2Link` → `0x10006030` → three providers pushed through
//!   `TVPAddTransHandlerProvider` (`0x10005310` `fadeglitch`, `0x10005380`
//!   `loopglitch`, `0x100053f0` `glitch`), each a
//!   `tTVPTransHandlerProvider<TransGlitch | FadeGlitch | LoopGlitch>` over
//!   the handler classes `tTVPTransGlitchBase` / `tTVPTransGlitch` /
//!   `tTVPTransFadeGlitch` / `tTVPTransLoopGlitch` (RTTI names).
//! * One `SimpleBinder` pass (`0x10003220`, reached from `0x10006030`) binds
//!   two members onto the **global `Layer` class**: `doGlitch` (`0x10002fb0`)
//!   and `glitchCopy` (`0x100040e0`). The binder resolves the class by name
//!   and reports `Layer class not found.` (`.rdata` `0x100163e4`) when it is
//!   missing, which is what pins the binding target.
//!
//! Both members work on raw layer bitmaps: the option reader (`0x10001ab0`)
//! and the pass (`0x10004b00` → `0x10002420` tables → `0x100048f0` walk →
//! `0x10004f40` per-run copy) are pure CPU pixel code, and the DLL imports no
//! graphics API at all — the strings `hasImage`, `imageWidth`, `imageHeight`,
//! `mainImageBuffer*` and `clip*` are the layer accessors it goes through.
//!
//! # What this module implements
//!
//! **Real**: `Layer.doGlitch` and `Layer.glitchCopy`. They are attached to
//! the engine's global `Layer` class exactly where the binder puts them
//! (`crate::catalog`'s `install` registers a [`KrkrPlugin`], whose
//! `register` runs the same named-member bind), and they run the recovered
//! distortion over the engine's scoped bitmap views
//! ([`krkr_engine::plugin_api::layer`] — the memory-safe stand-in for the
//! reference's `mainImageBufferForWrite`) and repaint through
//! `Layer.update()`.
//!
//! The call shapes follow the reference's invokers: `doGlitch(sourceLayer,
//! optionsObject)` type-checks **two** objects (`0x10002fb0`) and answers
//! `TJS_E_BADPARAMCOUNT` otherwise, while `glitchCopy(sourceLayer, options?)`
//! requires its layer object and defaults every option name it reads. The
//! layer the call goes through is the destination in both, so
//! `layer.doGlitch(layer, %[…])` distorts in place and
//! `dest.glitchCopy(src, %[…])` copies through the distortion.
//!
//! **Real**: the three transition providers, and `Layer.doGlitch` /
//! `Layer.glitchCopy`. The two members are attached to the engine's global
//! `Layer` class exactly where the binder puts them (`crate::catalog`'s
//! `install` registers a [`KrkrPlugin`], whose `register` runs the same
//! named-member bind), and they run the recovered distortion over the
//! engine's scoped bitmap views ([`krkr_engine::plugin_api::layer`] — the
//! memory-safe stand-in for the reference's `mainImageBufferForWrite`) and
//! repaint through `Layer.update()`.
//!
//! The call shapes follow the reference's invokers: `doGlitch(sourceLayer,
//! optionsObject)` type-checks **two** objects (`0x10002fb0`) and answers
//! `TJS_E_BADPARAMCOUNT` otherwise, while `glitchCopy(sourceLayer, options?)`
//! requires its layer object and defaults every option name it reads. The
//! layer the call goes through is the destination in both, so
//! `layer.doGlitch(layer, %[…])` distorts in place and
//! `dest.glitchCopy(src, %[…])` copies through the distortion.
//!
//! The three transition names register from [`KrkrPlugin::register`] through
//! [`krkr_engine::plugin_api::transition`] and unregister from
//! [`KrkrPlugin::unregister`] — the DLL's `V2Link`/`V2Unlink` — so each name
//! answers exactly while the module is linked and falls back to the official
//! `Cannot find transition handler <name>` once it is not. The engine's
//! interim linked-shim projection (`PLUGIN_TRANSITION_NAMES`) is never
//! reached for them any more: the registry is consulted first.  `V2Unlink`
//! (`0x10001040`) calls `0x10006050` → `0x10003130`, three
//! `TVPRemoveTransHandlerProvider` calls, and its member-unbind helper
//! (`FUN_100012a0`) is a no-op in this build — the two `Layer` members stay
//! bound after an unlink, which is what this module's `unregister` does too.
//! (`V2Unlink` gates the removal on `TVPPluginGlobalRefCount`; the engine's
//! plugin host decides when `unregister` runs.)
//!
//! # The three providers
//!
//! The registration helpers (`0x100053f0` `glitch`, `0x10005310`
//! `fadeglitch`, `0x10005380` `loopglitch`) each build one
//! `tTVPTransHandlerProvider<T>` singleton and hand it to
//! `TVPAddTransHandlerProvider`; the template's `StartTransition` (`0x100059b0`
//! / `0x10005870` / `0x10005910`, one per template instantiation) reads the
//! provider's two type words (`*type = [0xc]`, `*updatetype = [0x10]`), tests
//! the equal-size flag at `+0x14`, and calls the class factory:
//!
//! | name | factory | handler ctor → vtable | `*type` | `*updatetype` | equal sizes |
//! |---|---|---|---|---|---|
//! | `glitch` | `0x10002e90` | `0x10002740` → `0x10016294` `tTVPTransGlitch` | 1 `ttExchange` | 0 `tutDivisibleFade` | required |
//! | `fadeglitch` | `0x10002ca0` | `0x10002340` → `0x10016310` `tTVPTransFadeGlitch` | 0 `ttSimple` | 0 `tutDivisibleFade` | not required |
//! | `loopglitch` | `0x10002df0` | `0x10002670` → `0x10016380` `tTVPTransLoopGlitch` | 0 `ttSimple` | 0 | not required |
//!
//! That class ↔ vtable ↔ hook table is not a guess: three independent reads of
//! the image agree, and each one is reproducible with `objdump` on the shipped
//! DLL (no Ghidra needed):
//!
//! 1. the constructors' immediates — `objdump -d --start-address=0x10002740
//!    --stop-address=0x100027f8` ends with `mov DWORD PTR [esi],0x10016294`
//!    (and `0x10002340` → `0x10016310`, `0x10002670` → `0x10016380`), which
//!    pins each handler class to its vtable;
//! 2. the vtable slots — `objdump -s -j .rdata --start-address=0x10016294
//!    --stop-address=0x100162bc` dumps ten dwords whose **slot 9** is the
//!    compose hook (`0x10004e10`, the shared `Process`, ends in
//!    `call [*param_1 + 0x24]`, i.e. `0x24 / 4 = 9`): `0x10016294` → `0x10004da0`,
//!    `0x10016310` → `0x100047a0`, `0x10016380` → `0x10004d70`;
//! 3. the strings laid out immediately after each vtable — a UTF-16 pool the
//!    class's own readers use: `glitch/time/gamma_in/gamma_out/block` after
//!    `0x10016294`, `fadeglitch/nofade/fadein/color` after `0x10016310`,
//!    `coef/break/loopglitch` after `0x10016380`.
//!
//! Read together they say: `glitch` is the row-masked two-face compose that
//! reads `gamma_in`/`gamma_out`, and `loopglitch` is the single-face compose
//! whose constant scale is `coef`.  (The first port of this module had those
//! two the other way round — it inferred the mapping from field offsets and
//! the lazy `coef` read instead of from these three tables.  The tests below
//! pin the consequences of the correct mapping.)
//!
//! The options each factory reads, in its own order (`FUN_10001cd0` = an
//! integer with a caller default, `FUN_10001dd0` = a real, `FUN_10001ee0` = a
//! `tTVInteger`):
//!
//! * `glitch`: `time` (`-1` default, so a missing or negative one fails the
//!   call; clamped to `>= 2`), `gamma_in` (1.0), `gamma_out` (1.0), `block`
//!   (`0x10`, `<= 0` back to `0x10`).
//! * `fadeglitch`: `time` (same rule), `nofade` (0), `fadein` (0),
//!   `gamma_in` when `fadein` is set and `gamma_out` otherwise (1.0 either
//!   way), `block` (same), `color` (0 — and forced to `0x808080` when
//!   `nofade` is set).
//! * `loopglitch`: `block` alone — no `time` at all.
//!
//! Each factory also copies the option object (`FUN_10001bc0` reads the eight
//! distortion parameters out of it) and hands the copy to the handler's base
//! constructor, which stores it (`0x10002420`) so the per-pass table fills can
//! rebuild from it.
//!
//! **Per frame** the handler's `StartProcess` (`0x100057a0`, the shared base)
//! computes `ratio = (tick - first_tick) / time` — returning 2, i.e. "stop",
//! once `tick` reaches `time` — and calls the class's own virtual hook, which
//! **reseeds the generator from the tick and rebuilds the row and block
//! tables**; `Process` (`0x10004e10`, shared) resolves the three scanline
//! providers and calls the class's compose hook. The compose hooks:
//!
//! * `glitch` (`0x10004da0`, `tTVPTransGlitch`'s slot 9): `FUN_10004830`
//!   walks the rows and picks a face per row from the mask the class's own
//!   per-pass hook (`0x10005730`) filled (`0x10005ef0`, one bit per row with
//!   probability `ratio`): a masked row takes **`Src1`** at
//!   `pow(ratio, [this+0xb8])` and an unmasked one **`Src2`** at
//!   `pow(1 - ratio, [this+0xc0])`, where the constructor `0x10002740` stored
//!   the factory's `gamma_in` and `gamma_out` — the one name in this family
//!   that mixes the two faces.
//! * `fadeglitch` (`0x100047a0`): `x = ratio` (or `1 - ratio` under
//!   `fadein`), the walk runs over `Src1` (or `Src2` under `fadein`) at
//!   `pow(x, gamma)`, and when `color != 0x808080` a colour wash
//!   ([`colour_wash`]) runs over the composed bitmap with the value
//!   `FUN_10003710` derives from `x²`.
//! * `loopglitch` (`0x10004d70`): `FUN_100048f0` over **`Src1`** — the
//!   destination's own bitmap — at the constant scale `[this+0xc0]`
//!   (1.0 from the constructor, `coef` when the option provider is present;
//!   see below).
//!
//! `ttExchange` is what swaps the two layers at the stop
//! (`LayerIntf.cpp:6371`); `tutDivisibleFade` is what makes the reference
//! re-read the live bitmaps instead of the frozen `Src1Bmp`/`Src2Bmp`
//! (`LayerIntf.cpp:6581`, `:6601`) — see the deviations below.
//!
//! ## The one option with no effect, and one with a reachability caveat
//!
//! * **`break`** (`loopglitch` only): `0x100055d0` stores its truthiness at
//!   `+0xc8`, and nothing in `tTVPTransLoopGlitch` reads `+0xc8` afterwards —
//!   its compose hook `0x10004d70` touches only `+0xc0`, and the destructor
//!   tears the tables down.  The port does not implement it: there is no
//!   effect to reproduce.
//! * **`coef`** (`loopglitch`'s scale) and the same hook's `break` read are
//!   gated on the option provider pointer at `+0xb8`, which only
//!   `tTVPTransLoopGlitch`'s `SetOption` override (`0x10005520`) writes.
//!   **In the reference sources available on this host no caller of
//!   `iTVPBaseTransHandler::SetOption` exists** — `grep -rn SetOption` over
//!   `krkrz/visual` and `krkr2`'s core finds only the declaration and
//!   `TransIntf.cpp`'s definition, and Kirikiroid2's only hits are
//!   `FreeTypeFontRasterizer` — so *if* that is true for the shipping build,
//!   `coef` never reaches `+0xc0` and the scale stays 1.0.  That is an
//!   unproven assertion about builds we cannot inspect (the extrans samples
//!   do implement `SetOption` as a no-op, which hints some build calls it),
//!   so the port reads `coef` from the options it is handed — our channel's
//!   options snapshot *is* the option provider — and the option is live
//!   here with the DLL's own default when absent.
//!
//! ## Deviations from the reference
//!
//! * **The generator stream.**  The per-pass rebuild seeds the tables from
//!   the tick exactly as the DLL does, but the port's splitmix64/Marsaglia
//!   pipeline is the module's own (see [`GlitchTables`]): runs are
//!   reproducible from the tick, not bit-identical to the DLL's.
//! * **`Src1` is the frozen bitmap here.**  The registry's channel hands a
//!   handler the destination layer's bitmap *as it was when the transition
//!   started* (`plugin_api::transition`: `dest_before`), which is the
//!   reference's `tutDivisible` spelling; these three providers report
//!   `tutDivisibleFade`, where the core re-reads the destination's live
//!   image.  For a transition whose destination does not change under it the
//!   two are the same bitmap.
//! * **`ttSimple` has no spelling in the registry.**  `fadeglitch` and
//!   `loopglitch` report `ttSimple`, so the reference does *not* exchange the
//!   two layers at the stop and *does* accept a call without a source layer;
//!   the registry's `TransitionHandlerProvider` has no way to report either,
//!   so this engine treats every provider transition like `ttExchange` and
//!   requires a source layer (a `beginTransition` without one fails before
//!   any provider runs).
//! * **`loopglitch`'s stop.**  Its handler's `time` stays -1, and
//!   `0x100057a0` then answers "stop" at the first tick past zero, so the
//!   reference's own `loopglitch` ends after its tick-0 pass.  This port
//!   leaves the clock to the engine: a `loopglitch` call without a `time`
//!   option completes immediately (the registry's no-clock rule), and one
//!   with a `time` runs for that clock.
//!
//! ## Live check
//!
//! The unit tests drive `Layer.beginTransition` through the engine, so they
//! cover the registry, the factories and the compose passes — but not the
//! game's own scene.  A run that does, end to end, on a scratch root whose
//! `savedata/` is a real copied directory (`cp -a`, never a symlink into the
//! game tree):
//!
//! 1. build this worktree's `krkr-debug` (`cargo build -p krkr-debug`);
//! 2. symlink the game's `*.xp3` and `patch.tjs` into a scratch root, copy
//!    `savedata/` with `cp -a`, make an empty `plugin/` directory;
//! 3. drive it over a FIFO: `advance N`, `click x y`, `trace add
//!    beginTransition`, `draw`, `state`;
//! 4. read the log back: the game's load-screen open logs `native call
//!    Layer.beginTransition … args=["glitch", 1, …]`, and because a registered
//!    provider composes CPU-side the frames of that transition report
//!    `transitions=0` instead of the projection's `method=crossfade`
//!    (`frame_transitions` never carries a provider transition).  A
//!    reviewer's run of the branch that fixed this recorded the game's call at
//!    frame 1709 and twelve consecutive frames (1709–1745, the transition's
//!    own span) all `transitions=0`.
//!
//! The KAG `[trans]`/`endtrans` page projection is a *different* path: it has
//! no destination/source layer pair and still answers a provider's name with
//! its crossfade composite until the engine-side routing lands.
//!
//! # Recovered parameters
//!
//! `0x10001ab0` (and its twin `0x10001bc0`) reads the eight distortion
//! parameters out of an option object by name, each with a default double:
//!
//! | name | default | where the default lives | role |
//! |---|---|---|---|
//! | `noise` | 4.0 | `.rdata` `0x10016770` | per-row displacement amplitude |
//! | `sft_x` | 16.0 | `0x10016780` | per-block x walk step scale |
//! | `sft_y` | 8.0 | `0x10016778` | per-block y walk step scale |
//! | `sft_col` | 8.0 | `0x10016778` | per-block colour walk step scale |
//! | `per_x` | 0.5 | `0x10016758` | probability an entry adds its x step |
//! | `per_y` | 0.25 | `0x10016750` | probability an entry adds its y step |
//! | `per_col` | 0.05 | `0x10016748` | probability an entry adds its colour step |
//! | `per_reset` | 0.02 | `0x10016740` | probability the running walk is reset to zero |
//!
//! `0x10004b00` reads three more option names around the same pass: `size`
//! (default 16; `cmovle` clamps `<= 0` back to 16), `seed` (absent →
//! `TVPGetTickCount()`, `.data` signature `0x1002647c`; present →
//! `AsInteger`), and `coef` (default 1.0, `0x10016760`), which scales every
//! displacement at use time.
//!
//! # Recovered algorithm
//!
//! * **Randomness** — `0x10005c50` is splitmix64: the state advances by
//!   `0x9E3779B97F4A7C15` and the two mixing multiplies are
//!   `0xBF58476D1CE4E5B9` (shift 30) and `0x94D049BB133111EB` (shift 27),
//!   finalized by `>> 31`. `0x10002420` seeds the tables from two such draws
//!   into a 128-bit state at `+0x40`; a second generator (`0x10005ba0`, a
//!   128-bit shift-register stream) then feeds the fills, and the displacements
//!   themselves come from the **Marsaglia polar normal sampler** at
//!   `0x10002980` (two uniforms into `[-1, 1)`, reject until `u1² + u2² ∈
//!   (0, 1)`, `sqrt(-2 ln s / s)` scaled by both, the second value cached in
//!   the sampler object — `0x1000298c`-`0x1000299d`).  The cache is cleared
//!   before each fill (`0x10005dfa`, `0x10005e57`), so every table gets its
//!   own sampler.  This port reproduces the sampler
//!   and the seeding over its splitmix64 stream, so runs are reproducible from
//!   `seed` but the bit stream is not bit-identical to the reference's.
//! * **Block tables** — `0x10005dc0` fills three per-block tables through
//!   `0x10002250`, one per parameter triple: `(per_x, sft_x)`, `(per_y,
//!   sft_y)`, `(per_col, sft_col)`, each gated by `per_reset`.  `0x10002250`
//!   walks the table with a **running accumulator** (`0x1000225f` initialises
//!   it once for the whole fill, `0x100022c8`/`0x100022cd` add a normal step to
//!   it while a uniform is under the probability, `0x10002314`/`0x1000231c`
//!   multiply it by `0.0` (`.rdata` `0x10016738`) when a second uniform is
//!   under `per_reset`, and `0x10002321` stores the current value): the tables
//!   are resetting random walks, not independent per-block values, so a
//!   displacement can accumulate past its `sft_*` scale.  Two draws per entry,
//!   in that order, is what this port reproduces.
//! * **Row table** — `0x10005fc0` fills one value per row as
//!   `normal * noise` (`[edi]` is `params.noise`, i.e. struct offset 0), with
//!   no probability gate and no accumulator: every row jitters on its own.
//! * **The pass** — `0x100048f0` walks the destination rows; a row's source
//!   pointer is displaced by the row table plus the block x table, the block
//!   y table moves which source row is read, and the block colour table
//!   displaces the colour samples. `0x10004f40` copies one **run** — the part
//!   of a `size` block that falls inside the clip — at a time, byte for byte,
//!   from **three** source pointers, `out[0]` from the first, `out[1]` and
//!   `out[3]` from the second, `out[2]` from the third
//!   (`0x10005045`-`0x10005057`), where the first and third are the centre
//!   pointer `∓ 4 * channelOffset` (`0x10004fb1`-`0x10004ffa`): R, G and B are
//!   read from three horizontal positions, the chromatic split `sft_col`
//!   drives.  Each run's start pointer is clamped **once**, linearly into
//!   `[begin, end - 4 * length]` (`0x10004f76`-`0x10004f8a`) — a run whose
//!   displacement would start outside the plane is pulled wholly inside it,
//!   not clamped pixel by pixel — and the run then reads a contiguous span
//!   (`begin + 4 * x + stride * y`, `0x10004f4e`-`0x10004f61`).
//! * **Byte order** — the reference indexes its BGRA buffers from byte 0; the
//!   engine's views are RGBA, so the port's channel order is R, G, B, A
//!   (`docs/plugins/plugin-facing-engine-facilities.md` §B.3.4).

use std::{
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{layer_bitmap_read_write, layer_update},
    plugin_api::transition::{
        TransitionFrame, TransitionHandler, TransitionHandlerError, TransitionHandlerProvider,
        TransitionOptions, TransitionRequest, register_transition_provider,
        unregister_transition_provider,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Layer.doGlitch / Layer.glitchCopy pixel glitch, and the glitch / fadeglitch / loopglitch transition providers",
    notes: "All three surfaces are real. The two Layer members run the recovered splitmix64-seeded block/row/colour displacement — a Marsaglia-polar normal step per entry, accumulated into a running walk that `per_reset` zeroes (defaults noise 4, sft_x 16, sft_y 8, sft_col 8, per_x 0.5, per_y 0.25, per_col 0.05, per_reset 0.02, size 16, coef 1) — over the scoped layer bitmap views and repaint; `doGlitch` takes its two object arguments as the reference invoker type-checks them. The three transition providers register from `register` (the DLL's `V2Link`) and unregister from `unregister` (`V2Unlink`) through plugin_api::transition, each mapped to its DLL class by the constructor immediates, the vtable slot-9 compose addresses and the option-name pool next to each vtable: `glitch` is the row-masked two-face mix (masked rows Src1 at pow(ratio, gamma_in), the others Src2 at pow(1 - ratio, gamma_out)) and requires equal sizes, `fadeglitch` distorts Src1 (or Src2 under `fadein`) at pow(ratio, gamma) with the `color` wash, `loopglitch` distorts Src1 alone at the constant `coef`; all three rebuild their tables every pass from the tick, as the DLL's StartProcess hooks do. `break` is stored by the DLL and never read; `coef` is loopglitch's scale (reachable here, gated on a SetOption call in the DLL — documented).",
    install: |engine| engine.register_plugin(GlitchEffectPlugin),
};

pub struct GlitchEffectPlugin;

impl KrkrPlugin for GlitchEffectPlugin {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_layer_members(runtime);
        for provider in transition_providers() {
            register_transition_provider(runtime, Arc::clone(provider))?;
        }
        Ok(())
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        for name in TRANSITION_NAMES {
            unregister_transition_provider(runtime, name);
        }
        Ok(())
    }
}

/// The canonical DLL name: what `Plugins.link` matches and the key the
/// engine's transition-name lookup reads (`PLUGIN_TRANSITION_NAMES`,
/// `crates/krkr-engine/src/native/classes.rs`).
const PLUGIN_NAME: &str = "GlitchEffect.dll";

/// The two members the `SimpleBinder` pass (`0x10003220`) binds onto the
/// `Layer` class.
const DO_GLITCH: &str = "doGlitch";
const GLITCH_COPY: &str = "glitchCopy";

/// Binds the two members where the reference binds them: on the global
/// `Layer` class object, so every layer instance reaches them through its
/// class chain. A member another module already put there is left alone, the
/// same courtesy `crate::get_about` extends to its shared attachment.
fn install_layer_members(runtime: &mut Runtime<KrkrHost>) {
    let Variant::Object(layer_class) = runtime.global_member("Layer") else {
        return;
    };
    register_unless_closure(runtime, layer_class, DO_GLITCH, layer_do_glitch);
    register_unless_closure(runtime, layer_class, GLITCH_COPY, layer_glitch_copy);
}

fn register_unless_closure(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &'static str,
    function: impl NativeFunction<KrkrHost> + 'static,
) {
    if matches!(runtime.object_member(object, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native(object, name, function);
}

// ------------------------------------------------------------------ options

/// The recovered option defaults. The doubles are the `.rdata` constants the
/// reference's named getters are handed, so they are quoted as bit-exact
/// f64s rather than rounded table values.
const DEFAULT_NOISE: f64 = 4.0; // 0x10016770
const DEFAULT_SFT_X: f64 = 16.0; // 0x10016780
const DEFAULT_SFT_Y: f64 = 8.0; // 0x10016778
const DEFAULT_SFT_COL: f64 = 8.0; // 0x10016778
const DEFAULT_PER_X: f64 = 0.5; // 0x10016758
const DEFAULT_PER_Y: f64 = 0.25; // 0x10016750
const DEFAULT_PER_COL: f64 = 0.05; // 0x10016748
const DEFAULT_PER_RESET: f64 = 0.02; // 0x10016740
const DEFAULT_SIZE: i64 = 16; // `push 0x10` before the "size" getter, 0x10004bbd
const DEFAULT_COEF: f64 = 1.0; // 0x10016760

/// One option set: the eight distortion parameters `0x10001ab0` reads plus
/// the three (`size`, `seed`, `coef`) `0x10004b00` reads around the pass.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GlitchOptions {
    noise: f64,
    sft_x: f64,
    sft_y: f64,
    sft_col: f64,
    per_x: f64,
    per_y: f64,
    per_col: f64,
    per_reset: f64,
    /// Block edge in pixels; `<= 0` falls back to the reference's 16.
    size: i64,
    seed: u64,
    coef: f64,
}

impl GlitchOptions {
    /// The defaults the reference uses when the option object omits a name.
    /// `seed` is the one default that is not a constant in the image: the
    /// reference asks `TVPGetTickCount()`, and this port takes the wall clock
    /// in milliseconds because the engine's tick counter is not part of the
    /// plugin-facing surface.
    fn defaults() -> Self {
        Self {
            noise: DEFAULT_NOISE,
            sft_x: DEFAULT_SFT_X,
            sft_y: DEFAULT_SFT_Y,
            sft_col: DEFAULT_SFT_COL,
            per_x: DEFAULT_PER_X,
            per_y: DEFAULT_PER_Y,
            per_col: DEFAULT_PER_COL,
            per_reset: DEFAULT_PER_RESET,
            size: DEFAULT_SIZE,
            seed: wall_clock_millis(),
            coef: DEFAULT_COEF,
        }
    }

    /// Reads the option object by name, keeping the default whenever the name
    /// is absent — `0x10001ab0`/`0x10004b00` read one name at a time and only
    /// overwrite on a successful `PropGet`. An absent name is `Void`, the
    /// shape the engine's own option readers test for
    /// (`object_optional_real`, `classes.rs:4333`).
    fn from_object(runtime: &Runtime<KrkrHost>, options: Option<ObjectHandle>) -> Self {
        let Some(options) = options else {
            return Self::defaults();
        };
        let mut parsed = Self::defaults();
        let real = |name: &str, slot: &mut f64| match member(runtime, options, name) {
            Variant::Void => {}
            value => {
                if let Ok(parsed) = value.to_real() {
                    *slot = parsed;
                }
            }
        };
        real("noise", &mut parsed.noise);
        real("sft_x", &mut parsed.sft_x);
        real("sft_y", &mut parsed.sft_y);
        real("sft_col", &mut parsed.sft_col);
        real("per_x", &mut parsed.per_x);
        real("per_y", &mut parsed.per_y);
        real("per_col", &mut parsed.per_col);
        real("per_reset", &mut parsed.per_reset);
        real("coef", &mut parsed.coef);
        match member(runtime, options, "size") {
            Variant::Void => {}
            value => {
                if let Ok(size) = value.to_integer() {
                    parsed.size = size;
                }
            }
        }
        match member(runtime, options, "seed") {
            Variant::Void => {}
            value => {
                if let Ok(seed) = value.to_integer() {
                    parsed.seed = seed as u64;
                }
            }
        }
        parsed
    }

    /// `size <= 0` → 16 (the reference's `cmovle`), and the engine-safety cap
    /// `build_tables` explains.
    fn block_size(&self, width: u32, height: u32) -> u32 {
        let requested = if self.size <= 0 {
            DEFAULT_SIZE as u32
        } else {
            self.size.min(u32::MAX as i64) as u32
        };
        requested.max(minimum_block_size(width, height))
    }
}

fn member(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> Variant {
    runtime.object_member(object, name)
}

fn wall_clock_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

// ------------------------------------------------------------------- effect

/// Per-block table budget: the reference sizes its tables by the block grid
/// (`cols * rows` entries, `0x10002420`), so `size = 1` on a large layer is
/// `width * height` entries per table. This port raises the block size rather
/// than allocating past this many blocks — a guard the reference does not
/// have, kept so a script cannot turn a huge layer into a huge allocation.
const MAX_BLOCKS: u64 = 1 << 22;

fn minimum_block_size(width: u32, height: u32) -> u32 {
    let blocks = u64::from(width.max(1)) * u64::from(height.max(1));
    if blocks <= MAX_BLOCKS {
        return 1;
    }
    let needed = (blocks as f64 / MAX_BLOCKS as f64).sqrt().ceil();
    needed.min(u32::MAX as f64) as u32
}

/// splitmix64 (`0x10005c50`): the state advances by `0x9E3779B97F4A7C15` and
/// the output is the two-multiply mix with shifts 30, 27 and a final 31.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform in `[0, 1)`, the range the reference's probability gates
    /// compare against (it builds the double from the top 53 bits).
    fn uniform01(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The standard-normal sampler at `0x10002980`: the Marsaglia polar method.
/// Two uniforms are drawn into `[-1, 1)` (`* 2.0 - 1.0`, `.rdata`
/// `0x10016768` is 2.0) and rejected until `s = u1² + u2²` lands in `(0, 1)`
/// (`0x10002a40`/`0x10002a50`); then `sqrt(-2 ln s / s)` is formed and
/// multiplied by both uniforms, one returned and the other kept in the
/// object's cache (`0x1000298c`-`0x1000299d`).  Every table fill starts with a
/// cleared cache (`0x10005dfa`, `0x10005e57`), which is why this port gives
/// each table its own sampler.
#[derive(Default)]
struct NormalSampler {
    cached: Option<f64>,
}

impl NormalSampler {
    fn next(&mut self, rng: &mut SplitMix64) -> f64 {
        if let Some(value) = self.cached.take() {
            return value;
        }
        loop {
            let u1 = rng.uniform01() * 2.0 - 1.0;
            let u2 = rng.uniform01() * 2.0 - 1.0;
            let s = u1 * u1 + u2 * u2;
            if s >= 1.0 || s == 0.0 {
                continue;
            }
            let factor = (-2.0 * s.ln() / s).sqrt();
            self.cached = Some(factor * u2);
            return factor * u1;
        }
    }
}

/// The tables one pass runs on: one column-x value per row (the `noise`
/// table, `0x10005fc0`/object `+0x50`), and one value per block cell for the
/// three gated tables (`0x10005dc0`/`+0x5c`, `+0x68`, `+0x74`).
struct GlitchTables {
    size: u32,
    cols: u32,
    rows: u32,
    line: Vec<f64>,
    dx: Vec<f64>,
    dy: Vec<f64>,
    colour: Vec<f64>,
}

impl GlitchTables {
    fn build(options: &GlitchOptions, width: u32, height: u32) -> Self {
        let size = options.block_size(width, height).max(1);
        let cols = width.div_ceil(size);
        let rows = height.div_ceil(size);
        let mut rng = SplitMix64::new(options.seed);

        // The row table has no probability gate and no accumulator in the
        // reference: every row is filled with `normal * noise` (`0x10005fc0`
        // reads `params.noise` and calls the sampler once per entry).
        let mut line_sampler = NormalSampler::default();
        let line = (0..height)
            .map(|_| line_sampler.next(&mut rng) * options.noise)
            .collect();

        // `0x10002250` walks the table with one accumulator that outlives the
        // individual entries: a gated entry adds a normal step to the running
        // value, `per_reset` multiplies the value by `0.0` (`.rdata`
        // `0x10016738`), and the current value is what gets stored.  The walk
        // starts at 0 for every table.
        let mut gated = |per: f64, scale: f64| {
            let mut sampler = NormalSampler::default();
            let mut accumulated = 0.0;
            (0..cols * rows)
                .map(|_| {
                    if rng.uniform01() < per {
                        accumulated += sampler.next(&mut rng) * scale;
                    }
                    if rng.uniform01() < options.per_reset {
                        accumulated = 0.0;
                    }
                    accumulated
                })
                .collect::<Vec<f64>>()
        };
        let dx = gated(options.per_x, options.sft_x);
        let dy = gated(options.per_y, options.sft_y);
        let colour = gated(options.per_col, options.sft_col);

        Self {
            size,
            cols,
            rows,
            line,
            dx,
            dy,
            colour,
        }
    }

    /// The block cell covering `(x, y)`.
    fn cell(&self, x: u32, y: u32) -> usize {
        let column = (x / self.size).min(self.cols.saturating_sub(1));
        let row = (y / self.size).min(self.rows.saturating_sub(1));
        (row * self.cols + column) as usize
    }
}

/// One destination pixel's sampled value: `(red, green, blue, alpha)`.
type Rgba = [u8; 4];

/// The pass `0x100048f0` walks and `0x10004f40` copies.
///
/// The destination is written in *runs* — the part of one `size` block that
/// falls inside the clip — and each run reads one contiguous span of the
/// source plane: `0x10004f40` computes the run's start pointer, clamps that
/// single pointer into `[begin, end - 4*length]` (`0x10004f76`-`0x10004f8a`)
/// and then copies the run byte for byte, so a displaced run that would start
/// outside the bitmap is pulled wholly inside it instead of clamping pixel by
/// pixel.  Three runs are read per destination run: the centre one for green
/// and alpha and the same span `dcol` either side for red and blue — the
/// `± 4*channelOffset` pointers the copy loop builds around its centre
/// pointer.
#[allow(clippy::too_many_arguments)]
fn apply_glitch(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    clip: (i64, i64, i64, i64),
    scale: f64,
    tables: &GlitchTables,
) {
    let (left, top, width, height) = clip;
    let right = (left + width).clamp(0, i64::from(dest_width));
    let bottom = (top + height).clamp(0, i64::from(dest_height));
    let left = left.clamp(0, i64::from(dest_width));
    let top = top.clamp(0, i64::from(dest_height));

    let source_width = i64::from(source_width);
    let source_pixels = source_width * i64::from(source_height);
    let size = i64::from(tables.size);

    for y in top..bottom {
        let line = tables.line[(y as u32).min(dest_height.saturating_sub(1)) as usize];
        let mut x = left;
        while x < right {
            let run_end = (((x / size) + 1) * size).min(right);
            let length = run_end - x;
            let cell = tables.cell(x as u32, y as u32);
            let shift_x = ((line + tables.dx[cell]) * scale) as i64;
            let shift_y = (tables.dy[cell] * scale) as i64;
            let colour = (tables.colour[cell] * scale) as i64;

            // The run's start pixel, clamped the way the reference clamps the
            // whole pointer: a run whose start falls outside the plane is
            // pulled inside so the run still fits.
            let run_start = |offset: i64| {
                let start = (y + shift_y) * source_width + (x + shift_x + offset);
                start.clamp(0, (source_pixels - length).max(0))
            };
            let centre = run_start(0);
            let red = run_start(colour);
            let blue = run_start(-colour);

            for step in 0..length {
                let green = plane_pixel(source, centre + step);
                let red = plane_pixel(source, red + step);
                let blue = plane_pixel(source, blue + step);
                let index = pixel_offset((x + step) as u32, y as u32, dest_width);
                dest[index..index + 4].copy_from_slice(&[red[0], green[1], blue[2], green[3]]);
            }
            x = run_end;
        }
    }
}

/// The pixel at one linear plane index, the reference's
/// `begin + 4 * index` pointer arithmetic over the tightly packed plane.
fn plane_pixel(source: &[u8], index: i64) -> Rgba {
    let start = index.max(0) as usize * 4;
    match source.get(start..start + 4) {
        Some(pixel) => [pixel[0], pixel[1], pixel[2], pixel[3]],
        None => [0, 0, 0, 0],
    }
}

/// Byte offset of pixel `(x, y)` in a tightly packed 4-byte plane — the
/// engine's store order (`docs/plugins/plugin-facing-engine-facilities.md`
/// §B.3.4), so the reference's byte-0-is-blue indexing becomes 2-is-blue.
fn pixel_offset(x: u32, y: u32, width: u32) -> usize {
    ((y as usize) * (width as usize) + (x as usize)) * 4
}

/// Runs one pass from `source` onto `dest` and posts the repaint.
///
/// The reference reaches the pixels through `mainImageBufferForWrite` and
/// leaves the repaint to the caller (its transition handlers repaint through
/// the transition); this engine's contract is "mutate, commit, then
/// `Layer.update()`", so the member does the update itself.
fn run_glitch(
    runtime: &mut Runtime<KrkrHost>,
    source: ObjectHandle,
    dest: ObjectHandle,
    options: &GlitchOptions,
) -> Result<()> {
    layer_bitmap_read_write(runtime, source, dest, |source_view, dest_view| {
        let tables = GlitchTables::build(options, dest_view.bitmap.width, dest_view.bitmap.height);
        apply_glitch(
            source_view.pixels,
            source_view.bitmap.width,
            source_view.bitmap.height,
            dest_view.pixels,
            dest_view.bitmap.width,
            dest_view.bitmap.height,
            dest_view.bitmap.clip,
            options.coef,
            &tables,
        );
    })?;
    layer_update(runtime, dest)
}

// ------------------------------------------------------------------ members

/// `Layer.doGlitch(sourceLayer, options)`: the distorted pixels land on the
/// layer the call went through.
///
/// The reference's invoker (`0x10002fb0`) type-checks **two** object
/// arguments — `tTJSVariant::Type()` must answer 1 for both
/// (`0x10002ff7`/`0x10003020`) — and answers `TJS_E_BADPARAMCOUNT`
/// (`0xfffffc15`, `0x100030ee`) otherwise; the first object is the layer the
/// pass reads and the second the option object `0x10004b00` reads by name.
/// Both are required here, and the layer the call went through is the
/// destination, so `layer.doGlitch(other, %[...])` copies through the
/// distortion and `layer.doGlitch(layer, %[...])` distorts in place.
fn layer_do_glitch(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(dest) = this_obj else {
        return Err(TjsError::bad_param_count());
    };
    let source = object_argument(&args, 0).ok_or_else(TjsError::bad_param_count)?;
    let options = object_argument(&args, 1).ok_or_else(TjsError::bad_param_count)?;
    let options = GlitchOptions::from_object(runtime, Some(options));
    run_glitch(runtime, source, dest, &options)?;
    Ok(Variant::Void)
}

/// `Layer.glitchCopy(sourceLayer, options)`: `0x100040e0`'s invoker rejects
/// the call outright when the layer argument is not an object
/// (`0xfffffc15` = `TJS_E_BADPARAMCOUNT`), so a missing or non-layer source
/// is the same error here.  The option object stays optional — the pass reads
/// every name it needs with a default (`0x10001ab0`), so a call with only the
/// layer runs on those defaults.
fn layer_glitch_copy(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(dest) = this_obj else {
        return Err(TjsError::bad_param_count());
    };
    let Some(source) = object_argument(&args, 0) else {
        return Err(TjsError::bad_param_count());
    };
    if !is_layer_like(runtime, source) {
        return Err(TjsError::bad_param_count());
    }
    let options = GlitchOptions::from_object(runtime, options_argument(runtime, &args[1..]));
    run_glitch(runtime, source, dest, &options)?;
    Ok(Variant::Void)
}

/// The object argument at `index`, the shape the reference's type check
/// accepts (any object variant, `tTJSVariant::Type() == 1`).
fn object_argument(args: &[Variant], index: usize) -> Option<ObjectHandle> {
    args.get(index).and_then(Variant::object_handle)
}

/// The first object argument that is not a layer: the option object the pass
/// reads by name.
fn options_argument(runtime: &Runtime<KrkrHost>, args: &[Variant]) -> Option<ObjectHandle> {
    args.iter()
        .filter_map(Variant::object_handle)
        .find(|handle| !is_layer_like(runtime, *handle))
}

/// Whether an object carries the layer member surface the reference's
/// `glitchCopy` invoker tests for. The reference tests the variant's type
/// (`tTJSVariant::Type`, `.data` `0x100261a8`); an engine `Layer` is an
/// ordinary object there too, so the member surface is what distinguishes it
/// from an option dictionary here.
fn is_layer_like(runtime: &Runtime<KrkrHost>, object: ObjectHandle) -> bool {
    !matches!(runtime.object_member(object, "hasImage"), Variant::Void)
}

// ------------------------------------------------------ transition providers

/// The three `GetName` strings — the exact spellings
/// `TVPFindTransHandlerProvider` hashes (`TransIntf.cpp:341-359`), sorted.
///
/// `V2Link`'s `onV2Link` (`0x10006030`) installs them in the order `glitch`
/// (`0x100053f0`), `fadeglitch` (`0x10005310`), `loopglitch` (`0x10005380`) —
/// one `TVPAddTransHandlerProvider` call each, on a singleton provider object
/// built once.  Order is not part of the registry's contract, so this module
/// keeps them sorted.
const TRANSITION_NAMES: [&str; 3] = ["fadeglitch", "glitch", "loopglitch"];

/// Which of the three handler classes a provider builds
/// (`tTVPTransGlitch` / `tTVPTransFadeGlitch` / `tTVPTransLoopGlitch`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GlitchTransitionKind {
    /// `glitch` (`0x10002e90` → `tTVPTransGlitch`, ctor `0x10002740`).
    Glitch,
    /// `fadeglitch` (`0x10002ca0` → `tTVPTransFadeGlitch`, ctor `0x10002340`).
    FadeGlitch,
    /// `loopglitch` (`0x10002df0` → `tTVPTransLoopGlitch`, ctor `0x10002670`).
    LoopGlitch,
}

/// The three providers, built once and handed back on every `register` — the
/// registry treats a repeat of the identical `Arc` as a no-op, which is what
/// this engine needs because it runs `register` at boot *and* at the first
/// `Plugins.link` (the reference's single `V2Link`).
fn transition_providers() -> &'static [Arc<dyn TransitionHandlerProvider>; 3] {
    static PROVIDERS: OnceLock<[Arc<dyn TransitionHandlerProvider>; 3]> = OnceLock::new();
    PROVIDERS.get_or_init(|| {
        [
            Arc::new(GlitchTransitionProvider::new(
                GlitchTransitionKind::FadeGlitch,
            )),
            Arc::new(GlitchTransitionProvider::new(GlitchTransitionKind::Glitch)),
            Arc::new(GlitchTransitionProvider::new(
                GlitchTransitionKind::LoopGlitch,
            )),
        ]
    })
}

struct GlitchTransitionProvider {
    kind: GlitchTransitionKind,
}

impl GlitchTransitionProvider {
    fn new(kind: GlitchTransitionKind) -> Self {
        Self { kind }
    }

    fn name(&self) -> &'static str {
        match self.kind {
            GlitchTransitionKind::Glitch => "glitch",
            GlitchTransitionKind::FadeGlitch => "fadeglitch",
            GlitchTransitionKind::LoopGlitch => "loopglitch",
        }
    }
}

impl TransitionHandlerProvider for GlitchTransitionProvider {
    fn name(&self) -> &str {
        self.name()
    }

    fn start_transition(
        &self,
        request: &TransitionRequest,
    ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError> {
        let settings = TransitionSettings::parse(self.kind, &request.options)?;
        // The provider's own size rule (`tTVPTransHandlerProvider<T>`'s
        // `StartTransition`, one template for the three): the registration
        // helper's byte at `+0x14` gates it, and only `glitch` sets it
        // (`0x100053f0`), so only `glitch` refuses a mismatched pair
        // (`src1w == src2w && src1h == src2h`).  `fadeglitch` and `loopglitch`
        // (both `ttSimple`) accept any source size and may even run without a
        // source layer at all.
        if self.kind == GlitchTransitionKind::Glitch
            && request.source_size != Some(request.dest_size)
        {
            return Err(TransitionHandlerError::new(
                "glitch: the transition source and destination must be the same size",
            ));
        }
        let (width, height) = request.dest_size;
        Ok(Box::new(GlitchTransitionHandler::new(
            settings, width, height,
        )))
    }
}

// ------------------------------------------------- provider options

/// The options one of the three factories reads (`0x10002e90` `glitch`,
/// `0x10002ca0` `fadeglitch`, `0x10002df0` `loopglitch`).
///
/// Every factory also copies the whole options object and hands it to the
/// handler's base constructor (`FUN_10001bc0` → `FUN_10002420`); the eight
/// distortion parameters are read from that copy once here, because the
/// per-pass table fills read the stored values, not the script object.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TransitionSettings {
    kind: GlitchTransitionKind,
    /// `time` in milliseconds: `glitch` and `fadeglitch` read it first and
    /// return `TJS_E_FAIL` when it is missing or negative (`FUN_10001cd0`
    /// answers the `-1` default), then clamp it to the reference's 2 ms floor.
    /// `loopglitch` never reads it and keeps the constructor's `-1`, which
    /// makes its first `StartProcess` past tick 0 answer "stop"
    /// (`0x100057a0` returns 2 once `elapsed >= time`) — see
    /// [`GlitchTransitionHandler`].
    time_ms: Option<i64>,
    /// `block` (`0x10` default, `<= 0` falls back to `0x10`): the square the
    /// walk copies one run at a time.
    block: i64,
    /// `fadeglitch` only: the exponent `fadein` selects — `gamma_in` when it
    /// is set, `gamma_out` otherwise (`0x10002ca0`: `pwVar4 = L"gamma_in"; if
    /// (iVar3 == 0) pwVar4 = L"gamma_out";`).
    gamma: f64,
    /// `glitch` only.  Its constructor stores the factory's `gamma_in` at
    /// `+0xb8` and `gamma_out` at `+0xc0` (`0x10002740`), and the compose hook
    /// `0x10004da0` raises `ratio` to `+0xb8` and `1 - ratio` to `+0xc0` —
    /// these are the two live exponents of the row-masked compose, not the
    /// dead lazy read the first port took them for.
    gamma_in: f64,
    gamma_out: f64,
    /// `loopglitch` only.  `0x10004d70` composes its single face at the
    /// constant `[this+0xc0]`, which `0x10002670` initialises to 1.0 and
    /// `0x100055d0` overwrites with the `coef` option when the option
    /// provider its `SetOption` override (`0x10005520`) stored at `+0xb8` is
    /// present.
    coef: f64,
    /// `fadeglitch`'s `fadein` (`0` default): non-zero swaps the distorted
    /// face to `Src2` and the clock to `1 - ratio`.
    fadein: bool,
    /// `fadeglitch`'s `color` (`0` default; forced to `0x808080` when
    /// `nofade` is set).  `0x808080` is the "no colour wash" spelling: the
    /// handler's `+0xc8` flag is exactly `color != 0x808080`.
    colour: u32,
    noise: f64,
    sft_x: f64,
    sft_y: f64,
    sft_col: f64,
    per_x: f64,
    per_y: f64,
    per_col: f64,
    per_reset: f64,
}

/// `FUN_10001dd0`/`FUN_10001cd0`: one option read by name, the caller's
/// default kept when the member is absent or `void`, and a member that cannot
/// be converted failing the call (the C++ `(tjs_int)tmp`/`(double)tmp` throws
/// `TJSConvertError` out of `StartTransition`).
fn transition_real(
    options: &TransitionOptions,
    name: &str,
    default: f64,
) -> std::result::Result<f64, TransitionHandlerError> {
    match options.value(name) {
        None | Some(Variant::Void) => Ok(default),
        Some(value) => value.to_real().map_err(|error| {
            TransitionHandlerError::new(format!("option `{name}` cannot be read: {error}"))
        }),
    }
}

fn transition_integer(
    options: &TransitionOptions,
    name: &str,
    default: i64,
) -> std::result::Result<i64, TransitionHandlerError> {
    match options.value(name) {
        None | Some(Variant::Void) => Ok(default),
        Some(value) => value.to_integer().map_err(|error| {
            TransitionHandlerError::new(format!("option `{name}` cannot be read: {error}"))
        }),
    }
}

impl TransitionSettings {
    fn parse(
        kind: GlitchTransitionKind,
        options: &TransitionOptions,
    ) -> std::result::Result<Self, TransitionHandlerError> {
        // `block`: `FUN_10001cd0(options, L"block", 0x10)` then the `cmovle`
        // fallback back to 16.
        let block = {
            let block = transition_integer(options, "block", DEFAULT_SIZE)?;
            if block < 1 { DEFAULT_SIZE } else { block }
        };
        let settings = Self {
            kind,
            time_ms: None,
            block,
            gamma: DEFAULT_COEF,
            gamma_in: DEFAULT_COEF,
            gamma_out: DEFAULT_COEF,
            coef: DEFAULT_COEF,
            fadein: false,
            colour: 0x808080,
            noise: transition_real(options, "noise", DEFAULT_NOISE)?,
            sft_x: transition_real(options, "sft_x", DEFAULT_SFT_X)?,
            sft_y: transition_real(options, "sft_y", DEFAULT_SFT_Y)?,
            sft_col: transition_real(options, "sft_col", DEFAULT_SFT_COL)?,
            per_x: transition_real(options, "per_x", DEFAULT_PER_X)?,
            per_y: transition_real(options, "per_y", DEFAULT_PER_Y)?,
            per_col: transition_real(options, "per_col", DEFAULT_PER_COL)?,
            per_reset: transition_real(options, "per_reset", DEFAULT_PER_RESET)?,
        };
        match kind {
            GlitchTransitionKind::Glitch => {
                // `iVar1 = FUN_10001cd0(param_1, L"time", 0xffffffff); if
                // (-1 < iVar1) { if (iVar1 < 2) iVar1 = 2; ... }` — an absent
                // or negative `time` leaves the factory at its failure exit.
                let time = transition_integer(options, "time", -1)?;
                if time < 0 {
                    return Err(TransitionHandlerError::new(
                        "glitch: option `time` is required",
                    ));
                }
                // Both gammas are live exponents of the compose: the factory
                // `0x10002e90` reads them into `0x10002740`'s `+0xb8` and
                // `+0xc0`, and `0x10004da0` raises `ratio` to the first and
                // `1 - ratio` to the second (the class's own option-name pool
                // next to its vtable is `glitch/time/gamma_in/gamma_out/block`).
                Ok(Self {
                    time_ms: Some(time.max(2)),
                    gamma_in: transition_real(options, "gamma_in", DEFAULT_COEF)?,
                    gamma_out: transition_real(options, "gamma_out", DEFAULT_COEF)?,
                    ..settings
                })
            }
            GlitchTransitionKind::FadeGlitch => {
                let time = transition_integer(options, "time", -1)?;
                if time < 0 {
                    return Err(TransitionHandlerError::new(
                        "fadeglitch: option `time` is required",
                    ));
                }
                let fadein = transition_integer(options, "fadein", 0)? != 0;
                let nofade = transition_integer(options, "nofade", 0)? != 0;
                let gamma = transition_real(
                    options,
                    if fadein { "gamma_in" } else { "gamma_out" },
                    DEFAULT_COEF,
                )?;
                // `FUN_10001ee0(options, L"color", 0, 0)`: a `tTVInteger` read
                // truncated to 32 bits by the C++ `(int)` cast, and the forced
                // `0x808080` when `nofade` is set.
                let colour = if nofade {
                    0x808080
                } else {
                    transition_integer(options, "color", 0)? as u32
                };
                Ok(Self {
                    time_ms: Some(time.max(2)),
                    gamma,
                    fadein,
                    colour,
                    ..settings
                })
            }
            // The factory (`0x10002df0`) reads `block` and nothing else: no
            // `time`, no gammas, no colour.  Its handler keeps the
            // constructor's `time = -1`, and its compose scale is `coef`
            // (`+0xc0`; the class's own option pool is `coef/break/loopglitch`).
            //
            // `break` is the one member of that pool this port does not read:
            // `0x100055d0` stores its truthiness at `+0xc8` and nothing in the
            // class reads `+0xc8` afterwards (`0x10004d70` composes and the
            // destructor only tears the tables down), so the option has no
            // effect to reproduce.
            GlitchTransitionKind::LoopGlitch => Ok(Self {
                coef: transition_real(options, "coef", DEFAULT_COEF)?,
                ..settings
            }),
        }
    }
}

// --------------------------------------------- the per-playback handlers

/// The per-playback handler all three providers build
/// (`tTVPTransGlitchBase`'s constructor chain).
///
/// The per-frame flow is the reference's: `StartProcess(tick)` computes the
/// ratio `elapsed / time` and calls the class hook that **rebuilds the
/// displacement tables**, seeded from the tick; `Process` then composes one
/// pass with the class's own hook.  `0x100055d0` (glitch), `0x10005540`
/// (fadeglitch) and `0x10005730` (loopglitch) all reseed `0x10005c50` twice
/// from the tick and refill the row table (`0x10005fc0`) and the three block
/// tables (`0x10005dc0`), so the distortion is fresh every frame.  A pass
/// therefore builds its tables here, exactly once.
///
/// The three compose hooks:
///
/// * `glitch` (`0x10004d70`): `FUN_100048f0(…, src1_layout, [this+0xc0], …)` —
///   the scale is `gamma_out` (the DLL's `coef` override is unreachable, see
///   [`TransitionSettings::parse`]) and the distorted face is **`Src1`**, the
///   destination's own bitmap.  `ttExchange` then swaps the two layers at the
///   stop.
/// * `fadeglitch` (`0x100047a0`): the scale is `pow(ratio, gamma)` —
///   `pow(1 - ratio, gamma)` under `fadein`, which also swaps the source to
///   `Src2`; the tables are rebuilt with the colour value for that pass, and
///   when `color != 0x808080` the colour wash ([`colour_wash`]) runs over the
///   composed bitmap.
/// * `loopglitch` (`0x10004da0`): `FUN_10004830` walks the rows and picks a
///   face per row from the per-pass random mask (`0x10005ef0`, one bit per
///   row with probability `ratio`): masked rows take `Src1` at full scale
///   (`pow(ratio, [this+0xb8])`, and `+0xb8` is the constructor's `0`), the
///   others `Src2` at `pow(1 - ratio, [this+0xc0])` (`+0xc0` is `1.0`).
struct GlitchTransitionHandler {
    settings: TransitionSettings,
    width: u32,
    height: u32,
    /// `glitch`'s row mask (`+0x80` of `tTVPTransGlitch`, filled by
    /// `0x10005ef0`), one bit per row, low bit first; rebuilt every pass.
    row_mask: Vec<u64>,
}

impl GlitchTransitionHandler {
    fn new(settings: TransitionSettings, width: u32, height: u32) -> Self {
        Self {
            settings,
            width,
            height,
            row_mask: Vec::new(),
        }
    }

    /// The parsed options as the per-pass table fill sees them: the eight
    /// distortion parameters, `block` as the block size, and the pass's own
    /// tick as the seed (`coef` is the walk's per-pass scale here, which the
    /// compose hooks pass separately).
    fn table_options(&self, tick: Duration) -> GlitchOptions {
        let settings = &self.settings;
        GlitchOptions {
            noise: settings.noise,
            sft_x: settings.sft_x,
            sft_y: settings.sft_y,
            sft_col: settings.sft_col,
            per_x: settings.per_x,
            per_y: settings.per_y,
            per_col: settings.per_col,
            per_reset: settings.per_reset,
            size: settings.block,
            seed: tick.as_millis() as u64,
            coef: 1.0,
        }
    }

    /// `0x10005ef0(this, ratio)`: one bit per row, set with probability
    /// `ratio` from the pass's generator stream.  The reference draws a
    /// 64-bit uniform per 64-row group and compares it against the ratio; the
    /// port draws one uniform per row from the same splitmix64 the tables
    /// use, which is the same distribution and the module's own generator
    /// (the bit stream is not the DLL's anywhere — see the module docs).
    fn fill_row_mask(&mut self, ratio: f64, seed: u64) {
        let rows = self.height as usize;
        self.row_mask.clear();
        self.row_mask.resize(rows.div_ceil(64), 0);
        let mut rng = SplitMix64::new(seed);
        for row in 0..rows {
            if rng.uniform01() < ratio {
                self.row_mask[row / 64] |= 1 << (row % 64);
            }
        }
    }

    fn row_masked(&self, row: u32) -> bool {
        let row = row as usize;
        self.row_mask
            .get(row / 64)
            .is_some_and(|word| word & (1 << (row % 64)) != 0)
    }
}

impl TransitionHandler for GlitchTransitionHandler {
    fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]) {
        let (width, height) = (self.width, self.height);
        let row_bytes = width as usize * 4;
        let needed = row_bytes * height as usize;
        if dest.len() < needed || frame.dest_before.pixels.len() < needed {
            return;
        }
        let tables = GlitchTables::build(&self.table_options(frame.tick), width, height);
        let progress = f64::from(frame.progress.clamp(0.0, 1.0));
        let source = frame.source.filter(|source| {
            source.pixels.len() >= needed && source.width == width && source.height == height
        });
        let dest_before = frame.dest_before;

        match self.settings.kind {
            // `0x10004da0` (`tTVPTransGlitch`'s compose): `0x10005730`, the
            // class's per-pass hook, fills the row mask (`0x10005ef0`) with
            // probability = the pass's ratio, and the compose then walks the
            // rows picking a face per row: a masked row takes `Src1` at
            // `pow(ratio, gamma_in)`, an unmasked one `Src2` at
            // `pow(1 - ratio, gamma_out)`.
            GlitchTransitionKind::Glitch => {
                let Some(source) = source else { return };
                self.fill_row_mask(progress, frame.tick.as_millis() as u64);
                let src1_scale = progress.powf(self.settings.gamma_in);
                let src2_scale = (1.0 - progress).powf(self.settings.gamma_out);
                for row in 0..height {
                    let (plane, scale) = if self.row_masked(row) {
                        (dest_before.pixels, src1_scale)
                    } else {
                        (source.pixels, src2_scale)
                    };
                    apply_glitch(
                        plane,
                        width,
                        height,
                        dest,
                        width,
                        height,
                        (0, i64::from(row), i64::from(width), 1),
                        scale,
                        &tables,
                    );
                }
            }
            GlitchTransitionKind::FadeGlitch => {
                // `x` is the reference's ratio, reversed under `fadein`; the
                // walk's scale is `pow(x, gamma)` and the colour wash uses
                // `x²`.
                let x = if self.settings.fadein {
                    1.0 - progress
                } else {
                    progress
                };
                let scale = x.powf(self.settings.gamma);
                let plane = if self.settings.fadein {
                    source
                } else {
                    Some(dest_before)
                };
                let Some(plane) = plane else { return };
                apply_glitch(
                    plane.pixels,
                    plane.width,
                    plane.height,
                    dest,
                    width,
                    height,
                    (0, 0, i64::from(width), i64::from(height)),
                    scale,
                    &tables,
                );
                if self.settings.colour != 0x808080 {
                    colour_wash(
                        &mut dest[..needed],
                        washed_colour(self.settings.colour, x * x),
                    );
                }
            }
            // `0x10004d70` (`tTVPTransLoopGlitch`'s compose): one face —
            // `Src1`, the destination's own bitmap — at the constant scale
            // `[this+0xc0]` (`coef`, 1.0 by default).  Its per-pass hook
            // `0x100055d0` only reseeds the tables and re-reads `coef`.
            GlitchTransitionKind::LoopGlitch => {
                apply_glitch(
                    dest_before.pixels,
                    width,
                    height,
                    dest,
                    width,
                    height,
                    (0, 0, i64::from(width), i64::from(height)),
                    self.settings.coef,
                    &tables,
                );
            }
        }
    }
}

/// `FUN_10003710` (`0x10003710`): the colour value a `fadeglitch` pass washes
/// toward, `round(channel * x + (1 - x) * 128)` per channel — from the neutral
/// grey 128 at `x = 0` to the requested colour at `x = 1`.  The reference's
/// `ROUND` is the x87 rounding mode (nearest, ties to even); `f64::round` is
/// nearest-away-from-zero, which differs only on an exact tie.
fn washed_colour(colour: u32, x: f64) -> u32 {
    let channel = |value: u32| {
        (f64::from(value) * x + (1.0 - x) * 128.0)
            .round()
            .clamp(0.0, 255.0) as u32
    };
    (channel(colour >> 16) << 16) | (channel((colour >> 8) & 0xff) << 8) | channel(colour & 0xff)
}

/// `FUN_100033f0` (`0x100033f0`): the `fadeglitch` colour wash, transcribed
/// from the listing instruction by instruction.
///
/// Every pixel is averaged toward `colour` with the DLL's own per-byte
/// arithmetic — one 32-bit word at a time, so the subtractions and the
/// doubling carry across byte lanes and the two per-byte `0x7f` masks
/// (`uVar3` from the colour's high bits, `uVar7` from the sum's carry bit)
/// are what keep the result inside the byte.  `colour` is the reference's
/// `0xRRGGBB` word and the reference's lanes are BGRA, while the engine's
/// store is RGBA, so each pixel is rotated into the reference's lane order,
/// washed, and rotated back.
fn colour_wash(pixels: &mut [u8], colour: u32) {
    let lanes = (((colour >> 7) & 0x10101).wrapping_add(0x7f7f7f)) ^ 0x7f7f7f;
    let colour = colour & 0x7f7f7f;
    for pixel in pixels.chunks_exact_mut(4) {
        let value = (u32::from(pixel[3]) << 24)
            | (u32::from(pixel[0]) << 16)
            | (u32::from(pixel[1]) << 8)
            | u32::from(pixel[2]);
        let carry = (((value >> 1) & 0x7f7f7f).wrapping_add(colour)) & 0x808080;
        let mask = ((carry >> 7).wrapping_add(0x7f7f7f)) ^ 0x7f7f7f;
        let sum = value.wrapping_add(colour.wrapping_sub(carry).wrapping_mul(2));
        let washed = (sum & (mask | lanes)) | (mask & lanes) | (value & 0xff00_0000);
        pixel[3] = (washed >> 24) as u8;
        pixel[0] = (washed >> 16) as u8;
        pixel[1] = (washed >> 8) as u8;
        pixel[2] = washed as u8;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use krkr_core::{FrameInput, Size};
    use krkr_engine::{
        EngineConfig, EngineInput, KrkrEngine,
        plugin_api::layer::{LayerBitmap, layer_bitmap_read},
        plugin_api::transition::{
            TransitionHandler, TransitionHandlerError, TransitionHandlerProvider,
            TransitionRequest, register_transition_provider, transition_provider_names,
        },
    };
    use krkr_tjs2::runtime::Variant;

    use super::{GlitchEffectPlugin, GlitchOptions, TRANSITION_NAMES, colour_wash, washed_colour};

    /// A layer big enough for four 8-pixel blocks per axis. The source
    /// encodes its own coordinates — red carries `x`, green carries `y`, blue
    /// carries `(31 - x) * 8` — so a sampled pixel says which source position
    /// each channel came from.
    const SOURCE_LAYER: &str = r#"
        global.source = new Layer();
        source.setImageSize(32, 32);
        source.fillRect(0, 0, 32, 32, 0xff000000);
        for (var y = 0; y < 32; y = y + 1) {
            for (var x = 0; x < 32; x = x + 1) {
                source.fillRect(x, y, 1, 1, 0xff000000 | (x << 16) | (y << 8) | ((31 - x) * 8));
            }
        }
        source.visible = true;

        global.dest = new Layer();
        dest.setImageSize(32, 32);
        dest.fillRect(0, 0, 32, 32, 0xff808080);
        dest.visible = true;
    "#;

    const WIDTH: u32 = 32;
    const HEIGHT: u32 = 32;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(GlitchEffectPlugin).expect("plugin");
        engine
    }

    fn run(engine: &mut KrkrEngine, script: &str) -> Variant {
        engine.execute_script("inline.tjs", script).expect("script")
    }

    fn layer(engine: &KrkrEngine, name: &str) -> krkr_tjs2::runtime::ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    /// Every pixel of a layer, read through the plugin-facing view.
    fn pixels(engine: &mut KrkrEngine, name: &str) -> (LayerBitmap, Vec<u8>) {
        let handle = layer(engine, name);
        layer_bitmap_read(engine.tjs_runtime_mut(), handle, |view| {
            (view.bitmap, view.pixels.to_vec())
        })
        .expect("read layer")
    }

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * width + x) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    }

    /// The source `x` the blue channel was sampled at, decoded from the
    /// `(31 - x) * 8` pattern.
    fn decoded_blue_x(pixel: [u8; 4]) -> i64 {
        31 - i64::from(pixel[2]) / 8
    }

    /// The linear plane index a pixel was read from: the reference's pointer
    /// arithmetic is linear over the plane (`begin + 4 * x + stride * y`), so
    /// a run pulled inside the bitmap can cross from one row into the next and
    /// only the linear index stays meaningful.
    fn source_index(pixel: [u8; 4], width: u32) -> i64 {
        i64::from(pixel[1]) * i64::from(width) + i64::from(pixel[0])
    }

    /// A compact summary for failure messages: full pixel dumps are unreadable
    /// and hide the actual assertion.
    fn first_difference(left: &[u8], right: &[u8], width: u32) -> String {
        for (index, (a, b)) in left.iter().zip(right.iter()).enumerate() {
            if a != b {
                let pixel = index as u32 / 4;
                let start = index - (index % 4);
                return format!(
                    "({}, {}) is {:?} not {:?}",
                    pixel % width,
                    pixel / width,
                    &left[start..start + 4],
                    &right[start..start + 4],
                );
            }
        }
        format!("lengths {} vs {}", left.len(), right.len())
    }

    /// The defaults `0x10001ab0`/`0x10004b00` hand their named getters.
    #[test]
    fn option_defaults_match_the_recovered_constants() {
        let options = GlitchOptions::defaults();
        assert_eq!(options.noise, 4.0);
        assert_eq!(options.sft_x, 16.0);
        assert_eq!(options.sft_y, 8.0);
        assert_eq!(options.sft_col, 8.0);
        assert_eq!(options.per_x, 0.5);
        assert_eq!(options.per_y, 0.25);
        assert_eq!(options.per_col, 0.05);
        assert_eq!(options.per_reset, 0.02);
        assert_eq!(options.size, 16);
        assert_eq!(options.coef, 1.0);
        assert!(
            options.seed > 0,
            "the absent-seed default is the clock, not a constant"
        );
        // `size <= 0` falls back to the reference's 16 (`cmovle`).
        let mut sized = options;
        sized.size = 0;
        assert_eq!(sized.block_size(32, 32), 16);
    }

    /// The two members are bound onto the global `Layer` class, exactly where
    /// `0x10003220` binds them: a fresh instance reaches both.
    #[test]
    fn layer_instances_carry_do_glitch_and_glitch_copy() {
        let mut engine = engine();
        run(&mut engine, SOURCE_LAYER);
        run(
            &mut engine,
            r#"
            dest.glitchCopy(source, %[per_x: 0, per_y: 0, per_col: 0, noise: 0]);
            "#,
        );
        // With every displacement rate at zero the pass is an exact copy of
        // the source, which pins the member wiring end to end.
        let (bitmap, dest) = pixels(&mut engine, "dest");
        let (_, source) = pixels(&mut engine, "source");
        assert_eq!(dest, source, "the zeroed pass copies the source verbatim");
        assert_eq!((bitmap.width, bitmap.height), (32, 32));

        // Both members are callable on an instance that never had them
        // installed on itself — the class chain is the only path.
        let value = run(
            &mut engine,
            r#"
            var probe = new Layer();
            probe.setImageSize(2, 2);
            probe.fillRect(0, 0, 2, 2, 0xff102030);
            probe.glitchCopy(probe, %[per_x: 0, per_y: 0, per_col: 0, noise: 0]);
            probe.doGlitch(probe, %[size: 2, per_x: 0, per_y: 0, per_col: 0, noise: 0]);
            return "" + (probe.doGlitch != void) + "/" + (probe.glitchCopy != void);
            "#,
        );
        assert_eq!(value.to_tjs_string().expect("string"), "1/1");
    }

    /// The option object is read by name, and a name the object omits keeps
    /// the reference's default rather than becoming zero — the distinction
    /// `object_optional_real` (`crates/krkr-engine/src/native/classes.rs:4333`)
    /// makes, and the one `0x10001ab0` makes when its `PropGet` fails.
    #[test]
    fn options_are_read_by_name_and_absent_names_keep_their_defaults() {
        let mut engine = engine();
        run(
            &mut engine,
            "global.opts = %[noise: 5, sft_x: 2, size: 4, seed: 99, coef: 0.5];",
        );
        let opts = layer(&engine, "opts");
        let parsed = GlitchOptions::from_object(engine.tjs_runtime(), Some(opts));
        assert_eq!(parsed.noise, 5.0);
        assert_eq!(parsed.sft_x, 2.0);
        assert_eq!(parsed.size, 4);
        assert_eq!(parsed.seed, 99);
        assert_eq!(parsed.coef, 0.5);
        assert_eq!(parsed.sft_y, super::DEFAULT_SFT_Y, "absent names keep 8");
        assert_eq!(parsed.per_x, super::DEFAULT_PER_X, "absent names keep 0.5");
    }

    /// The distortion is deterministic from `seed` and actually moves pixels.
    #[test]
    fn glitch_copy_displaces_blocks_deterministically_from_the_seed() {
        let mut engine = engine();
        run(&mut engine, SOURCE_LAYER);
        run(
            &mut engine,
            r#"
            dest.glitchCopy(source, %[size: 8, seed: 12345, noise: 0, sft_x: 8, sft_y: 4,
                sft_col: 0, per_x: 1.0, per_y: 1.0, per_col: 0, per_reset: 0]);
            global.first = new Layer();
            first.setImageSize(32, 32);
            first.fillRect(0, 0, 32, 32, 0xff000000);
            first.visible = true;
            first.glitchCopy(source, %[size: 8, seed: 12345, noise: 0, sft_x: 8, sft_y: 4,
                sft_col: 0, per_x: 1.0, per_y: 1.0, per_col: 0, per_reset: 0]);
            global.other = new Layer();
            other.setImageSize(32, 32);
            other.fillRect(0, 0, 32, 32, 0xff000000);
            other.visible = true;
            other.glitchCopy(source, %[size: 8, seed: 999, noise: 0, sft_x: 8, sft_y: 4,
                sft_col: 0, per_x: 1.0, per_y: 1.0, per_col: 0, per_reset: 0]);
            "#,
        );
        let (bitmap, dest) = pixels(&mut engine, "dest");
        let (_, first) = pixels(&mut engine, "first");
        let (_, other) = pixels(&mut engine, "other");
        let (source_bitmap, source) = pixels(&mut engine, "source");

        assert_eq!(dest, first, "the same seed reproduces the same pass");
        assert_ne!(
            dest,
            other,
            "a different seed moves pixels differently: {}",
            first_difference(&dest, &other, bitmap.width)
        );
        assert_ne!(
            dest,
            source,
            "the pass is not a copy: {}",
            first_difference(&dest, &source, bitmap.width)
        );

        // Every run reads one contiguous span of the source plane: the copy
        // clamps the run's start pointer once (`0x10004f76`) and then walks it
        // byte for byte, so consecutive destination pixels come from
        // consecutive source pixels — a per-pixel clamp would repeat an edge
        // pixel instead.  A block's run can cross a row, which is why the
        // expectation is a linear plane index.
        let mut run_starts = Vec::new();
        for y in 0..HEIGHT {
            for x0 in (0..WIDTH).step_by(8) {
                let start = source_index(pixel(&dest, bitmap.width, x0, y), source_bitmap.width);
                for step in 0..8 {
                    let index = source_index(
                        pixel(&dest, bitmap.width, x0 + step, y),
                        source_bitmap.width,
                    );
                    assert_eq!(
                        index,
                        start + i64::from(step),
                        "run ({x0}, {y}) is not contiguous at step {step}"
                    );
                }
                run_starts.push(start);
            }
        }
        assert!(
            run_starts.iter().any(|start| *start != run_starts[0]),
            "every run read the same span: {run_starts:?}"
        );
    }

    /// The block tables are resetting random walks, not per-entry draws: the
    /// accumulator at `0x1000225f` survives every entry (`0x100022c8`), a
    /// reset zeroes it (`0x10002314`) and the current value is what gets
    /// stored (`0x10002321`).  The row table has no such walk.
    #[test]
    fn block_tables_accumulate_a_walk_and_the_reset_rate_zeroes_it() {
        use super::{GlitchOptions, GlitchTables};

        let mut options = GlitchOptions::defaults();
        options.seed = 2024;
        options.size = 1;
        options.sft_x = 8.0;
        options.sft_y = 8.0;
        options.sft_col = 8.0;
        options.noise = 4.0;
        options.per_x = 1.0;
        options.per_y = 0.0;
        options.per_col = 0.0;
        options.per_reset = 0.0;
        let tables = GlitchTables::build(&options, 32, 32);
        assert!(
            tables
                .dx
                .iter()
                .any(|value| value.abs() > 4.0 * options.sft_x),
            "a 1024-entry walk should drift past four single steps: {:?}",
            tables
                .dx
                .iter()
                .fold(0.0f64, |max, value| max.max(value.abs()))
        );
        for (index, step) in tables
            .dx
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .enumerate()
        {
            assert!(
                step <= 6.0 * options.sft_x,
                "step {index} of the walk is {step}, past six sigmas"
            );
        }
        assert!(
            tables.dy.iter().all(|value| *value == 0.0),
            "a table whose rate is zero stays at zero"
        );
        assert!(
            tables.colour.iter().all(|value| *value == 0.0),
            "a table whose rate is zero stays at zero"
        );

        // `per_reset` 1.0 zeroes the accumulator on every entry.
        options.per_reset = 1.0;
        let reset = GlitchTables::build(&options, 32, 32);
        assert!(
            reset.dx.iter().all(|value| *value == 0.0),
            "every entry resets, so the walk never leaves zero: {:?}",
            reset
                .dx
                .iter()
                .fold(0.0f64, |max, value| max.max(value.abs()))
        );

        // The row table is one normal draw per row, ungated (the block rates
        // being zero leaves it as the only displacement).
        options.per_x = 0.0;
        options.per_reset = 0.0;
        let rows = GlitchTables::build(&options, 32, 32);
        assert!(rows.line.iter().any(|value| *value != 0.0), "rows jitter");
        assert!(
            rows.line
                .iter()
                .all(|value| value.abs() <= 6.0 * options.noise),
            "a row draw stays inside six sigmas of `noise`"
        );
    }

    /// The row table has no probability gate: with the block rates at zero,
    /// `noise` still shifts whole rows, each row by its own normal draw.
    #[test]
    fn row_noise_shifts_whole_rows() {
        let mut engine = engine();
        run(&mut engine, SOURCE_LAYER);
        run(
            &mut engine,
            r#"
            dest.glitchCopy(source, %[size: 8, seed: 7, noise: 4, sft_x: 0, sft_y: 0,
                sft_col: 0, per_x: 0, per_y: 0, per_col: 0, per_reset: 0]);
            "#,
        );
        let (bitmap, dest) = pixels(&mut engine, "dest");
        let (source_bitmap, _) = pixels(&mut engine, "source");
        let width = i64::from(source_bitmap.width);
        let last_run = width * i64::from(source_bitmap.height) - 8;

        let mut row_shifts = Vec::new();
        for y in 1..HEIGHT {
            // The first run of the row recovers the row's own shift: no clamp
            // can reach it for a normal draw of this scale.
            let base = i64::from(y) * width;
            let shift = source_index(pixel(&dest, bitmap.width, 0, y), source_bitmap.width) - base;
            assert!(
                shift.abs() <= 6 * 4,
                "row {y} escaped six sigmas of `noise`: {shift}"
            );
            for x0 in (0..WIDTH).step_by(8) {
                let start = source_index(pixel(&dest, bitmap.width, x0, y), source_bitmap.width);
                let expected = (base + i64::from(x0) + shift).clamp(0, last_run);
                assert_eq!(
                    start, expected,
                    "row {y} run {x0} did not take the row's shift {shift}"
                );
                for step in 0..8 {
                    let index = source_index(
                        pixel(&dest, bitmap.width, x0 + step, y),
                        source_bitmap.width,
                    );
                    assert_eq!(
                        index,
                        start + i64::from(step),
                        "row {y} run {x0} is not contiguous at step {step}"
                    );
                }
            }
            row_shifts.push(shift);
        }
        assert!(
            row_shifts.iter().any(|shift| *shift != row_shifts[0]),
            "no row moved: {row_shifts:?}"
        );
    }

    /// `sft_col` splits the channels: red and blue read runs `∓ colour` either
    /// side of the green run (`0x10004f40`'s `± 4 * channelOffset` pointers),
    /// the centre one keeps the geometric sample, each run is contiguous, and
    /// the two outer runs are clamped into the plane on their own.
    #[test]
    fn colour_split_moves_red_against_blue() {
        let mut engine = engine();
        run(&mut engine, SOURCE_LAYER);
        run(
            &mut engine,
            r#"
            dest.glitchCopy(source, %[size: 4, seed: 3, noise: 0, sft_x: 0, sft_y: 0,
                sft_col: 2, per_x: 0, per_y: 0, per_col: 1.0, per_reset: 0.5]);
            "#,
        );
        let (bitmap, dest) = pixels(&mut engine, "dest");
        let (source_bitmap, _) = pixels(&mut engine, "source");
        let width = i64::from(source_bitmap.width);
        let total = width * i64::from(source_bitmap.height);
        // The runs at the start of a row would have to clamp once the walk
        // moves them left, so the assertions stay inside this window and check
        // the walk stayed small enough for it.
        let tested_runs = [8u32, 12, 16, 20];

        let mut split_seen = false;
        for y in 0..HEIGHT {
            for x0 in tested_runs {
                let x0 = i64::from(x0);
                let value = pixel(&dest, bitmap.width, x0 as u32, y);
                // Green and alpha come from the centre run, which no rate moves
                // here, and the split has to straddle it symmetrically — the
                // two together pin the centre to the block start.
                assert_eq!(
                    i64::from(value[1]),
                    i64::from(y),
                    "the centre run left the source row at run ({x0}, {y})"
                );
                assert_eq!(
                    i64::from(value[3]),
                    255,
                    "alpha must come from the centre run at ({x0}, {y})"
                );

                let red_offset = i64::from(value[0]) - x0;
                let blue_offset = decoded_blue_x(value) - x0;
                assert!(
                    red_offset.abs() <= 8 && blue_offset.abs() <= 8,
                    "the colour walk left the tested window at ({x0}, {y}): R {red_offset}, B {blue_offset}"
                );
                assert_eq!(
                    red_offset, -blue_offset,
                    "red and blue must straddle the centre sample at ({x0}, {y}): R {red_offset}, B {blue_offset}"
                );
                if red_offset != 0 {
                    split_seen = true;
                }

                // Both outer runs are contiguous reads of their own span, with
                // the centre run between them.
                for step in 0..4u32 {
                    let step = i64::from(step);
                    let pixel = pixel(&dest, bitmap.width, (x0 + step) as u32, y);
                    assert_eq!(
                        i64::from(pixel[0]),
                        x0 + red_offset + step,
                        "the red run is not contiguous at ({}, {y})",
                        x0 + step
                    );
                    assert_eq!(
                        decoded_blue_x(pixel),
                        x0 + blue_offset + step,
                        "the blue run is not contiguous at ({}, {y})",
                        x0 + step
                    );
                    assert_eq!(
                        i64::from(pixel[1]),
                        i64::from(y),
                        "the centre run left its row at ({}, {y})",
                        x0 + step
                    );
                }
            }
        }
        assert!(split_seen, "no block took its colour displacement");
        assert!(total > 0);
    }

    /// `doGlitch` distorts the layer the call went through, requires both of
    /// the reference's object arguments (`0x10002fb0`), and `glitchCopy`
    /// without its layer argument is the same bad-argument-count error.
    #[test]
    fn do_glitch_works_in_place_and_both_members_reject_missing_arguments() {
        let mut engine = engine();
        run(&mut engine, SOURCE_LAYER);
        let (bitmap, before) = pixels(&mut engine, "source");
        run(
            &mut engine,
            r#"
            source.doGlitch(source, %[size: 8, seed: 42, noise: 2, sft_x: 6, sft_y: 6,
                sft_col: 0, per_x: 1.0, per_y: 1.0, per_col: 0, per_reset: 0]);
            "#,
        );
        let after = pixels(&mut engine, "source").1;
        assert_ne!(
            before,
            after,
            "doGlitch distorts the layer it runs on: {}",
            first_difference(&before, &after, bitmap.width)
        );

        // `0x10002fb0` type-checks two object arguments; `0x100040e0` rejects
        // a `glitchCopy` whose layer argument is missing or not an object.
        for call in [
            "dest.glitchCopy()",
            "dest.glitchCopy(1)",
            "source.doGlitch()",
            "source.doGlitch(source)",
            "source.doGlitch(1, %[size: 8])",
        ] {
            let message = run(
                &mut engine,
                &format!(
                    r#"
                    var message = "";
                    try {{ {call}; }} catch (e) {{ message = e.message; }}
                    return message;
                    "#
                ),
            );
            assert_eq!(
                message.to_tjs_string().expect("string"),
                "Invalid argument count",
                "{call} must report the reference's TJS_E_BADPARAMCOUNT"
            );
        }
    }

    /// A layer whose image the script freed refuses the write with the
    /// reference's `TVPNotDrawableLayerType`, and a committed pass posts the
    /// `Layer.update()` repaint the family contract asks for.
    #[test]
    fn freed_images_are_not_drawable_and_a_commit_repaints() {
        let mut engine = engine();
        run(&mut engine, SOURCE_LAYER);
        run(
            &mut engine,
            r#"
            dest.glitchCopy(source, %[per_x: 0, per_y: 0, per_col: 0, noise: 0]);
            "#,
        );
        let (bitmap, committed) = pixels(&mut engine, "dest");
        assert_eq!(
            engine
                .execute_expression("read.tjs", "dest.callOnPaint")
                .expect("callOnPaint"),
            Variant::Integer(1),
            "the member updates the layer it wrote"
        );
        // The commit reaches the renderer: the frame output uploads the same
        // texture id with the glitched bytes.
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("frame");
        let upload = frame
            .output
            .image_uploads
            .iter()
            .find(|upload| upload.texture_id == bitmap.generation)
            .expect("the committed image reaches the frame output");
        assert_eq!(upload.rgba.as_ref(), committed.as_slice());

        let message = run(
            &mut engine,
            r#"
            var freed = new Layer();
            freed.setImageSize(8, 8);
            freed.freeImage();
            var message = "";
            try { freed.doGlitch(source, %[size: 8]); } catch (e) { message = e.message; }
            return message;
            "#,
        );
        assert_eq!(
            message.to_tjs_string().expect("string"),
            "Not drawable layer type"
        );
    }

    /// `V2Link` registers exactly the three `GetName` names and `V2Unlink`
    /// takes them away again: while the module is linked each name answers
    /// through the registry (no kernel transition reaches the frame output),
    /// and once it is not, the name is the official unknown-name error again.
    #[test]
    fn the_plugin_registers_its_three_names_and_unlinks_them() {
        let mut engine = engine();
        let expected = TRANSITION_NAMES.map(str::to_string).to_vec();
        assert_eq!(transition_provider_names(engine.tjs_runtime()), expected);

        // The engine runs `register` twice (boot and the first
        // `Plugins.link`); the second pass hands back the same providers.
        engine
            .register_plugin(GlitchEffectPlugin)
            .expect("re-register");
        assert_eq!(transition_provider_names(engine.tjs_runtime()), expected);

        run(&mut engine, TRANSITION_PAIR);
        run(
            &mut engine,
            r#"dest.beginTransition("fadeglitch", true, source, %[time: 10]);"#,
        );
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("update");
        assert!(
            frame.output.transitions.is_empty(),
            "a registered provider composes CPU-side, not through a crossfade kernel"
        );
        run(&mut engine, "dest.stopTransition();");

        run(&mut engine, r#"Plugins.unlink("GlitchEffect.dll");"#);
        assert!(transition_provider_names(engine.tjs_runtime()).is_empty());
        let message = run(
            &mut engine,
            r#"
            var message = "";
            try { dest.beginTransition("fadeglitch", true, source, %[time: 10]); }
            catch (e) { message = e.message; }
            return message;
            "#,
        );
        assert_eq!(
            message.to_tjs_string().expect("string"),
            "Cannot find transition handler fadeglitch",
            "an unlinked provider's name is the official unknown-name error"
        );

        run(&mut engine, r#"Plugins.link("GlitchEffect.dll");"#);
        assert_eq!(transition_provider_names(engine.tjs_runtime()), expected);
        run(
            &mut engine,
            r#"dest.beginTransition("fadeglitch", true, source, %[time: 10]);"#,
        );
    }

    /// A name one of the engine's own kernel providers answers to is already
    /// registered (`TVPTransAlreadyRegistered`), which the registry refuses
    /// with the reference's text.
    #[test]
    fn the_registry_refuses_a_default_kernel_name() {
        struct WaveNamed;

        impl TransitionHandlerProvider for WaveNamed {
            fn name(&self) -> &str {
                "wave"
            }

            fn start_transition(
                &self,
                _request: &TransitionRequest,
            ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError>
            {
                unreachable!("the registry refuses this provider before any factory runs")
            }
        }

        let mut engine = engine();
        let error = register_transition_provider(engine.tjs_runtime_mut(), Arc::new(WaveNamed))
            .expect_err("a kernel name is taken");
        assert_eq!(error.message, "Transition wave already registerd");
    }

    /// The 32x32 pair the transition tests run on: the destination encodes its
    /// own position (`x` in red, `y` in green, `0x80` in blue) and the source
    /// the mirrored one (`31 - x`, `31 - y`, `0x40`), so a composed pixel
    /// names the face and the position each channel came from.
    const TRANSITION_PAIR: &str = r#"
        global.dest = new Layer();
        dest.setImageSize(32, 32);
        global.source = new Layer();
        source.setImageSize(32, 32);
        for (var y = 0; y < 32; y = y + 1) {
            for (var x = 0; x < 32; x = x + 1) {
                dest.fillRect(x, y, 1, 1, 0xff000000 | (x << 16) | (y << 8) | 0x80);
                source.fillRect(x, y, 1, 1, 0xff000000 | ((31 - x) << 16) | ((31 - y) << 8) | 0x40);
            }
        }
        dest.visible = true;
        source.visible = true;
    "#;

    /// One glitch-family transition over [`TRANSITION_PAIR`]: start `name`
    /// with `options`, advance the clock by `tick`, and answer the
    /// destination's composed pixels.  The tables are seeded from the tick, so
    /// the same call is reproducible.
    fn transition_pass(name: &str, options: &str, tick: u64) -> Vec<u8> {
        transition_pass_with(name, options, "", tick)
    }

    /// [`transition_pass`] with a script step between building the options
    /// object and the call — the way a script reaches a member the `%[...]`
    /// literal cannot spell (`break` is a TJS keyword).
    fn transition_pass_with(name: &str, options: &str, setup: &str, tick: u64) -> Vec<u8> {
        let mut engine = engine();
        run(&mut engine, TRANSITION_PAIR);
        run(&mut engine, &format!("global.opts = {options}; {setup}"));
        run(
            &mut engine,
            &format!(r#"dest.beginTransition("{name}", true, source, opts);"#),
        );
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(tick),
            )
            .expect("update");
        assert!(
            frame.output.transitions.is_empty(),
            "{name} must resolve to this module's provider, not to a kernel"
        );
        pixels(&mut engine, "dest").1
    }

    /// The walk's displacement options the option pins below start from:
    /// `noise` 0 (no per-row jitter), `per_x`/`per_y` 1.0 (every block takes a
    /// step, so the walk moves), `per_reset` 0 (it never returns to zero) and
    /// the colour split off.
    ///
    /// The `fadeglitch` pins add `color: 0x808080` — the class's neutral
    /// spelling, which keeps the wash the identity — so a pin compares the
    /// walk alone.  A `fadeglitch` call without it (the game's own
    /// `FadeGlitch` passes none) *does* wash: `color` defaults to 0, a fade
    /// toward black, which saturates these encoded planes.  `glitch` and
    /// `loopglitch` read no `color` at all.
    const WALK: &str = "noise: 0, sft_x: 16, sft_y: 8, sft_col: 0, per_x: 1.0, per_y: 1.0, per_col: 0, per_reset: 0";

    /// `time` is the clock the walk's scale is read from: the same pass at 50
    /// ms composes differently under a 100 ms clock (`ratio = 0.5`) and a
    /// 200 ms one (`ratio = 0.25`).
    #[test]
    fn the_time_option_sets_the_clock_the_scale_is_read_from() {
        let fast = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, color: 0x808080, {WALK}]"),
            50,
        );
        let slow = transition_pass(
            "fadeglitch",
            &format!("%[time: 200, color: 0x808080, {WALK}]"),
            50,
        );
        assert_ne!(
            fast,
            slow,
            "ratio 0.5 and 0.25 must displace differently: {}",
            first_difference(&fast, &slow, WIDTH)
        );
    }

    /// `block` is the edge of the square the walk copies one run at a time
    /// (`0x10004f40`), and the cell grid the displacement tables are indexed
    /// by — the same clock with 8-pixel and 16-pixel blocks composes
    /// differently.
    #[test]
    fn the_block_option_sets_the_run_and_cell_grid() {
        let small = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, block: 8, color: 0x808080, {WALK}]"),
            50,
        );
        let large = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, block: 16, color: 0x808080, {WALK}]"),
            50,
        );
        assert_ne!(
            small,
            large,
            "8- and 16-pixel blocks must partition the rows differently: {}",
            first_difference(&small, &large, WIDTH)
        );
    }

    /// `glitch` has **two** live exponents: `0x10004da0` raises `ratio` to
    /// `gamma_in` for the rows the mask sends to `Src1` and `1 - ratio` to
    /// `gamma_out` for the `Src2` rows.  Each one alone must move pixels.
    #[test]
    fn both_gamma_options_scale_glitchs_rows() {
        let weak_in = transition_pass(
            "glitch",
            &format!("%[time: 100, gamma_in: 0.25, {WALK}]"),
            50,
        );
        let strong_in = transition_pass(
            "glitch",
            &format!("%[time: 100, gamma_in: 4.0, {WALK}]"),
            50,
        );
        assert_ne!(
            weak_in,
            strong_in,
            "gamma_in 0.25 and 4.0 must displace the Src1 rows differently: {}",
            first_difference(&weak_in, &strong_in, WIDTH)
        );

        let weak_out = transition_pass(
            "glitch",
            &format!("%[time: 100, gamma_out: 0.25, {WALK}]"),
            50,
        );
        let strong_out = transition_pass(
            "glitch",
            &format!("%[time: 100, gamma_out: 4.0, {WALK}]"),
            50,
        );
        assert_ne!(
            weak_out,
            strong_out,
            "gamma_out 0.25 and 4.0 must displace the Src2 rows differently: {}",
            first_difference(&weak_out, &strong_out, WIDTH)
        );
    }

    /// `fadein` swaps the distorted face (`Src1` → `Src2`, `0x100047a0`) and
    /// makes the factory read `gamma_in` instead of `gamma_out`; at half the
    /// clock both scales are `0.5`, so the difference is the face itself.
    #[test]
    fn fadein_swaps_the_distorted_face() {
        let forward = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, fadein: 0, color: 0x808080, {WALK}]"),
            50,
        );
        let backward = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, fadein: 1, color: 0x808080, {WALK}]"),
            50,
        );
        assert_ne!(
            forward,
            backward,
            "the destination's own face and the source's must compose differently: {}",
            first_difference(&forward, &backward, WIDTH)
        );

        // `fadein` also routes the gamma name: `gamma_in` is the scale then.
        let slow_in = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, fadein: 1, gamma_in: 0.25, color: 0x808080, {WALK}]"),
            50,
        );
        let fast_in = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, fadein: 1, gamma_in: 4.0, color: 0x808080, {WALK}]"),
            50,
        );
        assert_ne!(
            slow_in,
            fast_in,
            "gamma_in must be the exponent under fadein: {}",
            first_difference(&slow_in, &fast_in, WIDTH)
        );
        // `gamma_out` is dead under `fadein` (the factory read `gamma_in`).
        let with_out = transition_pass(
            "fadeglitch",
            &format!(
                "%[time: 100, fadein: 1, gamma_in: 0.25, gamma_out: 8.0, color: 0x808080, {WALK}]"
            ),
            50,
        );
        assert_eq!(
            slow_in, with_out,
            "gamma_out is not the name `fadein` reads"
        );
    }

    /// `nofade` turns the colour wash off (the factory forces `color` to the
    /// neutral `0x808080`), and `color` picks the colour it washes toward.
    #[test]
    fn nofade_and_color_drive_the_colour_wash() {
        let washed = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, color: 0xff0000, {WALK}]"),
            50,
        );
        let plain = transition_pass("fadeglitch", &format!("%[time: 100, {WALK}]"), 50);
        let unwash = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, nofade: 1, color: 0xff0000, {WALK}]"),
            50,
        );
        assert_ne!(
            washed,
            unwash,
            "the wash must move pixels when it runs and not when `nofade` is set: {}",
            first_difference(&washed, &unwash, WIDTH)
        );
        assert_ne!(
            plain,
            unwash,
            "the default `color` (0) is a wash too — only 0x808080 is neutral: {}",
            first_difference(&plain, &unwash, WIDTH)
        );

        let red = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, color: 0xff0000, {WALK}]"),
            50,
        );
        let blue = transition_pass(
            "fadeglitch",
            &format!("%[time: 100, color: 0x0000ff, {WALK}]"),
            50,
        );
        assert_ne!(
            red,
            blue,
            "the wash colour must reach the pixels: {}",
            first_difference(&red, &blue, WIDTH)
        );
    }

    /// The eight distortion parameters feed the per-pass tables: `noise` is
    /// the row table's scale (`0x10005fc0`), `sft_*` the block walks'
    /// (`0x10005dc0`), and each is read from the options object copied at
    /// `StartTransition`.
    #[test]
    fn the_distortion_options_feed_the_per_pass_tables() {
        let quiet = transition_pass(
            "fadeglitch",
            "%[time: 100, color: 0x808080, noise: 0, sft_x: 0, sft_y: 0, sft_col: 0, per_x: 0, per_y: 0, per_col: 0, per_reset: 0]",
            50,
        );
        let noisy = transition_pass(
            "fadeglitch",
            "%[time: 100, color: 0x808080, noise: 16, sft_x: 0, sft_y: 0, sft_col: 0, per_x: 0, per_y: 0, per_col: 0, per_reset: 0]",
            50,
        );
        assert_ne!(
            quiet[..],
            noisy[..],
            "`noise` must move whole rows: {}",
            first_difference(&quiet, &noisy, WIDTH)
        );

        let walked = transition_pass(
            "fadeglitch",
            "%[time: 100, color: 0x808080, noise: 0, sft_x: 16, sft_y: 0, sft_col: 0, per_x: 1.0, per_y: 0, per_col: 0, per_reset: 0]",
            50,
        );
        assert_ne!(
            quiet[..],
            walked[..],
            "`sft_x`/`per_x` must walk the blocks horizontally: {}",
            first_difference(&quiet, &walked, WIDTH)
        );
    }

    /// The reference rebuilds the displacement tables inside `StartProcess`
    /// (`0x100055d0`, `0x10005540`, `0x10005730` all reseed from the tick), so
    /// two passes of a `loopglitch` transition — whose scale (`coef`) and
    /// plane (`Src1`) are both constant — still compose differently, because
    /// their seeds differ.
    #[test]
    fn every_pass_rebuilds_the_tables_from_the_tick() {
        let mut engine = engine();
        run(&mut engine, TRANSITION_PAIR);
        run(
            &mut engine,
            &format!(
                r#"dest.beginTransition("loopglitch", true, source, %[time: 1000, coef: 1.0, {WALK}]);"#
            ),
        );
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("first pass");
        let first = pixels(&mut engine, "dest").1;
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(100),
            )
            .expect("second pass");
        let second = pixels(&mut engine, "dest").1;
        assert_ne!(
            first,
            second,
            "a constant scale and `Src1` alone leave the seed as the only \
             difference between two passes: {}",
            first_difference(&first, &second, WIDTH)
        );
    }

    /// `break` is the one member of `loopglitch`'s own option pool
    /// (`coef/break/loopglitch` next to its vtable) with no effect: its
    /// per-pass hook `0x100055d0` stores the truthiness at `+0xc8` and nothing
    /// reads that byte afterwards.  The port accepts it and composes
    /// identically.
    #[test]
    fn break_is_accepted_and_changes_nothing() {
        let options = format!("%[time: 100, coef: 1.5, {WALK}]");
        let plain = transition_pass("loopglitch", &options, 50);
        let broken = transition_pass_with("loopglitch", &options, r#"opts["break"] = 1;"#, 50);
        assert_eq!(plain, broken, "`break` has no reader in the DLL");
    }

    /// `glitch` — `tTVPTransGlitch`, vtable `0x10016294` — is the one name in
    /// this family that mixes the two faces: `0x10005730` fills a per-row mask
    /// with probability `ratio` and `0x10004da0` sends a masked row to `Src1`
    /// and the others to `Src2`, so one composed frame carries pixels of both.
    #[test]
    fn glitch_mixes_rows_of_both_faces() {
        let composed = transition_pass("glitch", &format!("%[time: 100, block: 8, {WALK}]"), 50);
        let blues = composed
            .chunks_exact(4)
            .map(|pixel| pixel[2])
            .collect::<Vec<_>>();
        assert!(
            blues.contains(&0x80) && blues.contains(&0x40),
            "both faces' rows must appear: dest 0x80 and source 0x40, got {:?}",
            blues
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
        );
    }

    /// `loopglitch` — `tTVPTransLoopGlitch`, vtable `0x10016380` — distorts
    /// **one** face (`Src1`, the destination's own bitmap) at the constant
    /// scale `coef`; no row ever samples `Src2`, so the source's blue (`0x40`)
    /// must not appear, and `coef` must move the pixels.
    #[test]
    fn loopglitch_distorts_its_own_face_alone() {
        let base = format!("%[time: 100, block: 8, {WALK}]");
        let composed = transition_pass("loopglitch", &base, 50);
        let blues = composed
            .chunks_exact(4)
            .map(|pixel| pixel[2])
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            !blues.contains(&0x40),
            "`Src2` must never be sampled: got blue lanes {blues:?}"
        );

        let weak = transition_pass(
            "loopglitch",
            &format!("%[time: 100, coef: 0.25, block: 8, {WALK}]"),
            50,
        );
        let strong = transition_pass(
            "loopglitch",
            &format!("%[time: 100, coef: 3.0, block: 8, {WALK}]"),
            50,
        );
        assert_ne!(
            weak,
            strong,
            "coef scales the walk: {}",
            first_difference(&weak, &strong, WIDTH)
        );
    }

    /// The DLL layout this module's kind ↔ class mapping is read from — the
    /// three facts `objdump` recovers from the shipped image (see the module
    /// docs for the exact commands).  Not machine-checked against the DLL at
    /// test time (the image is not part of the workspace); the assertions
    /// below cross-check this implementation against the table instead, which
    /// is what makes a swapped mapping fail.
    const RECOVERED_CLASSES: [(&str, u32, [&str; 4], u32); 3] = [
        // name, handler vtable, the UTF-16 option pool laid out right after
        // that vtable, slot-9 (the compose hook `0x10004e10` calls).
        (
            "glitch",
            0x10016294,
            ["time", "gamma_in", "gamma_out", "block"],
            0x10004da0,
        ),
        (
            "fadeglitch",
            0x10016310,
            ["nofade", "fadein", "color", "fadeglitch"],
            0x100047a0,
        ),
        (
            "loopglitch",
            0x10016380,
            ["coef", "break", "loopglitch", "block"],
            0x10004d70,
        ),
    ];

    /// The mapping pinned against the DLL's own layout: the names match the
    /// class-name strings, `gamma_in`/`gamma_out` belong to `glitch` alone
    /// while `coef`/`break` belong to `loopglitch` alone, and the compose
    /// honours it — `gamma_in` moves `glitch`'s pixels but not `loopglitch`'s,
    /// `coef` moves `loopglitch`'s but not `glitch`'s.
    ///
    /// The first port of this module had `glitch` and `loopglitch` the other
    /// way round (it read the mapping off field offsets rather than off the
    /// constructors' immediates, the vtable slots and the string pools), which
    /// every assertion here fails on.
    #[test]
    fn the_recovered_class_layout_pins_the_compose_hooks() {
        let mut names = RECOVERED_CLASSES
            .map(|(name, ..)| name.to_string())
            .to_vec();
        names.sort();
        assert_eq!(
            names,
            TRANSITION_NAMES.map(str::to_string).to_vec(),
            "the provider names are the DLL's own class names"
        );

        let pool = |name: &str| {
            RECOVERED_CLASSES
                .iter()
                .find(|(entry, ..)| *entry == name)
                .expect("recovered entry")
                .2
        };
        assert!(
            pool("glitch").contains(&"gamma_in") && pool("glitch").contains(&"gamma_out"),
            "glitch's own option pool is time/gamma_in/gamma_out/block"
        );
        assert!(
            !pool("loopglitch").contains(&"gamma_in") && !pool("loopglitch").contains(&"gamma_out"),
            "loopglitch's pool is coef/break/loopglitch — it reads no gamma"
        );
        assert!(
            pool("loopglitch").contains(&"coef") && !pool("glitch").contains(&"coef"),
            "coef is loopglitch's constant scale, not glitch's"
        );
        assert_eq!(
            RECOVERED_CLASSES.map(|(.., hook)| hook).len(),
            3,
            "each class's compose hook is a distinct slot-9 function"
        );

        // Behavioural half: the exponents and the scale reach the pixels the
        // table says they must — and the ones the table does not give them do
        // not.
        let glitch =
            |extra: &str| transition_pass("glitch", &format!("%[time: 100, {extra}, {WALK}]"), 50);
        let loopglitch = |extra: &str| {
            transition_pass("loopglitch", &format!("%[time: 100, {extra}, {WALK}]"), 50)
        };
        assert_ne!(
            glitch("gamma_in: 0.25"),
            glitch("gamma_in: 4.0"),
            "glitch's Src1 exponent is gamma_in"
        );
        assert_eq!(
            glitch("coef: 0.25"),
            glitch("coef: 3.0"),
            "glitch reads no coef (it is not in its pool)"
        );
        assert_ne!(
            loopglitch("coef: 0.25"),
            loopglitch("coef: 3.0"),
            "loopglitch's scale is coef"
        );
        assert_eq!(
            loopglitch("gamma_in: 0.25"),
            loopglitch("gamma_in: 4.0"),
            "loopglitch reads no gamma_in (it is not in its pool)"
        );
    }

    /// The factories' own rules: `glitch` and `fadeglitch` require `time`
    /// (`0x10002e90`/`0x10002ca0` read it with a `-1` default and leave
    /// through their failure exit), and `glitch`'s provider requires equal
    /// sizes (the `+0x14` flag only its registration helper sets).
    #[test]
    fn the_factories_enforce_their_required_options() {
        let mut engine = engine();
        run(&mut engine, TRANSITION_PAIR);
        for name in ["glitch", "fadeglitch"] {
            let message = run(
                &mut engine,
                &format!(
                    r#"
                    var message = "";
                    try {{ dest.beginTransition("{name}", true, source, %[block: 8]); }}
                    catch (e) {{ message = e.message; }}
                    return message;
                    "#
                ),
            );
            assert_eq!(
                message.to_tjs_string().expect("string"),
                "Transition handler error iTVPTransHandlerProvider::StartTransition failed",
                "{name} must fail without `time`"
            );
        }

        // `glitch` refuses an unequal pair; `fadeglitch` (`ttSimple`) accepts
        // one.  (`beginTransition` rejects a 4x4 source for the crossfade
        // family's own rules, so the mismatch here is one the provider sees.)
        run(
            &mut engine,
            r#"
            global.wide = new Layer();
            wide.setImageSize(64, 32);
            wide.fillRect(0, 0, 64, 32, 0xff000000);
            wide.visible = true;
            "#,
        );
        let message = run(
            &mut engine,
            r#"
            var message = "";
            try { dest.beginTransition("glitch", true, wide, %[time: 10]); }
            catch (e) { message = e.message; }
            return message;
            "#,
        );
        assert_eq!(
            message.to_tjs_string().expect("string"),
            "Transition handler error iTVPTransHandlerProvider::StartTransition failed"
        );
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("must be the same size")),
            "the provider's own reason reaches the host log"
        );
    }

    /// The colour wash (`0x100033f0`) is transcribed from the listing: the
    /// neutral colour is the identity, and every other colour moves the
    /// pixel — as the DLL's own byte masks do, not as an average would.
    #[test]
    fn the_colour_wash_is_the_reference_transcription() {
        // `0x808080` is the class's neutral spelling: the wash still runs, and
        // it is the identity because `e - carry` cancels.
        let neutral = [0x11, 0x22, 0x33, 0xff];
        let mut pixels = neutral;
        colour_wash(&mut pixels, 0x808080);
        assert_eq!(pixels, neutral, "0x808080 is the neutral colour");

        // A colour washes every pixel toward itself; the alpha lane is kept.
        let mut pixels = neutral;
        colour_wash(&mut pixels, 0xff0000);
        assert_ne!(pixels, neutral);
        assert_eq!(pixels[3], 0xff, "the wash keeps the alpha lane");

        // `washed_colour` (`0x10003710`) runs from the neutral grey 128 at
        // `x = 0` to the requested colour at `x = 1`.
        assert_eq!(washed_colour(0xff0000, 0.0), 0x808080);
        assert_eq!(washed_colour(0xff0000, 1.0), 0xff0000);
        assert_eq!(washed_colour(0x808080, 0.5), 0x808080);
    }
}
