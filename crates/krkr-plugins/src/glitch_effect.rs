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
    notes: "The two Layer members are real pixel work: the recovered splitmix64-seeded block/row/colour displacement — a Marsaglia-polar normal step per entry, accumulated into a running walk that `per_reset` zeroes (defaults noise 4, sft_x 16, sft_y 8, sft_col 8, per_x 0.5, per_y 0.25, per_col 0.05, per_reset 0.02, size 16, coef 1) — runs over the scoped layer bitmap views and repaints; `doGlitch` takes its two object arguments as the reference invoker type-checks them. The three transition providers are not implemented — the engine has no plugin transition-provider registry, so its lookup maps the name to crossfade while GlitchEffect.dll is linked — and the handler-only options (time, block, break, nofade, fadein, gamma_in, gamma_out, color) are therefore read by nothing.",
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

    let source_width = i64::from(source_bitmap.width);
    let source_pixels = source_width * i64::from(source_bitmap.height);
    let size = i64::from(tables.size);

    for y in top..bottom {
        let line = tables.line[(y as u32).min(dest_bitmap.height.saturating_sub(1)) as usize];
        let mut x = left;
        while x < right {
            let run_end = (((x / size) + 1) * size).min(right);
            let length = run_end - x;
            let cell = tables.cell(x as u32, y as u32);
            let shift_x = ((line + tables.dx[cell]) * options.coef) as i64;
            let shift_y = (tables.dy[cell] * options.coef) as i64;
            let colour = (tables.colour[cell] * options.coef) as i64;

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
                let index = pixel_offset((x + step) as u32, y as u32, dest_bitmap.width);
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
