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
//! **Mapped, not implemented**: the three transition names. Registering a
//! transition provider needs an engine-side registry that accepts a plugin's
//! own name and kernel; this engine has none yet, and its name lookup degrades
//! a linked plugin's names to `crossfade`
//! (`crates/krkr-engine/src/native/classes.rs`, `PLUGIN_TRANSITION_NAMES`).
//! So `trans method=glitch` / `Layer.beginTransition("glitch", …)` resolves
//! and cross-fades instead of glitching — the compat alternative the dossier
//! describes, which M54 put in place. The handler option names that only the
//! transition path reads (`time`, `block`, `break`, `nofade`, `fadein`,
//! `gamma_in`, `gamma_out`, `color`) are therefore parsed by nothing here.
//!
//! # Recovered parameters
//!
//! `0x10001ab0` (and its twin `0x10001bc0`) reads the eight distortion
//! parameters out of an option object by name, each with a default double:
//!
//! | name | default | where the default lives | role |
//! |---|---|---|---|
//! | `noise` | 4.0 | `.rdata` `0x10016770` | per-row displacement amplitude |
//! | `sft_x` | 16.0 | `0x10016780` | per-block x displacement |
//! | `sft_y` | 8.0 | `0x10016778` | per-block y displacement |
//! | `sft_col` | 8.0 | `0x10016778` | per-block colour displacement |
//! | `per_x` | 0.5 | `0x10016758` | probability a block takes its x displacement |
//! | `per_y` | 0.25 | `0x10016750` | probability a block takes its y displacement |
//! | `per_col` | 0.05 | `0x10016748` | probability a block takes its colour split |
//! | `per_reset` | 0.02 | `0x10016740` | probability a taken displacement is dropped again |
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
//!   128-bit shift-register stream) then feeds the table fills. This port
//!   keeps the seeding and draws the tables from the splitmix64 stream
//!   directly, so runs are reproducible from `seed` but the bit stream is not
//!   bit-identical to the reference's.
//! * **Block tables** — `0x10005dc0` fills three per-block tables through
//!   `0x10002250`, one per parameter triple: `(per_x, sft_x)`, `(per_y,
//!   sft_y)`, `(per_col, sft_col)`, each gated by `per_reset`. `0x10002250`
//!   draws a uniform, accumulates `rand * scale` only while it is under the
//!   probability, and multiplies the result by `0.0` (`.rdata` `0x10016738`)
//!   when a second uniform is under the reset probability. Two draws per
//!   entry, in that order, is what this port reproduces.
//! * **Row table** — `0x10005fc0` fills one value per row as
//!   `rand * noise` (`[edi]` is `params.noise`, i.e. struct offset 0), with
//!   no probability gate: every row jitters.
//! * **The pass** — `0x100048f0` walks the destination rows; a row's source
//!   pointer is displaced by the row table plus the block x table, the block
//!   y table moves which source row is read, and the block colour table
//!   displaces the colour samples. `0x10004f40` copies a run one pixel at a
//!   time from **three** source pointers — `out[0]` from the first, `out[1]`
//!   and `out[3]` from the second, `out[2]` from the third
//!   (`0x10005045`-`0x10005057`) — i.e. R, G and B are sampled at three
//!   different horizontal positions, the chromatic split `sft_col` drives.
//!   Source pointers are clamped into the bitmap
//!   (`0x10004f76`/`0x10004fd2`), which is the edge-clamp this port samples
//!   with.
//! * **Byte order** — the reference indexes its BGRA buffers from byte 0; the
//!   engine's views are RGBA, so the port's channel order is R, G, B, A
//!   (`docs/plugins/plugin-facing-engine-facilities.md` §B.3.4).

use std::time::{SystemTime, UNIX_EPOCH};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{LayerBitmap, layer_bitmap_read_write, layer_update},
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Layer.doGlitch / Layer.glitchCopy pixel glitch, and the glitch / fadeglitch / loopglitch transition names",
    notes: "The two Layer members are real pixel work: the recovered splitmix64-seeded block/row/colour displacement (defaults noise 4, sft_x 16, sft_y 8, sft_col 8, per_x 0.5, per_y 0.25, per_col 0.05, per_reset 0.02, size 16, coef 1) runs over the scoped layer bitmap views and repaints. The three transition providers are not implemented — the engine has no plugin transition-provider registry, so its lookup maps the name to crossfade while GlitchEffect.dll is linked — and the handler-only options (time, block, break, nofade, fadein, gamma_in, gamma_out, color) are therefore read by nothing.",
    install: |engine| engine.register_plugin(GlitchEffectPlugin),
};

pub struct GlitchEffectPlugin;

impl KrkrPlugin for GlitchEffectPlugin {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_layer_members(runtime);
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

    /// A uniform in `[0, 1)`, the range `0x10002250` compares its
    /// probabilities against (it builds the double from the top 53 bits).
    fn uniform01(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A signed displacement in `[-1, 1]`, the shape the block tables
    /// accumulate before scaling.
    fn bipolar(&mut self) -> f64 {
        self.uniform01() * 2.0 - 1.0
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

        // The row table has no probability gate in the reference: every row
        // is filled with `rand * noise` (`0x10005fc0` reads `params.noise`).
        let line = (0..height).map(|_| rng.bipolar() * options.noise).collect();

        let mut gated = |per: f64, scale: f64| {
            (0..cols * rows)
                .map(|_| {
                    let mut value = 0.0;
                    if rng.uniform01() < per {
                        value = rng.bipolar() * scale;
                    }
                    if rng.uniform01() < options.per_reset {
                        value = 0.0;
                    }
                    value
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

/// The pass `0x100048f0` walks and `0x10004f40` copies: every destination
/// pixel inside `clip` is written from the source bitmap at the block's
/// displacement, with the red and blue channels sampled `dcol` either side of
/// the green sample (the three source pointers of the copy loop).
fn apply_glitch(
    source: &[u8],
    source_bitmap: &LayerBitmap,
    dest: &mut [u8],
    dest_bitmap: &LayerBitmap,
    clip: (i64, i64, i64, i64),
    tables: &GlitchTables,
    options: &GlitchOptions,
) {
    let (left, top, width, height) = clip;
    let right = (left + width).clamp(0, i64::from(dest_bitmap.width));
    let bottom = (top + height).clamp(0, i64::from(dest_bitmap.height));
    let left = left.clamp(0, i64::from(dest_bitmap.width));
    let top = top.clamp(0, i64::from(dest_bitmap.height));

    for y in top..bottom {
        let line = tables.line[(y as u32).min(dest_bitmap.height.saturating_sub(1)) as usize];
        for x in left..right {
            let cell = tables.cell(x as u32, y as u32);
            let shift_x = ((line + tables.dx[cell]) * options.coef) as i64;
            let shift_y = (tables.dy[cell] * options.coef) as i64;
            let colour = (tables.colour[cell] * options.coef) as i64;

            let sx = x + shift_x;
            let sy = y + shift_y;
            let green = sample(source, source_bitmap, sx, sy);
            let red = sample(source, source_bitmap, sx + colour, sy);
            let blue = sample(source, source_bitmap, sx - colour, sy);

            let index = pixel_offset(x as u32, y as u32, dest_bitmap.width);
            dest[index..index + 4].copy_from_slice(&[red[0], green[1], blue[2], green[3]]);
        }
    }
}

/// The edge-clamped sample the reference's pointer bounds checks
/// (`0x10004f76`, `0x10004fd2`) amount to: a coordinate outside the source
/// bitmap reads the nearest edge pixel.
fn sample(source: &[u8], bitmap: &LayerBitmap, x: i64, y: i64) -> Rgba {
    if bitmap.width == 0 || bitmap.height == 0 {
        return [0, 0, 0, 0];
    }
    let x = x.clamp(0, i64::from(bitmap.width) - 1) as u32;
    let y = y.clamp(0, i64::from(bitmap.height) - 1) as u32;
    let index = pixel_offset(x, y, bitmap.width);
    match source.get(index..index + 4) {
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
            &source_view.bitmap,
            dest_view.pixels,
            &dest_view.bitmap,
            dest_view.bitmap.clip,
            &tables,
            options,
        );
    })?;
    layer_update(runtime, dest)
}

// ------------------------------------------------------------------ members

/// `Layer.doGlitch(...)`:  the distorted pixels land on the layer the call
/// went through.
///
/// The reference's invoker (`0x10002fb0`) requires an object argument before
/// it runs, and its pass (`0x10004b00`) reads the options out of an object;
/// the shape this port accepts covers both readings: an object argument that
/// carries the layer member surface is the *source* layer, any other object
/// is the option object. A call with neither distorts the layer in place.
fn layer_do_glitch(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(dest) = this_obj else {
        return Err(TjsError::bad_param_count());
    };
    let (source, options) = split_layer_and_options(runtime, dest, &args);
    let options = GlitchOptions::from_object(runtime, options);
    run_glitch(runtime, source.unwrap_or(dest), dest, &options)?;
    Ok(Variant::Void)
}

/// `Layer.glitchCopy(sourceLayer, options)`: `0x100040e0`'s invoker rejects
/// the call outright when the layer argument is not an object
/// (`0xfffffc15` = `TJS_E_BADPARAMCOUNT`), so a missing or non-layer source
/// is the same error here.
fn layer_glitch_copy(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(dest) = this_obj else {
        return Err(TjsError::bad_param_count());
    };
    let Some(source) = args.first().and_then(Variant::object_handle) else {
        return Err(TjsError::bad_param_count());
    };
    if !is_layer_like(runtime, source) {
        return Err(TjsError::bad_param_count());
    }
    let options = GlitchOptions::from_object(runtime, options_argument(runtime, &args[1..]));
    run_glitch(runtime, source, dest, &options)?;
    Ok(Variant::Void)
}

/// Splits a member's arguments into an optional source layer and an optional
/// option object, in the reference's order (layer first, options second).
fn split_layer_and_options(
    runtime: &Runtime<KrkrHost>,
    dest: ObjectHandle,
    args: &[Variant],
) -> (Option<ObjectHandle>, Option<ObjectHandle>) {
    let mut source = None;
    let mut rest = args;
    let candidate = args.first().and_then(Variant::object_handle);
    if let Some(candidate) =
        candidate.filter(|candidate| *candidate != dest && is_layer_like(runtime, *candidate))
    {
        source = Some(candidate);
        rest = &args[1..];
    }
    (source, options_argument(runtime, rest))
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use krkr_core::{FrameInput, Size};
    use krkr_engine::{
        EngineConfig, EngineInput, KrkrEngine,
        plugin_api::layer::{LayerBitmap, layer_bitmap_read},
    };
    use krkr_tjs2::runtime::Variant;

    use super::{GlitchEffectPlugin, GlitchOptions, PLUGIN_NAME};

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

    /// The source encodes its own coordinates: red carries `x`, green carries
    /// `y`, blue carries `(31 - x) * 8`, so a sampled pixel says which source
    /// position each channel came from.
    fn decoded(pixel: [u8; 4]) -> (i64, i64) {
        (i64::from(pixel[0]), i64::from(pixel[1]))
    }

    /// The source `x` the blue channel was sampled at, decoded from the
    /// `(31 - x) * 8` pattern.
    fn decoded_blue_x(pixel: [u8; 4]) -> i64 {
        31 - i64::from(pixel[2]) / 8
    }

    /// The edge clamp the pass samples with (`0x10004f76`): a coordinate
    /// outside the source bitmap reads the nearest edge pixel.
    fn clamped(coordinate: i64, extent: u32) -> i64 {
        coordinate.clamp(0, i64::from(extent) - 1)
    }

    /// A sampled coordinate is clamped, so a test recovers the displacement
    /// from an interior pixel and then checks the whole row or block against
    /// the clamped expectation.
    fn clamp_matches(sampled_at: i64, coordinate: i64, offset: i64, extent: u32) -> bool {
        sampled_at == clamped(coordinate + offset, extent)
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
            probe.doGlitch(%[size: 2, per_x: 0, per_y: 0, per_col: 0, noise: 0]);
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
        let (_, source) = pixels(&mut engine, "source");

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

        // Every block shares one displacement, and it stays inside `sft_*`.
        // An interior pixel recovers the displacement; the rest of the block
        // then has to match its edge-clamped samples.
        let mut block_offsets = Vec::new();
        for block in 0..4u32 {
            let probe_x = block * 8 + 4;
            let probe_y = block * 8 + 4;
            let (sx, sy) = decoded(pixel(&dest, bitmap.width, probe_x, probe_y));
            let offset = (sx - i64::from(probe_x), sy - i64::from(probe_y));
            assert!(
                offset.0.abs() <= 8 && offset.1.abs() <= 4,
                "offset {offset:?} escapes sft_x/sft_y"
            );
            for y in block * 8..block * 8 + 8 {
                for x in block * 8..block * 8 + 8 {
                    let (sx, sy) = decoded(pixel(&dest, bitmap.width, x, y));
                    assert!(
                        clamp_matches(sx, i64::from(x), offset.0, WIDTH)
                            && clamp_matches(sy, i64::from(y), offset.1, HEIGHT),
                        "({x}, {y}) in block {block} does not share the displacement {offset:?}"
                    );
                }
            }
            block_offsets.push(offset);
        }
        assert!(
            block_offsets
                .iter()
                .any(|offset| *offset != block_offsets[0]),
            "every block moved the same way: {block_offsets:?}"
        );
    }

    /// The row table has no probability gate: with the block rates at zero,
    /// `noise` still shifts whole rows, each row by its own amount.
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
        let mut row_offsets = Vec::new();
        for y in 0..32u32 {
            let probe = WIDTH / 2;
            let (sx, _) = decoded(pixel(&dest, bitmap.width, probe, y));
            let offset = sx - i64::from(probe);
            assert!(offset.abs() <= 4, "row {y} escapes the noise amplitude");
            for x in 0..32u32 {
                let (sx, sy) = decoded(pixel(&dest, bitmap.width, x, y));
                assert!(
                    clamp_matches(sx, i64::from(x), offset, WIDTH),
                    "row {y} is not shifted as one: ({x}, {y}) samples {sx}, not {}",
                    clamped(i64::from(x) + offset, WIDTH)
                );
                assert_eq!(
                    sy,
                    i64::from(y),
                    "green keeps its own source row at ({x}, {y})"
                );
            }
            row_offsets.push(offset);
        }
        assert!(
            row_offsets.iter().any(|offset| *offset != row_offsets[0]),
            "no row moved: {row_offsets:?}"
        );
    }

    /// `sft_col` splits the channels: red and blue sample either side of the
    /// green sample (`0x10004f40`'s three source pointers), and green keeps
    /// the geometric sample.
    #[test]
    fn colour_split_moves_red_against_blue() {
        let mut engine = engine();
        run(&mut engine, SOURCE_LAYER);
        run(
            &mut engine,
            r#"
            dest.glitchCopy(source, %[size: 8, seed: 3, noise: 0, sft_x: 0, sft_y: 0,
                sft_col: 6, per_x: 0, per_y: 0, per_col: 1.0, per_reset: 0]);
            "#,
        );
        let (bitmap, dest) = pixels(&mut engine, "dest");
        let mut split_seen = false;
        // Interior pixels only: their samples cannot have been clamped.
        for y in 0..32u32 {
            for x in 8..24u32 {
                let value = pixel(&dest, bitmap.width, x, y);
                let red = i64::from(value[0]);
                let green = i64::from(value[1]);
                let blue = decoded_blue_x(value);
                assert_eq!(green, i64::from(y), "green keeps the source row");
                assert!(
                    (red - blue).abs() <= 12,
                    "the colour split escapes 2 * sft_col at ({x},{y}): R {red} vs B {blue}"
                );
                assert_eq!(
                    (red - blue) % 2,
                    0,
                    "red and blue must straddle the green sample symmetrically at ({x},{y}): R {red}, B {blue}"
                );
                if red != i64::from(x) || blue != i64::from(x) {
                    split_seen = true;
                }
            }
        }
        assert!(split_seen, "no block took its colour displacement");
    }

    /// `doGlitch` without a source layer distorts in place, and `glitchCopy`
    /// without its layer argument is the reference's bad-argument-count
    /// error (`0x100040e0` returns `0xfffffc15`).
    #[test]
    fn do_glitch_works_in_place_and_glitch_copy_rejects_a_missing_layer() {
        let mut engine = engine();
        run(&mut engine, SOURCE_LAYER);
        let (bitmap, before) = pixels(&mut engine, "source");
        run(
            &mut engine,
            r#"
            source.doGlitch(%[size: 8, seed: 42, noise: 2, sft_x: 6, sft_y: 6,
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

        let message = run(
            &mut engine,
            r#"
            var message = "";
            try { dest.glitchCopy(); } catch (e) { message = e.message; }
            return message;
            "#,
        );
        assert_eq!(
            message.to_tjs_string().expect("string"),
            "Invalid argument count"
        );
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
            try { freed.doGlitch(); } catch (e) { message = e.message; }
            return message;
            "#,
        );
        assert_eq!(
            message.to_tjs_string().expect("string"),
            "Not drawable layer type"
        );
    }

    /// The plugin's linkage is what makes its three transition names resolve
    /// at all: the engine's lookup answers a linked plugin's names with the
    /// crossfade degrade (`PLUGIN_TRANSITION_NAMES`), and without the plugin
    /// they stay unknown — the reference's own provider-gated behaviour.
    #[test]
    fn the_three_transition_names_follow_the_plugin_linkage() {
        for name in ["glitch", "fadeglitch", "loopglitch"] {
            let mut linked = engine();
            run(&mut linked, SOURCE_LAYER);
            run(
                &mut linked,
                &format!("source.beginTransition(\"{name}\", true, dest, %[time: 10]);"),
            );
            let frame = linked
                .update(
                    EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                    Duration::ZERO,
                )
                .expect("update");
            assert_eq!(
                frame.output.transitions.first().expect("transition").method,
                "crossfade",
                "{name} is mapped by the engine while {PLUGIN_NAME} is linked"
            );

            let mut unlinked = KrkrEngine::new(EngineConfig::default()).expect("engine");
            run(&mut unlinked, SOURCE_LAYER);
            let message = run(
                &mut unlinked,
                &format!(
                    r#"
                    var message = "";
                    try {{ source.beginTransition("{name}", true, dest, %[time: 10]); }}
                    catch (e) {{ message = e.message; }}
                    return message;
                    "#
                ),
            );
            assert_eq!(
                message.to_tjs_string().expect("string"),
                format!("Cannot find transition handler {name}"),
                "{name} stays unknown without the plugin, as the reference does"
            );
        }
    }
}
