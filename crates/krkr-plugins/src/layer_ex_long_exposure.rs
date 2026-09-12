//! `layerExLongExposure.dll`: the long-exposure accumulator.
//!
//! Real plugin: `layerExLongExposure/Main.cpp` (kirikiri2 trunk,
//! `src/plugins/win32/layerExLongExposure`), five members attached to the
//! global `Layer` class with `NCB_ATTACH_CLASS_WITH_HOOK(LongExposure, Layer)`
//! (`:222-229`). Successive snapshots of the layer are summed into a
//! 32-bit-per-channel accumulator; `copyExposure` maps the accumulated range
//! back to 0..255 and writes it into the layer — the "long exposure" of a
//! moving sequence, aimed at building transition-rule artwork (`readme.txt`).
//!
//! | member | arguments | reference |
//! |---|---|---|
//! | `initExposure()` | 0 | `:8-15`: frees any buffer, then allocates `imageWidth*imageHeight*4` zeroed 32-bit sums |
//! | `snapExposure()` | 0 | `:17-40`: adds the layer's current image into the sums |
//! | `statExposure()` | 0 → Dictionary | `:80-102`: per-channel min/max of the sums |
//! | `copyExposure(stat)` | 1 | `:104-150`: normalizes the sums into the layer's own bitmap |
//! | `termExposure()` | 0 | `:152-156`: frees the buffer |
//!
//! Accumulation (`:30-39`) is a plain sum: `*w++ += (DWORD)(*r++)` gives every
//! channel byte its own 32-bit accumulator, wrapping on overflow — no
//! saturation and no averaging, and the sums are meant to exceed 255, because
//! `stat`/`copy` map the accumulated range back onto 0..255. The reference's
//! buffer holds one `DWORD` per channel in its B, G, R, A byte order; this port
//! stores the engine view's R, G, B, A order and names every channel the same
//! way, so the bytes it writes are identical
//! (`docs/plugins/plugin-facing-engine-facilities.md` §B.3.4).
//!
//! The accumulator is per-layer state: `init` allocates it (`:12-14`), `term`
//! frees it (`:152-156`), and the native instance is destroyed with the object
//! (`~LongExposure`, `:5`; the instance hook, `:212-221`). Here that is the
//! host's per-layer extension slot, pruned when the layer is invalidated.
//!
//! Three reference behaviours a script sees:
//!
//! * `snap`/`copy` require the image size `init` saw: a different size throws
//!   "invalid layer size" (`:23-24`, `:110-111`), and a layer without a usable
//!   image throws "invalid layer image" (`:21-22`, `:108-109`).
//! * `copy`'s argument picks the clamp (`:119-139`): `void` runs the implicit
//!   `statExposure`, an object is read member by member (a missing
//!   `min_*`/`max_*` reads 0), and anything else leaves the constructor's
//!   default min `0xFFFFFFFF`/max `0` (`:47`) — which `getNormalize` answers
//!   with 0xFF for every byte (`:62`), so a non-object argument whitens the
//!   image.
//! * No member calls `Layer.update()` — the reference has no `redraw()` at all
//!   — so neither does this port: the bytes commit through the plugin API's
//!   write path (visible to a following `getMainPixel`/`saveLayerImage`), and a
//!   script that wants the repaint calls `Layer.update()` itself.
//!
//! The reference's two crash paths (a failed `GetLayerImage` is ignored and the
//! null buffer dereferenced, `:26-28`; `_stat`'s failure result is dead code)
//! become the errors above, the same way the rest of the family's ports handle
//! them.

use std::sync::{Mutex, MutexGuard};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{
        LayerBitmapView, LayerBitmapViewMut, layer_bitmap_read, layer_bitmap_write,
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer long-exposure accumulator (initExposure, snapExposure, statExposure, copyExposure, termExposure)",
    notes: "All five registered members ported from layerExLongExposure/Main.cpp:8-156 over the engine's layer bitmap views, error strings included. Frames add into per-channel 32-bit wrapping sums; copy normalizes (n-min)*255/(max-min) into the layer and, like the reference, never calls Layer.update(). The accumulator is a per-layer host extension, pruned on invalidate.",
    install: |engine| engine.register_plugin(LayerExLongExposurePlugin),
};

pub struct LayerExLongExposurePlugin;

impl KrkrPlugin for LayerExLongExposurePlugin {
    fn name(&self) -> &str {
        "layerExLongExposure.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `NCB_ATTACH_CLASS_WITH_HOOK(LongExposure, Layer)` (`Main.cpp:222-229`).
        // ncbind fails a call with `_numparams < ArgsCount` (`ncbind.hpp:1186`):
        // the four zero-argument members take any call count, `copy` declares
        // one `tTJSVariant` (`:104`), and extra arguments are ignored.
        for (name, arg_count, function) in LAYER_MEMBERS {
            register_unless_closure(runtime, layer, name, *arg_count, *function);
        }
        Ok(())
    }
}

type LayerMember = (
    &'static str,
    NativeArgCount,
    fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>,
);

static LAYER_MEMBERS: &[LayerMember] = &[
    ("initExposure", NativeArgCount::Any, layer_init_exposure),
    ("snapExposure", NativeArgCount::Any, layer_snap_exposure),
    ("statExposure", NativeArgCount::Any, layer_stat_exposure),
    (
        "copyExposure",
        NativeArgCount::AtLeast(1),
        layer_copy_exposure,
    ),
    ("termExposure", NativeArgCount::Any, layer_term_exposure),
];

/// One layer's accumulator (`Main.cpp:4-6, :159-161`): the image size `init`
/// saw, plus four 32-bit sums per pixel — one per channel, in the engine view's
/// R, G, B, A byte order.
struct Exposure {
    width: u32,
    height: u32,
    /// `(y * width + x) * 4 + channel`, i.e. one `u32` per channel per pixel.
    sums: Vec<u32>,
}

/// The per-layer slot. `None` is the reference's `buffer == 0`: the state a
/// failed `init` leaves behind, what `term` restores, and what the object's own
/// death produces (`~LongExposure`, `:5`). The host prunes the whole slot when
/// the layer is invalidated, so the accumulator dies with the native instance
/// (`:212-221`).
type ExposureSlot = Mutex<Option<Exposure>>;

/// `initExposure()` (`Main.cpp:8-15`).
///
/// `term()` runs first (`:9`), so a re-init drops the sums and a *failed*
/// init leaves the layer with no accumulator at all.
fn layer_init_exposure(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(runtime, this_obj)?;
    let slot = runtime
        .host_mut()
        .layer_extension_or_insert_with::<ExposureSlot>(layer, || Mutex::new(None));
    let mut state = lock(&slot);
    *state = None;
    let Some((width, height)) = image_size(runtime, layer) else {
        return Err(TjsError::runtime("LongExposure.init: invalid layer image"));
    };
    // `new DWORD[width * height * 4]` + `ZeroMemory` (`:12-14`).
    *state = Some(Exposure {
        width,
        height,
        sums: vec![0; width as usize * height as usize * 4],
    });
    Ok(Variant::Void)
}

/// `snapExposure()` (`Main.cpp:17-40`).
fn layer_snap_exposure(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(runtime, this_obj)?;
    let Some(slot) = runtime.host().layer_extension::<ExposureSlot>(layer) else {
        return Err(TjsError::runtime("LongExposure.snap: not initialized"));
    };
    let mut state = lock(&slot);
    let Some(exposure) = state.as_mut() else {
        return Err(TjsError::runtime("LongExposure.snap: not initialized"));
    };
    let Some((width, height)) = image_size(runtime, layer) else {
        return Err(TjsError::runtime("LongExposure.snap: invalid layer image"));
    };
    if (width, height) != (exposure.width, exposure.height) {
        return Err(TjsError::runtime("LongExposure.snap: invalid layer size"));
    }
    // The reference ignores `GetLayerImage`'s failure and dereferences the null
    // buffer (`:26-28`); a layer the size probe above accepted can only lose its
    // image between the two calls in a way the engine does not allow, so this
    // reports the reference's image error instead of crashing.
    if layer_bitmap_read(runtime, layer, |view| accumulate(exposure, view)).is_err() {
        return Err(TjsError::runtime("LongExposure.snap: invalid layer image"));
    }
    Ok(Variant::Void)
}

/// `statExposure()` (`Main.cpp:80-102`).
///
/// Only the buffer is checked — the layer's current image is irrelevant to the
/// sums (`:81-86`).
fn layer_stat_exposure(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(runtime, this_obj)?;
    let Some(slot) = runtime.host().layer_extension::<ExposureSlot>(layer) else {
        return Err(TjsError::runtime("LongExposure.stat: not initialized"));
    };
    let state = lock(&slot);
    let Some(exposure) = state.as_ref() else {
        return Err(TjsError::runtime("LongExposure.stat: not initialized"));
    };
    let bounds = stat_bounds(&exposure.sums);
    // `ncbDictionaryAccessor` with the reference's member order and 64-bit
    // `tTVInteger` values (`:88-100`).
    let dictionary = runtime.alloc_dictionary_object();
    for (member, value) in [
        ("min_r", bounds.min[0]),
        ("min_g", bounds.min[1]),
        ("min_b", bounds.min[2]),
        ("min_a", bounds.min[3]),
        ("max_r", bounds.max[0]),
        ("max_g", bounds.max[1]),
        ("max_b", bounds.max[2]),
        ("max_a", bounds.max[3]),
    ] {
        runtime.set_object_member(dictionary, member, Variant::Integer(i64::from(value)));
    }
    Ok(Variant::Object(dictionary))
}

/// `copyExposure(stat)` (`Main.cpp:104-150`).
fn layer_copy_exposure(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(runtime, this_obj)?;
    let Some(slot) = runtime.host().layer_extension::<ExposureSlot>(layer) else {
        return Err(TjsError::runtime("LongExposure.copy: not initialized"));
    };
    let state = lock(&slot);
    let Some(exposure) = state.as_ref() else {
        return Err(TjsError::runtime("LongExposure.copy: not initialized"));
    };

    // The argument switch (`:119-139`): `void` is the implicit `statExposure`,
    // an object goes through `ncbPropAccessor::getIntValue` member by member,
    // and every other type — as well as `null` — keeps the constructor's
    // default bounds, whose `min >= max` answers 0xFF per byte (`:47, :62`).
    let bounds = match args.first() {
        Some(value) if !matches!(value, Variant::Void) => match value.object_handle() {
            Some(dictionary) => dictionary_bounds(runtime, dictionary),
            None => Bounds::DEFAULT,
        },
        _ => stat_bounds(&exposure.sums),
    };

    let Some((width, height)) = image_size(runtime, layer) else {
        return Err(TjsError::runtime("LongExposure.copy: invalid layer image"));
    };
    if (width, height) != (exposure.width, exposure.height) {
        return Err(TjsError::runtime("LongExposure.copy: invalid layer size"));
    }
    // `GetLayerImageForWrite` (`:113-116`) — unreachable after the size probe,
    // as in `snap`.
    if layer_bitmap_write(runtime, layer, |view| {
        write_exposure(exposure, view, &bounds)
    })
    .is_err()
    {
        return Err(TjsError::runtime("LongExposure.copy: invalid layer image"));
    }
    Ok(Variant::Void)
}

/// `termExposure()` (`Main.cpp:152-156`): frees the buffer. A layer that never
/// initialized has nothing to free and no error.
fn layer_term_exposure(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(runtime, this_obj)?;
    if let Some(slot) = runtime.host().layer_extension::<ExposureSlot>(layer) {
        *lock(&slot) = None;
    }
    Ok(Variant::Void)
}

/// `snap`'s inner loop (`Main.cpp:30-39`): every channel byte adds into its own
/// 32-bit sum, wrapping like the reference's `DWORD` on overflow.
fn accumulate(exposure: &mut Exposure, view: &LayerBitmapView<'_>) {
    let width = exposure.width as usize;
    let pitch = view.bitmap.pitch as usize;
    for y in 0..exposure.height as usize {
        for x in 0..width {
            let source = y * pitch + x * 4;
            let destination = (y * width + x) * 4;
            if let (Some(bytes), Some(sums)) = (
                view.pixels.get(source..source + 4),
                exposure.sums.get_mut(destination..destination + 4),
            ) {
                for (sum, byte) in sums.iter_mut().zip(bytes) {
                    *sum = sum.wrapping_add(u32::from(*byte));
                }
            }
        }
    }
}

/// `copy`'s inner loop (`Main.cpp:140-149`): the normalized sums back into the
/// layer plane, channel for channel.
fn write_exposure(exposure: &Exposure, view: &mut LayerBitmapViewMut<'_>, bounds: &Bounds) {
    let width = exposure.width as usize;
    let pitch = view.bitmap.pitch as usize;
    for y in 0..exposure.height as usize {
        for x in 0..width {
            let destination = y * pitch + x * 4;
            let source = (y * width + x) * 4;
            if let (Some(sums), Some(bytes)) = (
                exposure.sums.get(source..source + 4),
                view.pixels.get_mut(destination..destination + 4),
            ) {
                for (channel, (byte, sum)) in bytes.iter_mut().zip(sums).enumerate() {
                    *byte = normalize(*sum, bounds.min[channel], bounds.max[channel]);
                }
            }
        }
    }
}

/// The reference's `MinMaxRGBA` (`Main.cpp:42-67`): one min/max pair per
/// channel. The constructor's defaults — min `0xFFFFFFFF`, max `0` (`:47`) —
/// are the state `copy` starts from for a non-object argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Bounds {
    min: [u32; 4],
    max: [u32; 4],
}

impl Bounds {
    const DEFAULT: Self = Self {
        min: [u32::MAX; 4],
        max: [0; 4],
    };
}

/// `MinMaxRGBA::setMinMax`/`_stat` (`Main.cpp:57-60, :68-79`) over every
/// channel of every pixel.
fn stat_bounds(sums: &[u32]) -> Bounds {
    let mut bounds = Bounds::DEFAULT;
    for pixel in sums.chunks_exact(4) {
        for (channel, &value) in pixel.iter().enumerate() {
            if value < bounds.min[channel] {
                bounds.min[channel] = value;
            }
            if value > bounds.max[channel] {
                bounds.max[channel] = value;
            }
        }
    }
    bounds
}

/// `MinMaxRGBA::getNormalize` (`Main.cpp:61-66`).
///
/// `min >= max` answers 0xFF; otherwise the clamped `(n - min) * 255 /
/// (max - min)` in the reference's 32-bit unsigned arithmetic — the multiply
/// wraps, which only bites once a channel has accumulated past ~16.8 million.
fn normalize(n: u32, min: u32, max: u32) -> u8 {
    if min >= max {
        return 0xFF;
    }
    let n = n.clamp(min, max);
    (n.wrapping_sub(min).wrapping_mul(255) / (max - min)) as u8
}

/// `copy`'s dictionary path (`Main.cpp:124-138`): `ncbPropAccessor::getIntValue`
/// per member — a member the dictionary does not have reads 0 — and the value
/// truncated into the reference's `DWORD`.
fn dictionary_bounds(runtime: &mut Runtime<KrkrHost>, dictionary: ObjectHandle) -> Bounds {
    let mut bounds = Bounds::DEFAULT;
    for (channel, (min, max)) in [
        ("min_r", "max_r"),
        ("min_g", "max_g"),
        ("min_b", "max_b"),
        ("min_a", "max_a"),
    ]
    .into_iter()
    .enumerate()
    {
        bounds.min[channel] = dictionary_integer(runtime, dictionary, min) as u32;
        bounds.max[channel] = dictionary_integer(runtime, dictionary, max) as u32;
    }
    bounds
}

/// `ncbPropAccessor::getIntValue` (`savepng.cpp:131-169`): a member that does
/// not exist reads 0, and so does a value the integer conversion refuses.
fn dictionary_integer(
    runtime: &mut Runtime<KrkrHost>,
    dictionary: ObjectHandle,
    name: &str,
) -> i64 {
    if !runtime.has_object_member(dictionary, name) {
        return 0;
    }
    runtime
        .resolve_object_member(dictionary, name)
        .ok()
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(0)
}

/// The reference's `GetLayerSize` (`Main.cpp:163-176`): `hasImage` plus a
/// positive `imageWidth`/`imageHeight`. A layer without a main image refuses a
/// bitmap read, which is the same condition, so the caller reports the
/// reference's "invalid layer image".
fn image_size(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Option<(u32, u32)> {
    let size = layer_bitmap_read(runtime, layer, |view| {
        (view.bitmap.width, view.bitmap.height)
    })
    .ok()?;
    (size.0 > 0 && size.1 > 0).then_some(size)
}

fn this_layer(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Result<ObjectHandle> {
    let layer = this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))?;
    // The slot is keyed by object, so a bound method acts on the layer it binds.
    Ok(runtime.bound_this(layer).unwrap_or(layer))
}

/// A poisoned lock means a hook panicked while holding it; the accumulator is
/// still consistent, so it is taken back rather than panicking again.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Registers `function` unless a script already owns the member, the way the
/// rest of the family's ports attach their surface.
fn register_unless_closure(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &'static str,
    arg_count: NativeArgCount,
    function: impl NativeFunction<KrkrHost> + 'static,
) {
    if matches!(runtime.object_member(object, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native_with_arg_count(object, name, arg_count, function);
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::{ObjectHandle, Variant};

    use super::{ExposureSlot, LayerExLongExposurePlugin};

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(LayerExLongExposurePlugin)
            .expect("plugin");
        engine
    }

    /// The five members exist on the class, and a call shorter than the
    /// reference's declaration is `TJS_E_BADPARAMCOUNT` (`ncbind.hpp:1186`).
    #[test]
    fn the_five_members_are_registered_with_the_reference_arity() {
        let class_engine = engine();
        let layer = layer_class(&class_engine);
        for name in [
            "initExposure",
            "snapExposure",
            "statExposure",
            "copyExposure",
            "termExposure",
        ] {
            assert!(
                is_callable_member(&class_engine, layer, name),
                "Layer.{name} is registered"
            );
        }

        let mut engine = engine();
        engine
            .execute_script(
                "setup.tjs",
                "global.layer = new Layer(); layer.setImageSize(1, 1); layer.fillRect(0, 0, 1, 1, 0x80000000);",
            )
            .expect("layer");
        // `copy(tTJSVariant v)` declares one parameter (`Main.cpp:104`).
        let error = engine
            .execute_script("arity.tjs", "layer.copyExposure();")
            .expect_err("no argument");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        // The zero-argument members take a bare call, and ncbind ignores
        // arguments past the declared count.
        engine
            .execute_script(
                "arity.tjs",
                "layer.initExposure(); layer.snapExposure(); \
                 layer.copyExposure(layer.statExposure(), 1, 2); layer.termExposure();",
            )
            .expect("the reference's argument counts");
    }

    /// `Main.cpp:30-39` sums one 32-bit accumulator per channel and `:42-102`
    /// takes each channel's min/max. Two snapped frames give the sums
    /// p0 (30,60,90,120), p1 (150,180,210,240), p2 (300,330,360,385), so
    /// `copy` (`:104-150`) writes p0 (0,0,0,0), p1 (113,113,113,115) and p2
    /// (255,255,255,255) by hand from `(n-min)*255/(max-min)`.
    #[test]
    fn two_snaps_accumulate_and_copy_normalizes_each_channel() {
        let mut engine = engine();
        engine
            .execute_script(
                "two.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(3, 1);
                // Frame 1 (R,G,B,A): (10,20,30,40) (50,60,70,80) (100,110,120,130).
                layer.fillRect(0, 0, 1, 1, 0x280a141e);
                layer.fillRect(1, 0, 1, 1, 0x50323c46);
                layer.fillRect(2, 0, 1, 1, 0x82646e78);
                layer.initExposure();
                layer.snapExposure();
                // Frame 2: (20,40,60,80) (100,120,140,160) (200,220,240,255).
                layer.fillRect(0, 0, 1, 1, 0x5014283c);
                layer.fillRect(1, 0, 1, 1, 0xa064788c);
                layer.fillRect(2, 0, 1, 1, 0xffc8dcf0);
                layer.snapExposure();
                global.stat = layer.statExposure();
                "#,
            )
            .expect("two snaps");

        for (member, value) in [
            ("min_r", 30),
            ("max_r", 300),
            ("min_g", 60),
            ("max_g", 330),
            ("min_b", 90),
            ("max_b", 360),
            ("min_a", 120),
            ("max_a", 385),
        ] {
            assert_eq!(stat_member(&mut engine, member), value, "stat.{member}");
        }

        // `copyExposure(void)` is the implicit `statExposure` (`:120-123`).
        engine
            .execute_script("copy.tjs", "layer.copyExposure(void);")
            .expect("copy");
        assert_eq!(main_pixel(&mut engine, 0, 0), 0x000000);
        assert_eq!(mask_pixel(&mut engine, 0, 0), 0);
        assert_eq!(main_pixel(&mut engine, 1, 0), 0x717171);
        assert_eq!(mask_pixel(&mut engine, 1, 0), 0x73);
        assert_eq!(main_pixel(&mut engine, 2, 0), 0xffffff);
        assert_eq!(mask_pixel(&mut engine, 2, 0), 0xff);

        // A non-object argument keeps the constructor's default min
        // `0xFFFFFFFF`/max 0 (`:47`), and `min >= max` answers 0xFF (`:62`):
        // every byte whitens.
        engine
            .execute_script("copy.tjs", "layer.copyExposure(42);")
            .expect("default bounds");
        for x in 0..3i64 {
            assert_eq!(main_pixel(&mut engine, x, 0), 0xffffff, "pixel {x}");
            assert_eq!(mask_pixel(&mut engine, x, 0), 0xff, "pixel {x}");
        }

        // `copy` reads the sums without clearing them (`:140-149`), so the
        // explicit dictionary form (`:124-138`) reproduces the implicit one.
        engine
            .execute_script("copy.tjs", "layer.copyExposure(stat);")
            .expect("stat dictionary");
        assert_eq!(main_pixel(&mut engine, 1, 0), 0x717171);
        assert_eq!(mask_pixel(&mut engine, 1, 0), 0x73);
    }

    /// Three frames of one flat value per pixel: the sums are 765, 300 and 450,
    /// so the accumulator held values past 255 — 32-bit sums, neither bytes
    /// that wrap nor a saturating add (`Main.cpp:30-39`). `copy` maps that
    /// range to 0..255: (765-300)*255/465 = 255, 0 and
    /// (450-300)*255/465 = 82.
    #[test]
    fn three_snaps_keep_channel_sums_past_255() {
        let mut engine = engine();
        engine
            .execute_script(
                "three.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(3, 1);
                layer.initExposure();
                for (var i = 0; i < 3; i++) {
                    layer.fillRect(0, 0, 1, 1, 0xffffffff);
                    layer.fillRect(1, 0, 1, 1, 0x64646464);
                    layer.fillRect(2, 0, 1, 1, 0x96969696);
                    layer.snapExposure();
                }
                global.stat = layer.statExposure();
                layer.copyExposure(void);
                "#,
            )
            .expect("three snaps");

        assert_eq!(stat_member(&mut engine, "min_r"), 300);
        assert_eq!(stat_member(&mut engine, "max_r"), 765);
        assert_eq!(stat_member(&mut engine, "max_a"), 765);

        assert_eq!(main_pixel(&mut engine, 0, 0), 0xffffff);
        assert_eq!(mask_pixel(&mut engine, 0, 0), 0xff);
        assert_eq!(main_pixel(&mut engine, 1, 0), 0x000000);
        assert_eq!(mask_pixel(&mut engine, 1, 0), 0);
        assert_eq!(main_pixel(&mut engine, 2, 0), 0x525252);
        assert_eq!(mask_pixel(&mut engine, 2, 0), 0x52);
    }

    /// The accumulator's lifetime: a second `init` frees the first sum
    /// (`Main.cpp:9`), `term` frees it (`:152-156`), and a failed `init`
    /// leaves the layer uninitialized rather than keeping an older buffer.
    #[test]
    fn init_resets_term_drops_and_a_failed_init_clears() {
        let mut engine = engine();
        engine
            .execute_script(
                "reset.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(1, 1);
                layer.fillRect(0, 0, 1, 1, 0x64646464);      // 100 per channel
                layer.initExposure();
                layer.snapExposure();
                layer.fillRect(0, 0, 1, 1, 0xc8c8c8c8);      // 200 per channel
                layer.initExposure();                        // `term()` first: the 100 is gone
                layer.snapExposure();
                global.stat = layer.statExposure();
                layer.copyExposure(void);
                "#,
            )
            .expect("re-init");

        // 200, not 300: the re-init dropped the first snap. And min == max
        // answers 0xFF per channel (`:62`).
        assert_eq!(stat_member(&mut engine, "min_r"), 200);
        assert_eq!(stat_member(&mut engine, "max_r"), 200);
        assert_eq!(main_pixel(&mut engine, 0, 0), 0xffffff);
        assert_eq!(mask_pixel(&mut engine, 0, 0), 0xff);

        engine
            .execute_script("term.tjs", "layer.termExposure(); layer.termExposure();")
            .expect("term is idempotent");
        for (call, message) in [
            (
                "layer.snapExposure();",
                "LongExposure.snap: not initialized",
            ),
            (
                "layer.statExposure();",
                "LongExposure.stat: not initialized",
            ),
            (
                "layer.copyExposure(void);",
                "LongExposure.copy: not initialized",
            ),
        ] {
            let error = engine.execute_script("term.tjs", call).expect_err(call);
            assert_eq!(error.message, message, "{call}");
        }

        // `init` after `term` works again; a failed `init` (no drawable image)
        // drops the buffer it had (`Main.cpp:9-11`).
        engine
            .execute_script("again.tjs", "layer.initExposure(); layer.snapExposure();")
            .expect("re-init after term");
        engine
            .execute_script("free.tjs", "layer.freeImage();")
            .expect("freeImage");
        let error = engine
            .execute_script("free.tjs", "layer.initExposure();")
            .expect_err("no image");
        assert_eq!(error.message, "LongExposure.init: invalid layer image");
        let error = engine
            .execute_script("free.tjs", "layer.snapExposure();")
            .expect_err("the failed init cleared the buffer");
        assert_eq!(error.message, "LongExposure.snap: not initialized");
    }

    /// A resize between `init` and `snap`/`copy` is "invalid layer size"
    /// (`Main.cpp:23-24, :110-111`); a fresh `init` takes the new size.
    #[test]
    fn snap_and_copy_reject_a_resized_layer() {
        let mut engine = engine();
        engine
            .execute_script(
                "grow.tjs",
                "global.layer = new Layer(); layer.setImageSize(2, 2); \
                 layer.initExposure(); layer.snapExposure(); layer.setImageSize(3, 3);",
            )
            .expect("grow");
        for (call, message) in [
            (
                "layer.snapExposure();",
                "LongExposure.snap: invalid layer size",
            ),
            (
                "layer.copyExposure(void);",
                "LongExposure.copy: invalid layer size",
            ),
        ] {
            let error = engine.execute_script("grow.tjs", call).expect_err(call);
            assert_eq!(error.message, message, "{call}");
        }
        engine
            .execute_script("grow.tjs", "layer.initExposure(); layer.snapExposure();")
            .expect("re-init at the new size");
    }

    /// The reference never looks at the clip box: `snap` and `copy` walk
    /// `width`/`height` from `GetLayerSize` across the whole image
    /// (`Main.cpp:30-39`, `:140-149`), so a clip rect is invisible here — a
    /// contrast with the rest of the family, whose base class moves the buffer
    /// to the clip origin.
    #[test]
    fn the_clip_box_is_ignored() {
        let mut engine = engine();
        engine
            .execute_script(
                "clip.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(2, 1);
                layer.fillRect(0, 0, 2, 1, 0x40404040);
                layer.setClip(0, 0, 1, 1);
                layer.initExposure();
                layer.snapExposure();
                layer.copyExposure(void);
                "#,
            )
            .expect("clipped snap and copy");
        // Both pixels were snapped, so the sums are equal and min == max
        // answers 0xFF. A clip-box walk would leave pixel 1 at 0.
        for x in 0..2i64 {
            assert_eq!(main_pixel(&mut engine, x, 0), 0xffffff, "pixel {x}");
            assert_eq!(mask_pixel(&mut engine, x, 0), 0xff, "pixel {x}");
        }
    }

    /// `statExposure` reads only the buffer (`Main.cpp:81-86`) — the layer's
    /// image is not part of it — while `copy` needs the same-sized image back
    /// (`:107-116`). The accumulator outlives `freeImage` and dies only with
    /// the object.
    #[test]
    fn stat_survives_a_freed_image_and_copy_does_not() {
        let mut engine = engine();
        engine
            .execute_script(
                "freed.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(1, 1);
                layer.fillRect(0, 0, 1, 1, 0x30303030);
                layer.initExposure();
                layer.snapExposure();
                layer.freeImage();
                global.stat = layer.statExposure();
                "#,
            )
            .expect("stat without an image");
        assert_eq!(stat_member(&mut engine, "max_r"), 0x30);
        let error = engine
            .execute_script("freed.tjs", "layer.copyExposure(void);")
            .expect_err("copy without an image");
        assert_eq!(error.message, "LongExposure.copy: invalid layer image");
    }

    /// `copy`'s dictionary path (`Main.cpp:124-138`): a member the dictionary
    /// does not have reads 0 through `ncbPropAccessor::getIntValue`, and a
    /// channel whose `min >= max` answers 0xFF.
    #[test]
    fn copy_reads_the_clamp_dictionary_member_by_member() {
        let mut engine = engine();
        engine
            .execute_script(
                "dict.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(1, 1);
                layer.fillRect(0, 0, 1, 1, 0xff6432c8);  // (R,G,B,A) = (100,50,200,255)
                layer.initExposure();
                layer.snapExposure();

                global.clamp = new Dictionary();
                clamp.min_r = 0;    clamp.max_r = 200;   // (100-0)*255/200 = 127
                clamp.min_g = 220;  clamp.max_g = 255;   // the sum 50 clamps to 220 -> 0
                // min_b/max_b are absent: 0 >= 0 answers 0xFF
                clamp.min_a = 0;    clamp.max_a = 255;   // 255
                layer.copyExposure(clamp);
                "#,
            )
            .expect("copy with a dictionary");

        assert_eq!(main_pixel(&mut engine, 0, 0), 0x7f00ff);
        assert_eq!(mask_pixel(&mut engine, 0, 0), 0xff);
    }

    /// The accumulator is the reference's per-object native instance
    /// (`Main.cpp:212-221`): each layer has its own, a layer that never ran
    /// `init` answers "not initialized", and `invalidate` destroys the state
    /// with the object (`~LongExposure`, `:5`).
    #[test]
    fn each_layer_keeps_its_own_accumulator_until_it_is_invalidated() {
        let mut engine = engine();
        engine
            .execute_script(
                "two_layers.tjs",
                r#"
                global.first = new Layer();
                first.setImageSize(1, 1);
                first.fillRect(0, 0, 1, 1, 0x3f3f3f3f);
                first.initExposure();
                first.snapExposure();

                global.second = new Layer();
                second.setImageSize(1, 1);
                second.fillRect(0, 0, 1, 1, 0x10101010);
                second.initExposure();
                second.snapExposure();

                global.third = new Layer();
                third.setImageSize(1, 1);
                "#,
            )
            .expect("layers");

        engine
            .execute_script("read.tjs", "global.stat = first.statExposure();")
            .expect("first stat");
        assert_eq!(stat_member(&mut engine, "max_r"), 0x3f);
        engine
            .execute_script("read.tjs", "global.stat = second.statExposure();")
            .expect("second stat");
        assert_eq!(stat_member(&mut engine, "max_r"), 0x10);

        let error = engine
            .execute_script("third.tjs", "third.snapExposure();")
            .expect_err("uninitialized layer");
        assert_eq!(error.message, "LongExposure.snap: not initialized");

        let first = layer_handle(&engine, "first");
        engine
            .execute_script("invalidate.tjs", "invalidate first;")
            .expect("invalidate");
        assert!(
            engine
                .host()
                .layer_extension::<ExposureSlot>(first)
                .is_none(),
            "the accumulator is pruned with the layer"
        );
        engine
            .execute_script("read.tjs", "global.stat = second.statExposure();")
            .expect("the second layer keeps its state");
        assert_eq!(stat_member(&mut engine, "max_r"), 0x10);
    }

    /// The reference never calls `Layer.update()` (there is no `redraw()` in
    /// `Main.cpp`), so `copy` does not raise `callOnPaint` either — the bytes
    /// are already readable through the bitmap, and a script that wants the
    /// repaint calls `Layer.update()` itself.
    #[test]
    fn copy_commits_without_posting_a_repaint() {
        let mut engine = engine();
        engine
            .execute_script(
                "paint.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(1, 1);
                layer.fillRect(0, 0, 1, 1, 0x20202020);
                layer.initExposure();
                layer.snapExposure();
                layer.callOnPaint = 0;
                layer.copyExposure(void);
                global.pending = layer.callOnPaint;
                "#,
            )
            .expect("copy");
        assert_eq!(integer(&mut engine, "pending"), 0);
    }

    /// `MinMaxRGBA::getNormalize` (`Main.cpp:61-66`) against hand-computed
    /// values, including its 32-bit wrapping multiply: a channel sum past 255
    /// is normal business, and one past `0xFFFFFFFF / 255` wraps the product
    /// before the division, exactly as the reference's `DWORD` arithmetic does.
    #[test]
    fn normalize_matches_the_reference_arithmetic() {
        // Ordinary ranges.
        assert_eq!(super::normalize(150, 30, 300), 113);
        assert_eq!(super::normalize(30, 30, 300), 0);
        assert_eq!(super::normalize(300, 30, 300), 255);
        assert_eq!(super::normalize(240, 120, 385), 115);
        // The clamp, then the wrap: 20,000,000 * 255 mod 2^32 = 805,032,704,
        // and 805,032,704 / 32,000,000 = 25 (a non-wrapping multiply would say
        // 5,100,000,000 / 32,000,000 = 159).
        assert_eq!(super::normalize(20_000_000, 0, 32_000_000), 25);
        // `min >= max` is 0xFF, the default bounds included.
        assert_eq!(super::normalize(5, 5, 5), 0xFF);
        assert_eq!(super::normalize(5, 10, 0), 0xFF);
        assert_eq!(
            super::normalize(
                5,
                super::Bounds::DEFAULT.min[0],
                super::Bounds::DEFAULT.max[0]
            ),
            0xFF
        );
    }

    /// Registration follows the family's `register_unless_closure` discipline:
    /// a layer member a script already owns keeps answering, and the plugin
    /// never clobbers the rest of the global `Layer` surface.
    #[test]
    fn a_script_owned_member_survives_registration() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "pre.tjs",
                "global.called = 0; Layer.initExposure = function () { global.called = 1; };",
            )
            .expect("script member");
        engine
            .register_plugin(LayerExLongExposurePlugin)
            .expect("plugin");
        engine
            .execute_script(
                "call.tjs",
                "global.layer = new Layer(); layer.initExposure();",
            )
            .expect("call");
        assert_eq!(
            integer(&mut engine, "called"),
            1,
            "the script's member still answers"
        );
    }

    fn layer_class(engine: &KrkrEngine) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member("Layer")
            .object_handle()
            .expect("Layer class")
    }

    fn layer_handle(engine: &KrkrEngine, name: &str) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    /// Registered natives are stored as function objects and script functions
    /// as closures; both are callable members.
    fn is_callable_member(engine: &KrkrEngine, object: ObjectHandle, name: &str) -> bool {
        match engine.tjs_runtime().object_member(object, name) {
            Variant::Closure(_) => true,
            Variant::Object(handle) => engine.tjs_runtime().object_is_callable(handle),
            _ => false,
        }
    }

    fn stat_member(engine: &mut KrkrEngine, member: &str) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("stat.{member}"))
            .expect("stat member")
            .to_integer()
            .expect("integer")
    }

    fn integer(engine: &mut KrkrEngine, global: &str) -> i64 {
        engine
            .execute_expression("read.tjs", global)
            .expect("global")
            .to_integer()
            .expect("integer")
    }

    fn main_pixel(engine: &mut KrkrEngine, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("layer.getMainPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }

    fn mask_pixel(engine: &mut KrkrEngine, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("layer.getMaskPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }
}
