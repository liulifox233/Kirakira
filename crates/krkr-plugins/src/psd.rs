//! `psd.dll` — the `PSD` class and the `psd://` storage media.
//!
//! Reference: the krkr2 trunk `plugins/win32/psdfile/` (1 768 lines over
//! eight files plus the bundled `psdparse/` library, 3 904 lines; the
//! dossier's `docs/plugins/document-text-audio.md`, psd section). Two pieces
//! of the DLL are script-visible:
//!
//! * the global class `PSD` (`psdclass.cpp:825-901`,
//!   `NCB_REGISTER_CLASS(PSD)`), and
//! * a storage media named `psd` (`main.cpp:259-266`), so
//!   `psd://<file>/root/<folder>/…/<layer>.bmp` and
//!   `psd://<file>/id/<layerId>.bmp` resolve to in-memory 32-bit BMPs
//!   (`psdclass.cpp:596-619`, `:761-822`).
//!
//! Surface, verified line by line against the source:
//!
//! * Constants: `color_mode_*` 0-9 (`:829-836`), `blend_mode_*` -1..27
//!   (`:838-867`), `layer_type_*` 0-5 (`:870-875`).
//! * `load(filename) → bool` (`:131-157`): loads through the storage stack
//!   (`TVPGetPlacedPath`), registering the object as the media's weak
//!   reference for the file's basename when it succeeds.
//! * read-only `width/height/channels/depth/color_mode/layer_count`
//!   (`:52-59`): **-1** while nothing is loaded.
//! * `getLayerType(no)`, `getLayerName(no)` (`:211-228`); the name prefers
//!   the `luni` Unicode name over the Pascal name (`:194-204`).
//! * `getLayerInfo(no) → Dictionary` (`:235-321`): `top,left,bottom,right,
//!   width,height,opacity,mask,type` (the KRKR blend op through
//!   `convBlendMode`, `:10-54`), `layer_type,blend_mode,visible,name`,
//!   `clipping,layer_id,obsolete,transparency_protected,
//!   pixel_data_irrelevant`, plus `layer_comp` when the layer has comps and
//!   `group_layer_id` when it is inside a folder.
//! * `getLayerData(layer, no)`, `getLayerDataRaw`, `getLayerDataMask`
//!   (`:329-400`): the target must be a `Layer` instance
//!   (`not layer`), the layer index is checked (`no data` / `not such
//!   layer`), only normal layers load — a folder only in mask mode
//!   (`invalid layer type`). The target's `left/top/width/height/opacity/
//!   type/visible/imageLeft/imageTop/imageWidth/imageHeight/name` are set and
//!   the pixels are written through `mainImageBufferForWrite`. A zero-sized
//!   mask becomes a 1×1 image filled with `defaultMaskColor` and alpha 255
//!   (`:356-360`, `:394-396`); a zero-sized layer is skipped silently
//!   (`:369-372`).
//! * `getBlend(layer) → bool` (`:526-542`): false when the file carries no
//!   merged image, else the composited image resizes the target to the
//!   document size and is written.
//! * `getSlices()`, `getGuides()`, `getLayerComp()` (`:444-593`):
//!   dictionaries (or `void` when the resource is absent); `no data` before a
//!   load. `clearStorageCache()` drops the media's cached document
//!   (`main.cpp:248-254`).
//! * Storage: `CheckExistentStorage` (present in the layer maps, or the `id/`
//!   form), `Open` (BMP, read-only — anything else throws
//!   `%1:cannot open psdfile`, `main.cpp:124`), `GetListAt` (`.bmp` names per
//!   directory), and `invalid path:%1` when the name has no `/`
//!   (`main.cpp:173`). Paths are lowercased; `/` inside a layer name became
//!   `_` (`psdclass.cpp:596-619`); a name collision keeps the
//!   bottom-most layer, the reference's reverse registration order.
//!
//! # How the port parses
//!
//! The container grammar (`psdparse.h:206-229`, `:236-275`, `:596-627`) is
//! re-implemented over a byte cursor:
//!
//! * signature `8BPS`, u16 version (**parsed and ignored** — the reference
//!   has no PSB branch, so version-2 files misparse there too), six reserved
//!   bytes, u16 channels, u32 height, u32 width (**height first**), u16
//!   depth, u16 mode;
//! * colour-mode data (u32 size + bytes), image resources (u32 size + the
//!   `8BIM`-tagged list), the layer-and-mask block (u32 size + layer info +
//!   global mask info + the optional `8BIM Lr16/Lr32` repeat) and the optional
//!   merged image that is the rest of the file (`psdparse.h:596-627`);
//! * layer records (`psdparse.h:428-451`): i16 count (negative sets the
//!   unused `mergedAlpha` flag), four u32 bounds, u16 channel count,
//!   `(i16 id, u32 length)` channels, `8BIM`, the blend-mode key, opacity,
//!   clipping, flags, filler, then the extra data (mask record, blending
//!   ranges, a padded Pascal name, then `8BIM`/`8B64` additional blocks).
//!   Channel image data follows the records inside the same block; a
//!   channel's byte offset accumulates its declared length across layers and
//!   channels (`psdparse.cpp:148-163`).
//! * Layer facts: flags (transparency-protected bit 0, visible bit 1
//!   inverted, obsolete bit 2, pixel-data-irrelevant bit 4,
//!   `psddata.h:409-413`); layer type from `lsct`/`lsdk` (1/2 folder, 3
//!   hidden section divider, `psdlayer.cpp:7-31`), from the adjust/fill
//!   additional keys (`psdparse.cpp:176-203`), else normal; `luni` name
//!   (`psdlayer.cpp:33-37`); `lyid` id; group linkage by the reverse pass
//!   over folder/hidden dividers (`psdparse.cpp:272-288`).
//! * Channel decoding (`psdimage.cpp:675-940`): per-channel u16 compression —
//!   0 raw, 1 PackBits RLE (hand-ported from `psdimage.cpp:511-549`), 2 zlib,
//!   3 zlib with the 16/32-bit predictor (`psdimage.cpp:553-656`); any other
//!   id fills `0xff`. Channel roles by id (-1 alpha, -2/-3 mask); image
//!   modes: image, image+mask merged into alpha (`mergeMaskToAlpha`,
//!   `psdimage.cpp:399-505`), mask only (grayscale, mask geometry).
//! * Composition to 32-bit pixels per colour mode (`psdimage.cpp:221-397`):
//!   bitmap (bit 7 first, black is opaque), grayscale, indexed (the 768-byte
//!   colour table, `psdparse.cpp:128-145`), RGB and CMYK (the reference's
//!   `invK` formula, `psdimage.cpp:142-174`). Multichannel/duotone/Lab write
//!   no pixels, exactly like the reference. 16-bit samples use the top byte,
//!   32-bit ones a big-endian float × 255.
//!
//! # Documented divergences
//!
//! * **Layer comps are not parsed.** The descriptor format behind
//!   `getLayerComp`, the document resource 1065 and the per-layer `shmd`
//!   `cmls` block is not implemented, so `getLayerComp()` returns `void`,
//!   `getLayerInfo` omits `layer_comp`, and a document whose 1065 resource
//!   the reference would *reject* (its `loadResourceLayerComps` failure
//!   fails the whole load, `psdparse.cpp:290`) loads here.
//! * **The parser is strict where the reference tolerates truncation**: a
//!   structurally short file fails the parse (the reference's PEG needs exact
//!   full consumption too, but its integer readers clamp instead of failing).
//! * **Pixel output order**: the reference writes BGRA_LE (memory bytes
//!   B,G,R,A) into `mainImageBufferForWrite`; Kirakira's layer plane is
//!   RGBA, so the same colour values are written in the engine's byte order.
//! * **Destination-layer properties**: the engine exposes no plugin path that
//!   runs a native layer property's setter, so `left/top/width/height`
//!   go through `Layer.setPos(left, top, width, height)` and
//!   `imageLeft/imageTop/imageWidth/imageHeight` through
//!   `Layer.setImagePos` + `Layer.setImageSize` (both engine methods that
//!   update the render layer). `opacity`, `type`, `visible` and `name` are
//!   written as plain members, which reads back correctly but does not reach
//!   the render layer until the game assigns them through the property.
//! * The `8BIM Lr16/Lr32` layer-info repeat is mirrored faithfully *including*
//!   the reference's append-a-second-copy behaviour, so 16/32-bit documents
//!   report twice as many layers and mis-addressed channel data exactly as
//!   they do there.

use std::{
    collections::BTreeMap,
    io::{self, Read},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use flate2::read::ZlibDecoder;
use krkr_engine::{KrkrHost, KrkrPlugin, plugin_api};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{Closure, NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "PSD (Photoshop document) loading",
    notes: "A port of psdfile: the PSD class (all constants, load, the six \
            read-only dimensions, getLayerType/Name/Info/Data/Raw/Mask, \
            getSlices, getGuides, getBlend, clearStorageCache) and the `psd` \
            storage media. The container, layer records, RLE/zlib channel \
            decoding and the per-colour-mode composition are ported from \
            psdparse. Documented gaps: layer comps (getLayerComp, the \
            layer_comp field, resource 1065) return void; opacity/type/visible/\
            name are written as plain members (no render-layer update); layer \
            pixel writes use the engine's RGBA plane instead of the \
            reference's BGRA buffer.",
    install: |engine| engine.register_plugin(PsdPlugin::default()),
};

// ---------------------------------------------------------------------------
// Constants — the reference's class registration (`psdclass.cpp:825-901`)
// ---------------------------------------------------------------------------

const COLOR_MODE_BITMAP: i64 = 0;
const COLOR_MODE_GRAYSCALE: i64 = 1;
const COLOR_MODE_INDEXED: i64 = 2;
const COLOR_MODE_RGB: i64 = 3;
const COLOR_MODE_CMYK: i64 = 4;
const COLOR_MODE_MULTICHANNEL: i64 = 7;
const COLOR_MODE_DUOTONE: i64 = 8;
const COLOR_MODE_LAB: i64 = 9;

const LAYER_TYPE_NORMAL: i64 = 0;
const LAYER_TYPE_HIDDEN: i64 = 1;
const LAYER_TYPE_FOLDER: i64 = 2;
const LAYER_TYPE_ADJUST: i64 = 3;
const LAYER_TYPE_FILL: i64 = 4;

const BLEND_MODE_INVALID: i64 = -1;

/// The `blend_mode_*` constant table (`psdclass.cpp:838-867`) and the
/// `lxBlendMode` values `getLayerInfo`'s `type` field converts into
/// (`convBlendMode`, `:10-54`).
const BLEND_MODES: &[(&str, i64, i64)] = &[
    ("blend_mode_normal", 0, 0),
    ("blend_mode_dissolve", 1, 0),
    ("blend_mode_darken", 2, 1),
    ("blend_mode_multiply", 3, 2),
    ("blend_mode_color_burn", 4, 3),
    ("blend_mode_linear_burn", 5, 4),
    ("blend_mode_lighten", 6, 5),
    ("blend_mode_screen", 7, 6),
    ("blend_mode_color_dodge", 8, 7),
    ("blend_mode_linear_dodge", 9, 8),
    ("blend_mode_overlay", 10, 9),
    ("blend_mode_soft_light", 11, 10),
    ("blend_mode_hard_light", 12, 11),
    ("blend_mode_vivid_light", 13, 0),
    ("blend_mode_linear_light", 14, 0),
    ("blend_mode_pin_light", 15, 0),
    ("blend_mode_hard_mix", 16, 0),
    ("blend_mode_difference", 17, 12),
    ("blend_mode_exclusion", 18, 13),
    ("blend_mode_hue", 19, 0),
    ("blend_mode_saturation", 20, 0),
    ("blend_mode_color", 21, 0),
    ("blend_mode_luminosity", 22, 0),
    ("blend_mode_pass_through", 23, 0),
    ("blend_mode_darker_color", 24, 0),
    ("blend_mode_lighter_color", 25, 0),
    ("blend_mode_subtract", 26, 0),
    ("blend_mode_divide", 27, 0),
];

const COLOR_MODE_CONSTANTS: &[(&str, i64)] = &[
    ("color_mode_bitmap", COLOR_MODE_BITMAP),
    ("color_mode_grayscale", COLOR_MODE_GRAYSCALE),
    ("color_mode_indexed", COLOR_MODE_INDEXED),
    ("color_mode_rgb", COLOR_MODE_RGB),
    ("color_mode_cmyk", COLOR_MODE_CMYK),
    ("color_mode_multichannel", COLOR_MODE_MULTICHANNEL),
    ("color_mode_duotone", COLOR_MODE_DUOTONE),
    ("color_mode_lab", COLOR_MODE_LAB),
];

const LAYER_TYPE_CONSTANTS: &[(&str, i64)] = &[
    ("layer_type_normal", LAYER_TYPE_NORMAL),
    ("layer_type_hidden", LAYER_TYPE_HIDDEN),
    ("layer_type_folder", LAYER_TYPE_FOLDER),
    ("layer_type_adjust", LAYER_TYPE_ADJUST),
    ("layer_type_fill", LAYER_TYPE_FILL),
];

/// The media name the reference registers (`GetName` assigns `L"psd"`,
/// `main.cpp:9`).
const MEDIA_NAME: &str = "psd";

// `PSD::load`'s private state: one parsed document per `PSD` object.
thread_local! {
    static DOCUMENTS: std::cell::RefCell<BTreeMap<ObjectHandle, Arc<PsdDocument>>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

fn document(handle: ObjectHandle) -> Option<Arc<PsdDocument>> {
    DOCUMENTS.with(|documents| documents.borrow().get(&handle).cloned())
}

fn set_document(handle: ObjectHandle, document: Option<Arc<PsdDocument>>) {
    DOCUMENTS.with(|documents| {
        let mut documents = documents.borrow_mut();
        match document {
            Some(document) => {
                documents.insert(handle, document);
            }
            None => {
                documents.remove(&handle);
            }
        }
    });
}

pub struct PsdPlugin {
    media: OnceLock<Arc<PsdMedia>>,
}

impl Default for PsdPlugin {
    fn default() -> Self {
        PsdPlugin {
            media: OnceLock::new(),
        }
    }
}

impl KrkrPlugin for PsdPlugin {
    fn name(&self) -> &str {
        "psd.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let media = self.media.get_or_init(|| Arc::new(PsdMedia::new())).clone();
        install_class(runtime, Arc::clone(&media));
        // `NCB_PRE_REGIST_CALLBACK(initStorage)` (`main.cpp:259-266`); a host
        // without project storage cannot address `psd://` at all, and that is
        // not a reason to fail its boot.
        match plugin_api::storage::register_storage_media(runtime, media) {
            Ok(()) => Ok(()),
            Err(error) if runtime.host().project_storage().is_err() => {
                runtime.host_mut().log(&format!(
                    "WARN psd: media `{MEDIA_NAME}` not registered: {error}"
                ));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        plugin_api::storage::unregister_storage_media(runtime, MEDIA_NAME);
        Ok(())
    }
}

fn install_class(runtime: &mut Runtime<KrkrHost>, media: Arc<PsdMedia>) {
    let constructor_media = Arc::clone(&media);
    let class = runtime.alloc_native_constructor(
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>|
              -> Result<Variant> {
            psd_constructor(runtime, this_obj, args, &constructor_media)
        },
    );
    runtime.add_object_class_info(class, "PSD");
    install_members(runtime, class, &media);
    runtime.set_object_member(
        class,
        "PSD",
        Variant::Closure(Closure::new(class, Some(class))),
    );
    runtime.set_global_member("PSD", Variant::Object(class));
}

fn psd_constructor(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
    media: &Arc<PsdMedia>,
) -> Result<Variant> {
    let instance = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(instance, "PSD");
    // `Factory` (`psdclass.cpp:110-115`): the members are the class's, and a
    // fresh object starts without data.
    install_members(runtime, instance, media);
    Ok(Variant::Object(instance))
}

fn install_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle, media: &Arc<PsdMedia>) {
    for (name, value) in COLOR_MODE_CONSTANTS {
        runtime.set_object_member(handle, *name, Variant::Integer(*value));
    }
    for (name, value) in LAYER_TYPE_CONSTANTS {
        runtime.set_object_member(handle, *name, Variant::Integer(*value));
    }
    for (name, value, _krkr) in BLEND_MODES {
        runtime.set_object_member(handle, *name, Variant::Integer(*value));
    }
    for (name, key) in [
        ("width", Dimension::Width),
        ("height", Dimension::Height),
        ("channels", Dimension::Channels),
        ("depth", Dimension::Depth),
        ("color_mode", Dimension::ColorMode),
        ("layer_count", Dimension::LayerCount),
    ] {
        runtime.register_object_native_property_with_access(
            handle,
            name,
            NativePropertyAccess::ReadOnly,
            move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                let Some(this) =
                    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                else {
                    return Ok(Variant::Integer(-1));
                };
                let value = document(this)
                    .map(|document| match key {
                        Dimension::Width => i64::from(document.header.width),
                        Dimension::Height => i64::from(document.header.height),
                        Dimension::Channels => i64::from(document.header.channels),
                        Dimension::Depth => i64::from(document.header.depth),
                        Dimension::ColorMode => i64::from(document.header.mode),
                        Dimension::LayerCount => document.layers.len() as i64,
                    })
                    // `INTGETTER` answers -1 while nothing is loaded
                    // (`psdclass.h:52-59`).
                    .unwrap_or(-1);
                Ok(Variant::Integer(value))
            },
            |_runtime: &mut Runtime<KrkrHost>, _this_obj, _value| Err(TjsError::access_denied()),
        );
    }
    runtime.register_object_native_with_arg_count(handle, "load", NativeArgCount::AtLeast(1), load);
    runtime.register_object_native_with_arg_count(
        handle,
        "getLayerType",
        NativeArgCount::AtLeast(1),
        get_layer_type,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getLayerName",
        NativeArgCount::AtLeast(1),
        get_layer_name,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getLayerInfo",
        NativeArgCount::AtLeast(1),
        get_layer_info,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getLayerData",
        NativeArgCount::AtLeast(2),
        get_layer_data,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getLayerDataRaw",
        NativeArgCount::AtLeast(2),
        get_layer_data_raw,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getLayerDataMask",
        NativeArgCount::AtLeast(2),
        get_layer_data_mask,
    );
    runtime.register_object_native(handle, "getSlices", get_slices);
    runtime.register_object_native(handle, "getGuides", get_guides);
    runtime.register_object_native_with_arg_count(
        handle,
        "getBlend",
        NativeArgCount::AtLeast(1),
        get_blend,
    );
    runtime.register_object_native(handle, "getLayerComp", get_layer_comp);
    let media = Arc::clone(media);
    // `static void clearStorageCache()` (`main.cpp:248-254`), registered as a
    // static method (`psdclass.cpp:899`).
    runtime.register_object_native(
        handle,
        "clearStorageCache",
        move |_runtime: &mut Runtime<KrkrHost>,
              _this: Option<ObjectHandle>,
              _args: Vec<Variant>|
              -> Result<Variant> {
            media.clear_cache();
            Ok(Variant::Void)
        },
    );
    // The crate's state-cleanup convention (`wf_basic_effect`); the reference
    // releases the same per-object data in `PSD::~PSD` (`psdclass.cpp:103-105`).
    runtime.register_object_native(
        handle,
        "finalize",
        |_runtime: &mut Runtime<KrkrHost>,
         this_obj: Option<ObjectHandle>,
         _args: Vec<Variant>|
         -> Result<Variant> {
            if let Some(handle) = this_obj {
                set_document(handle, None);
            }
            Ok(Variant::Void)
        },
    );
}

#[derive(Clone, Copy)]
enum Dimension {
    Width,
    Height,
    Channels,
    Depth,
    ColorMode,
    LayerCount,
}

fn plugin_this(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
}

/// `PSD::load` (`psdclass.cpp:131-157`).
fn load(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = plugin_this(runtime, this_obj) else {
        return Ok(Variant::Integer(0));
    };
    let Some(Variant::String(filename)) = args.first() else {
        return Ok(Variant::Integer(0));
    };
    let bytes = match runtime.host().read_binary_storage(filename) {
        Ok(bytes) => bytes,
        Err(_) => {
            set_document(this, None);
            return Ok(Variant::Integer(0));
        }
    };
    match parse_document(&bytes) {
        Ok(document) => {
            set_document(this, Some(Arc::new(document)));
            Ok(Variant::Integer(1))
        }
        Err(error) => {
            set_document(this, None);
            runtime
                .host_mut()
                .log(&format!("psd: failed to parse `{filename}`: {error}"));
            Ok(Variant::Integer(0))
        }
    }
}

/// `checkLayerNo` (`psdclass.cpp:179-188`): `no data`, then `not such layer`.
fn layer_no(document: &Option<Arc<PsdDocument>>, index: i64) -> Result<(Arc<PsdDocument>, usize)> {
    let Some(document) = document.clone() else {
        return Err(TjsError::runtime("no data"));
    };
    if index < 0 || index >= document.layers.len() as i64 {
        return Err(TjsError::runtime("not such layer"));
    }
    Ok((document, index as usize))
}

fn document_of(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<Arc<PsdDocument>> {
    plugin_this(runtime, this_obj).and_then(document)
}

fn argument_integer(args: &[Variant], index: usize) -> Result<i64> {
    match args.get(index) {
        Some(value) => value.to_integer(),
        None => Err(TjsError::bad_param_count()),
    }
}

/// `PSD::getLayerType` (`psdclass.cpp:211-216`).
fn get_layer_type(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let index = argument_integer(&args, 0)?;
    let (document, index) = layer_no(&document_of(runtime, this_obj), index)?;
    Ok(Variant::Integer(document.layers[index].layer_type))
}

/// `PSD::getLayerName` (`psdclass.cpp:218-228`).
fn get_layer_name(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let index = argument_integer(&args, 0)?;
    let (document, index) = layer_no(&document_of(runtime, this_obj), index)?;
    Ok(Variant::String(layer_name(&document.layers[index])))
}

/// `layname`: the Unicode name when present, else the Pascal name
/// (`psdclass.cpp:194-204`).
fn layer_name(layer: &Layer) -> String {
    if !layer.name_unicode.is_empty() {
        layer.name_unicode.clone()
    } else {
        layer.name.clone()
    }
}

/// `PSD::getLayerInfo` (`psdclass.cpp:235-321`).
fn get_layer_info(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let index = argument_integer(&args, 0)?;
    let (document, index) = layer_no(&document_of(runtime, this_obj), index)?;
    let layer = &document.layers[index];
    let dictionary = runtime.alloc_dictionary_object();
    for (name, value) in [
        ("top", layer.top as i64),
        ("left", layer.left as i64),
        ("bottom", layer.bottom as i64),
        ("right", layer.right as i64),
        ("width", layer.width as i64),
        ("height", layer.height as i64),
        ("opacity", layer.opacity as i64),
    ] {
        runtime.set_object_member(dictionary, name, Variant::Integer(value));
    }
    let mask = layer.channels.iter().any(|channel| channel.is_mask());
    runtime.set_object_member(dictionary, "mask", Variant::Integer(i64::from(mask)));
    runtime.set_object_member(
        dictionary,
        "type",
        Variant::Integer(krkr_blend_mode(layer.blend_mode)),
    );
    runtime.set_object_member(dictionary, "layer_type", Variant::Integer(layer.layer_type));
    runtime.set_object_member(dictionary, "blend_mode", Variant::Integer(layer.blend_mode));
    runtime.set_object_member(
        dictionary,
        "visible",
        Variant::Integer(i64::from(layer.is_visible())),
    );
    runtime.set_object_member(dictionary, "name", Variant::String(layer_name(layer)));
    runtime.set_object_member(
        dictionary,
        "clipping",
        Variant::Integer(i64::from(layer.clipping)),
    );
    runtime.set_object_member(
        dictionary,
        "layer_id",
        Variant::Integer(i64::from(layer.layer_id)),
    );
    runtime.set_object_member(
        dictionary,
        "obsolete",
        Variant::Integer(i64::from(layer.is_obsolete())),
    );
    runtime.set_object_member(
        dictionary,
        "transparency_protected",
        Variant::Integer(i64::from(layer.is_transparency_protected())),
    );
    runtime.set_object_member(
        dictionary,
        "pixel_data_irrelevant",
        Variant::Integer(i64::from(layer.is_pixel_data_irrelevant())),
    );
    // `layer_comp` is omitted: the descriptor-backed layer comps are not
    // parsed (see the module docs).
    if let Some(parent) = layer.parent {
        runtime.set_object_member(
            dictionary,
            "group_layer_id",
            Variant::Integer(i64::from(document.layers[parent].layer_id)),
        );
    }
    Ok(Variant::Object(dictionary))
}

/// `convBlendMode` (`psdclass.cpp:10-54`): the PSD blend mode as a KRKR
/// `Layer.type` value; unsupported modes fall back to normal.
fn krkr_blend_mode(blend_mode: i64) -> i64 {
    BLEND_MODES
        .iter()
        .find(|(_, value, _)| *value == blend_mode)
        .map(|(_, _, krkr)| *krkr)
        .unwrap_or(0)
}

#[derive(Clone, Copy, PartialEq)]
enum ImageMode {
    Image,
    MaskedImage,
    Mask,
}

fn get_layer_data(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    get_layer_image_call(runtime, this_obj, &args, ImageMode::MaskedImage)
}

fn get_layer_data_raw(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    get_layer_image_call(runtime, this_obj, &args, ImageMode::Image)
}

fn get_layer_data_mask(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    get_layer_image_call(runtime, this_obj, &args, ImageMode::Mask)
}

/// `PSD::_getLayerData` (`psdclass.cpp:329-400`).
fn get_layer_image_call(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: &[Variant],
    mode: ImageMode,
) -> Result<Variant> {
    let Some(first) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    // A non-object argument (including `null`) is the reference's failed
    // `Layer` test (`psdclass.cpp:333`).
    let Some(target) = first.object_handle() else {
        return Err(TjsError::runtime("not layer"));
    };
    if !is_layer(runtime, target) {
        return Err(TjsError::runtime("not layer"));
    }
    let index = argument_integer(args, 1)?;
    let (document, index) = layer_no(&document_of(runtime, this_obj), index)?;
    let layer = &document.layers[index];
    // A folder may only be loaded in mask mode (`psdclass.cpp:340-344`).
    if layer.layer_type != LAYER_TYPE_NORMAL
        && !(layer.layer_type == LAYER_TYPE_FOLDER && mode == ImageMode::Mask)
    {
        return Err(TjsError::runtime("invalid layer type"));
    }
    let (left, top, width, height, opacity, blend) = match mode {
        ImageMode::Mask => (
            layer.mask.left,
            layer.mask.top,
            layer.mask.width,
            layer.mask.height,
            255,
            0,
        ),
        _ => (
            layer.left,
            layer.top,
            layer.width,
            layer.height,
            layer.opacity,
            krkr_blend_mode(layer.blend_mode),
        ),
    };
    let mut dummy = false;
    let (left, top, width, height) = if mode == ImageMode::Mask && (width == 0 || height == 0) {
        // A zero-sized mask becomes a 1x1 image so the target layer can be
        // created at all (`psdclass.cpp:356-360`).
        dummy = true;
        (0, 0, 1, 1)
    } else {
        (left, top, width, height)
    };
    if width <= 0 || height <= 0 {
        // A zero-sized layer cannot be loaded (`psdclass.cpp:369-372`).
        return Ok(Variant::Void);
    }
    // The target's geometry first: `setPos` is the engine method that maps
    // `left/top/width/height` onto the render layer, `setImagePos` /
    // `setImageSize` the image rectangle.
    runtime.call_object_method(
        target,
        "setPos",
        vec![
            Variant::Integer(i64::from(left)),
            Variant::Integer(i64::from(top)),
            Variant::Integer(i64::from(width)),
            Variant::Integer(i64::from(height)),
        ],
    )?;
    runtime.call_object_method(
        target,
        "setImagePos",
        vec![Variant::Integer(0), Variant::Integer(0)],
    )?;
    runtime.call_object_method(
        target,
        "setImageSize",
        vec![
            Variant::Integer(i64::from(width)),
            Variant::Integer(i64::from(height)),
        ],
    )?;
    for (name, value) in [
        ("opacity", Variant::Integer(i64::from(opacity))),
        ("type", Variant::Integer(blend)),
        ("visible", Variant::Integer(i64::from(layer.is_visible()))),
        ("name", Variant::String(layer_name(layer))),
    ] {
        runtime.set_object_member(target, name, value);
    }
    if mode == ImageMode::Mask {
        runtime.set_object_member(
            target,
            "defaultMaskColor",
            Variant::Integer(i64::from(layer.mask.default_color)),
        );
    }
    let pixels = if dummy {
        let color = layer.mask.default_color;
        vec![color, color, color, 0xff]
    } else {
        match compose_layer_image(&document, layer, mode, width as usize, height as usize) {
            Some(pixels) => pixels,
            // `getLayerImage` failing leaves the buffer as it was
            // (`psdimage.cpp:795-940`): nothing is written.
            None => return Ok(Variant::Void),
        }
    };
    let width = width as u32;
    let height = height as u32;
    if let Err(error) = plugin_api::layer::layer_bitmap_write(runtime, target, |view| {
        if view.bitmap.width == width && view.bitmap.height == height {
            let row = (width as usize * 4).min(view.pixels.len());
            let source_row = width as usize * 4;
            for y in 0..height as usize {
                let destination = y * view.bitmap.pitch as usize;
                let source = y * source_row;
                if destination + row > view.pixels.len() || source + row > pixels.len() {
                    break;
                }
                view.pixels[destination..destination + row]
                    .copy_from_slice(&pixels[source..source + row]);
            }
        }
    }) {
        // A target without a main image (`Not drawable layer type` in the
        // reference's terms) cannot receive pixels; the property writes above
        // already happened, exactly like the reference's failed
        // `mainImageBufferForWrite` path.
        runtime
            .host_mut()
            .log(&format!("psd: layer image write failed: {error}"));
    }
    Ok(Variant::Void)
}

/// The reference hands `mainImageBufferForWrite` a `Layer`; Kirakira tags
/// every layer object with the class name.
fn is_layer(runtime: &Runtime<KrkrHost>, handle: ObjectHandle) -> bool {
    let mut current = Some(handle);
    while let Some(object) = current {
        if runtime
            .object_class_infos(object)
            .iter()
            .any(|info| info == "Layer")
        {
            return true;
        }
        current = runtime.object_super_class(object);
    }
    false
}

/// `PSD::getBlend` (`psdclass.cpp:526-542`).
fn get_blend(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(first) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    let Some(target) = first.object_handle() else {
        return Err(TjsError::runtime("not layer"));
    };
    if !is_layer(runtime, target) {
        return Err(TjsError::runtime("not layer"));
    }
    let Some(document) = document_of(runtime, this_obj) else {
        return Ok(Variant::Integer(0));
    };
    let Some(pixels) = compose_merged_image(&document) else {
        // `if (imageData)` (`psdclass.cpp:530`): a file without the merged
        // image section answers false.
        return Ok(Variant::Integer(0));
    };
    let width = document.header.width as i64;
    let height = document.header.height as i64;
    runtime.call_object_method(
        target,
        "setImagePos",
        vec![Variant::Integer(0), Variant::Integer(0)],
    )?;
    runtime.call_object_method(
        target,
        "setImageSize",
        vec![Variant::Integer(width), Variant::Integer(height)],
    )?;
    let _ = plugin_api::layer::layer_bitmap_write(runtime, target, |view| {
        let row = (document.header.width as usize * 4).min(view.pixels.len());
        for y in 0..document.header.height as usize {
            let destination = y * view.bitmap.pitch as usize;
            let source = y * document.header.width as usize * 4;
            if destination + row > view.pixels.len() || source + row > pixels.len() {
                break;
            }
            view.pixels[destination..destination + row]
                .copy_from_slice(&pixels[source..source + row]);
        }
    });
    Ok(Variant::Integer(1))
}

/// `PSD::getSlices` (`psdclass.cpp:444-489`).
fn get_slices(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(document) = document_of(runtime, this_obj) else {
        return Err(TjsError::runtime("no data"));
    };
    let Some(slice) = &document.slices else {
        return Ok(Variant::Void);
    };
    let dictionary = runtime.alloc_dictionary_object();
    runtime.set_object_member(
        dictionary,
        "top",
        Variant::Integer(i64::from(slice.bounding_top)),
    );
    runtime.set_object_member(
        dictionary,
        "left",
        Variant::Integer(i64::from(slice.bounding_left)),
    );
    runtime.set_object_member(
        dictionary,
        "bottom",
        Variant::Integer(i64::from(slice.bounding_bottom)),
    );
    runtime.set_object_member(
        dictionary,
        "right",
        Variant::Integer(i64::from(slice.bounding_right)),
    );
    runtime.set_object_member(
        dictionary,
        "name",
        Variant::String(slice.group_name.clone()),
    );
    let list = runtime.alloc_array_object(Vec::new());
    for item in &slice.items {
        let entry = runtime.alloc_dictionary_object();
        for (name, value) in [
            ("id", i64::from(item.id)),
            ("group_id", i64::from(item.group_id)),
            ("origin", i64::from(item.origin)),
            ("type", i64::from(item.kind)),
            ("left", i64::from(item.left)),
            ("top", i64::from(item.top)),
            ("right", i64::from(item.right)),
            ("bottom", i64::from(item.bottom)),
            (
                "color",
                (i64::from(item.color_a) << 24)
                    | (i64::from(item.color_r) << 16)
                    | (i64::from(item.color_g) << 8)
                    | i64::from(item.color_b),
            ),
            ("cell_text_is_html", i64::from(item.cell_text_is_html)),
            ("horizontal_alignment", i64::from(item.horizontal_alignment)),
            ("vertical_alignment", i64::from(item.vertical_alignment)),
            ("associated_layer_id", i64::from(item.associated_layer_id)),
        ] {
            runtime.set_object_member(entry, name, Variant::Integer(value));
        }
        for (name, value) in [
            ("name", item.name.clone()),
            ("url", item.url.clone()),
            ("target", item.target.clone()),
            ("message", item.message.clone()),
            ("alt_tag", item.alt_tag.clone()),
            ("cell_text", item.cell_text.clone()),
        ] {
            runtime.set_object_member(entry, name, Variant::String(value));
        }
        runtime.array_push(list, Variant::Object(entry));
    }
    runtime.set_object_member(dictionary, "slices", Variant::Object(list));
    Ok(Variant::Object(dictionary))
}

/// `PSD::getGuides` (`psdclass.cpp:499-521`).
fn get_guides(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(document) = document_of(runtime, this_obj) else {
        return Err(TjsError::runtime("no data"));
    };
    let Some(guide) = &document.guides else {
        return Ok(Variant::Void);
    };
    let dictionary = runtime.alloc_dictionary_object();
    runtime.set_object_member(
        dictionary,
        "horz_grid",
        Variant::Integer(i64::from(guide.horizontal_grid)),
    );
    runtime.set_object_member(
        dictionary,
        "vert_grid",
        Variant::Integer(i64::from(guide.vertical_grid)),
    );
    let vertical = runtime.alloc_array_object(Vec::new());
    let horizontal = runtime.alloc_array_object(Vec::new());
    for (location, direction) in &guide.guides {
        let value = Variant::Integer(i64::from(*location));
        if *direction == 0 {
            runtime.array_push(vertical, value);
        } else {
            runtime.array_push(horizontal, value);
        }
    }
    runtime.set_object_member(dictionary, "vertical", Variant::Object(vertical));
    runtime.set_object_member(dictionary, "horizontal", Variant::Object(horizontal));
    Ok(Variant::Object(dictionary))
}

/// `PSD::getLayerComp` (`psdclass.cpp:565-593`): the layer comp resources are
/// not parsed (see the module docs), so this always answers `void`.
fn get_layer_comp(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if document_of(runtime, this_obj).is_none() {
        return Err(TjsError::runtime("no data"));
    }
    Ok(Variant::Void)
}

// ---------------------------------------------------------------------------
// Document model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default, Debug)]
struct Header {
    version: i32,
    channels: i32,
    height: i32,
    width: i32,
    depth: i32,
    mode: i32,
}

#[derive(Clone, Debug)]
struct ChannelInfo {
    id: i32,
    offset: usize,
    length: u32,
}

impl ChannelInfo {
    /// `ChannelInfo::isMaskChannel` (`psddata.h:369`).
    fn is_mask(&self) -> bool {
        self.id == -2 || self.id == -3
    }
}

#[derive(Clone, Debug, Default)]
struct LayerMask {
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    width: i32,
    height: i32,
    default_color: u8,
}

#[derive(Clone, Debug)]
struct Layer {
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    width: i32,
    height: i32,
    channels: Vec<ChannelInfo>,
    blend_mode: i64,
    opacity: i32,
    clipping: i32,
    flag: i32,
    layer_id: i32,
    layer_type: i64,
    name: String,
    name_unicode: String,
    mask: LayerMask,
    parent: Option<usize>,
}

impl Default for Layer {
    fn default() -> Self {
        Layer {
            top: 0,
            left: 0,
            bottom: 0,
            right: 0,
            width: 0,
            height: 0,
            channels: Vec::new(),
            blend_mode: BLEND_MODE_INVALID,
            opacity: 255,
            clipping: 0,
            flag: 0,
            layer_id: -1,
            layer_type: LAYER_TYPE_NORMAL,
            name: String::new(),
            name_unicode: String::new(),
            mask: LayerMask::default(),
            parent: None,
        }
    }
}

impl Layer {
    fn is_visible(&self) -> bool {
        self.flag & (1 << 1) == 0
    }

    fn is_transparency_protected(&self) -> bool {
        self.flag & (1 << 0) != 0
    }

    fn is_obsolete(&self) -> bool {
        self.flag & (1 << 2) != 0
    }

    fn is_pixel_data_irrelevant(&self) -> bool {
        self.flag & (1 << 4) != 0
    }
}

#[derive(Clone, Debug, Default)]
struct SliceItem {
    id: i32,
    group_id: i32,
    origin: i32,
    associated_layer_id: i32,
    name: String,
    kind: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    url: String,
    target: String,
    message: String,
    alt_tag: String,
    cell_text_is_html: bool,
    cell_text: String,
    horizontal_alignment: i32,
    vertical_alignment: i32,
    color_a: u8,
    color_r: u8,
    color_g: u8,
    color_b: u8,
}

#[derive(Clone, Debug, Default)]
struct SliceResource {
    bounding_left: i32,
    bounding_top: i32,
    bounding_right: i32,
    bounding_bottom: i32,
    group_name: String,
    items: Vec<SliceItem>,
}

#[derive(Clone, Debug, Default)]
struct GridGuide {
    horizontal_grid: i32,
    vertical_grid: i32,
    guides: Vec<(i32, u8)>,
}

#[derive(Debug, Default)]
struct PsdDocument {
    header: Header,
    color_table: Vec<[u8; 4]>,
    transparency_index: i32,
    layers: Vec<Layer>,
    channel_data: Vec<u8>,
    merged: Option<Vec<u8>>,
    slices: Option<SliceResource>,
    guides: Option<GridGuide>,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ParseError(String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn fail<T>(message: &str) -> std::result::Result<T, ParseError> {
    Err(ParseError(message.to_string()))
}

struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.position
    }

    fn eof(&self) -> bool {
        self.remaining() == 0
    }

    fn bytes(&mut self, count: usize) -> std::result::Result<&'a [u8], ParseError> {
        if self.remaining() < count {
            return fail("unexpected end of data");
        }
        let slice = &self.data[self.position..self.position + count];
        self.position += count;
        Ok(slice)
    }

    fn skip(&mut self, count: usize) -> std::result::Result<(), ParseError> {
        self.bytes(count).map(|_| ())
    }

    fn u8(&mut self) -> std::result::Result<u8, ParseError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> std::result::Result<u16, ParseError> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn i16(&mut self) -> std::result::Result<i16, ParseError> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> std::result::Result<u32, ParseError> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32(&mut self) -> std::result::Result<i32, ParseError> {
        Ok(self.u32()? as i32)
    }

    fn literal(&mut self, expected: &[u8]) -> std::result::Result<(), ParseError> {
        let bytes = self.bytes(expected.len())?;
        if bytes != expected {
            return fail("unexpected signature");
        }
        Ok(())
    }

    /// A Unicode string: `int32` count then that many big-endian UTF-16 code
    /// units (`psdparse.h:181-187`).
    fn unicode_string(&mut self) -> std::result::Result<String, ParseError> {
        let count = self.u32()? as usize;
        let mut units = Vec::with_capacity(count);
        for _ in 0..count {
            units.push(self.u16()?);
        }
        Ok(String::from_utf16_lossy(&units))
    }
}

fn parse_document(data: &[u8]) -> std::result::Result<PsdDocument, ParseError> {
    let mut cursor = Cursor::new(data);
    let mut document = PsdDocument::default();
    cursor.literal(b"8BPS")?;
    document.header.version = i32::from(cursor.u16()?);
    cursor.skip(6)?;
    document.header.channels = i32::from(cursor.u16()?);
    // Height precedes width (`psdparse.h:221-222`).
    document.header.height = cursor.u32()? as i32;
    document.header.width = cursor.u32()? as i32;
    document.header.depth = i32::from(cursor.u16()?);
    document.header.mode = i32::from(cursor.u16()?);

    // Colour mode data.
    let color_mode_size = cursor.u32()? as usize;
    let color_mode = cursor.bytes(color_mode_size)?;

    // Image resources.
    let resource_size = cursor.u32()? as usize;
    let resources = parse_image_resources(cursor.bytes(resource_size)?)?;

    // Layer and mask information.
    let layer_and_mask_size = cursor.u32()? as usize;
    let layer_and_mask = cursor.bytes(layer_and_mask_size)?;
    if !layer_and_mask.is_empty() {
        parse_layer_and_mask(layer_and_mask, &mut document)?;
    }

    // The merged image is the rest of the file (`psdparse.h:623-626`).
    if cursor.remaining() > 0 {
        document.merged = Some(cursor.bytes(cursor.remaining())?.to_vec());
    }
    if !cursor.eof() {
        return fail("trailing data");
    }

    apply_resources(&mut document, color_mode, &resources)?;
    Ok(document)
}

struct Resource {
    id: u16,
    data: Vec<u8>,
}

/// The `8BIM`-tagged resource list (`psdparse.h:241-260`).
fn parse_image_resources(data: &[u8]) -> std::result::Result<Vec<Resource>, ParseError> {
    let mut cursor = Cursor::new(data);
    let mut resources = Vec::new();
    while !cursor.eof() {
        cursor.literal(b"8BIM")?;
        let id = cursor.u16()?;
        let name_length = cursor.u8()? as usize;
        cursor.skip(name_length)?;
        // The name is padded to an even length (`psdparse.h:246`).
        if !(name_length + 1).is_multiple_of(2) {
            cursor.skip(1)?;
        }
        let size = cursor.u32()? as usize;
        // The data range covers the padded size (`psdparse.h:249-250`).
        let padded = size.div_ceil(2) * 2;
        let resource_data = cursor.bytes(padded)?;
        resources.push(Resource {
            id,
            data: resource_data[..size.min(resource_data.len())].to_vec(),
        });
    }
    Ok(resources)
}

/// The resource post-pass (`psdparse.cpp:10-125`): guides and slices (the
/// colour table count and transparency index feed the indexed table below),
/// in file order.
fn apply_resources(
    document: &mut PsdDocument,
    color_mode: &[u8],
    resources: &[Resource],
) -> std::result::Result<(), ParseError> {
    for resource in resources {
        match resource.id {
            1032 => {
                document.guides = Some(parse_grid_and_guides(&resource.data)?);
            }
            1050 => {
                if let Some(slices) = parse_slices(&resource.data)? {
                    document.slices = Some(slices);
                }
            }
            1046 => {
                let mut cursor = Cursor::new(&resource.data);
                let _ = cursor.u16();
            }
            1047 => {
                let mut cursor = Cursor::new(&resource.data);
                document.transparency_index = i32::from(cursor.i16()?);
            }
            _ => {}
        }
    }
    // The indexed colour table is the 768-byte colour mode data
    // (`psdparse.cpp:128-145`).
    if document.header.mode as i64 == COLOR_MODE_INDEXED && color_mode.len() == 768 {
        let mut cursor = Cursor::new(color_mode);
        let mut table = vec![[0u8; 4]; 256];
        for entry in table.iter_mut() {
            entry[0] = cursor.u8()?;
            entry[3] = 0xff;
        }
        for entry in table.iter_mut() {
            entry[1] = cursor.u8()?;
        }
        for entry in table.iter_mut() {
            entry[2] = cursor.u8()?;
        }
        if document.transparency_index >= 0 && document.transparency_index < 256 {
            table[document.transparency_index as usize][3] = 0;
        }
        document.color_table = table;
    }
    Ok(())
}

/// `loadResourceGridAndGuide` (`psdresource.cpp:76-94`).
fn parse_grid_and_guides(data: &[u8]) -> std::result::Result<GridGuide, ParseError> {
    let mut cursor = Cursor::new(data);
    let _version = cursor.u32()?;
    let horizontal_grid = cursor.i32()?;
    let vertical_grid = cursor.i32()?;
    let count = cursor.i32()?;
    let mut guides = Vec::new();
    for _ in 0..count.max(0) {
        let location = cursor.i32()?;
        let direction = cursor.u8()?;
        guides.push((location, direction));
    }
    Ok(GridGuide {
        horizontal_grid,
        vertical_grid,
        guides,
    })
}

/// `loadResourceSlice` (`psdresource.cpp:9-73`): only the version-6 binary
/// form fills `data.slice`; versions 7/8 are the descriptor form the
/// reference parses and discards, leaving `isEnabled` false — so `getSlices`
/// answers `void` for them here too.
fn parse_slices(data: &[u8]) -> std::result::Result<Option<SliceResource>, ParseError> {
    let mut cursor = Cursor::new(data);
    let version = cursor.i32()?;
    if version != 6 {
        return Ok(None);
    }
    let mut resource = SliceResource {
        bounding_left: cursor.i32()?,
        bounding_top: cursor.i32()?,
        bounding_right: cursor.i32()?,
        bounding_bottom: cursor.i32()?,
        group_name: cursor.unicode_string()?,
        items: Vec::new(),
    };
    let count = cursor.i32()?;
    for _ in 0..count.max(0) {
        let mut item = SliceItem {
            id: cursor.i32()?,
            group_id: cursor.i32()?,
            origin: cursor.i32()?,
            ..SliceItem::default()
        };
        item.associated_layer_id = if item.origin == 1 {
            cursor.i32()?
        } else {
            // The reference has no documentation for the other origins and
            // stores -1 (`psdresource.cpp:26-30`).
            -1
        };
        item.name = cursor.unicode_string()?;
        item.kind = cursor.i32()?;
        item.left = cursor.i32()?;
        item.top = cursor.i32()?;
        item.right = cursor.i32()?;
        item.bottom = cursor.i32()?;
        item.url = cursor.unicode_string()?;
        item.target = cursor.unicode_string()?;
        item.message = cursor.unicode_string()?;
        item.alt_tag = cursor.unicode_string()?;
        item.cell_text_is_html = cursor.u8()? != 0;
        item.cell_text = cursor.unicode_string()?;
        item.horizontal_alignment = cursor.i32()?;
        item.vertical_alignment = cursor.i32()?;
        item.color_a = cursor.u8()?;
        item.color_r = cursor.u8()?;
        item.color_g = cursor.u8()?;
        item.color_b = cursor.u8()?;
        resource.items.push(item);
    }
    Ok(Some(resource))
}

/// The layer-and-mask block (`psdparse.h:530-590`).
fn parse_layer_and_mask(
    data: &[u8],
    document: &mut PsdDocument,
) -> std::result::Result<(), ParseError> {
    let mut cursor = Cursor::new(data);
    let layer_info_size = cursor.u32()? as usize;
    let layer_info = cursor.bytes(layer_info_size)?;
    parse_layer_info(layer_info, document)?;
    if cursor.remaining() >= 4 {
        let global_mask_size = cursor.u32()? as usize;
        cursor.skip(global_mask_size.min(cursor.remaining()))?;
    }
    // The 16/32-bit layer info repeat: the reference re-parses the same
    // grammar against the same document, appending a second copy of every
    // layer and replacing the channel blob (`psdparse.h:552-557`).
    if cursor.remaining() >= 12 {
        let rest = &cursor.data[cursor.position..];
        if &rest[..4] == b"8BIM" && (&rest[4..8] == b"Lr16" || &rest[4..8] == b"Lr32") {
            cursor.skip(8)?;
            let size = cursor.u32()? as usize;
            let repeat = cursor.bytes(size)?;
            parse_layer_info(repeat, document)?;
        }
    }
    Ok(())
}

/// The layer info block (`psdparse.h:411-475`).
fn parse_layer_info(
    data: &[u8],
    document: &mut PsdDocument,
) -> std::result::Result<(), ParseError> {
    let mut cursor = Cursor::new(data);
    let count = cursor.i16()?;
    let layer_count = i32::from(count).unsigned_abs() as usize;
    let mut offset = 0usize;
    for _ in 0..layer_count {
        let mut layer = Layer::default();
        layer.top = cursor.i32()?;
        layer.left = cursor.i32()?;
        layer.bottom = cursor.i32()?;
        layer.right = cursor.i32()?;
        layer.width = layer.right - layer.left;
        layer.height = layer.bottom - layer.top;
        let channel_count = cursor.u16()? as usize;
        for _ in 0..channel_count {
            let id = i32::from(cursor.i16()?);
            let length = cursor.u32()?;
            layer.channels.push(ChannelInfo { id, offset, length });
            offset += length as usize;
        }
        cursor.literal(b"8BIM")?;
        let blend_mode_key = cursor.u32()?;
        layer.blend_mode = blend_key_to_mode(blend_mode_key);
        layer.opacity = i32::from(cursor.u8()?);
        layer.clipping = i32::from(cursor.u8()?);
        layer.flag = i32::from(cursor.u8()?);
        let _filler = cursor.u8()?;
        let extra_size = cursor.u32()? as usize;
        let extra = cursor.bytes(extra_size)?;
        parse_extra_data(extra, &mut layer);
        document.layers.push(layer);
    }
    // Everything after the records is the concatenated channel image data
    // (`psdparse.h:448-450`).
    let rest = &data[cursor.position..];
    document.channel_data = rest.to_vec();
    link_layer_groups(document);
    Ok(())
}

/// The extra-data block (`psdparse.h:329-359`).
fn parse_extra_data(data: &[u8], layer: &mut Layer) {
    let mut cursor = Cursor::new(data);
    let Ok(mask_size) = cursor.u32() else {
        return;
    };
    if mask_size > 0 {
        if let Ok(mask_data) = cursor.bytes(mask_size as usize) {
            parse_mask(mask_data, &mut layer.mask);
        }
    } else {
        layer.mask = LayerMask::default();
    }
    let Ok(blending_size) = cursor.u32() else {
        return;
    };
    if cursor.skip(blending_size as usize).is_err() {
        return;
    }
    let Ok(name_length) = cursor.u8() else {
        return;
    };
    let name_length = name_length as usize;
    if let Ok(name) = cursor.bytes(name_length) {
        layer.name = String::from_utf8_lossy(name).into_owned();
    }
    // The Pascal name is padded to a 4-byte boundary.
    let padding = (4 - ((name_length + 1) & 3)) & 3;
    if cursor.skip(padding).is_err() {
        return;
    }
    // Additional blocks, `*additional` in the grammar: a short one simply
    // ends the loop (the reference ignores the parse result,
    // `psdparse.h:357-358`).
    while cursor.remaining() >= 12 {
        let signature = match cursor.bytes(4) {
            Ok(signature) => signature,
            Err(_) => return,
        };
        let signature_ok = signature == b"8BIM";
        if signature != b"8BIM" && signature != b"8B64" {
            return;
        }
        let Ok(key) = cursor.u32() else {
            return;
        };
        let Ok(size) = cursor.u32() else {
            return;
        };
        let Ok(payload) = cursor.bytes(size as usize) else {
            return;
        };
        if !signature_ok {
            // `8B64` blocks are refused with a note on stderr
            // (`psdparse.cpp:168-173`).
            continue;
        }
        apply_additional(key, payload, layer);
    }
}

/// `LayerMaskParser` (`psdparse.h:274-300`).
fn parse_mask(data: &[u8], mask: &mut LayerMask) {
    let mut cursor = Cursor::new(data);
    let Ok(top) = cursor.i32() else { return };
    let Ok(left) = cursor.i32() else { return };
    let Ok(bottom) = cursor.i32() else { return };
    let Ok(right) = cursor.i32() else { return };
    let Ok(default_color) = cursor.u8() else {
        return;
    };
    let _flags = cursor.u8();
    let _padding = cursor.u8();
    mask.top = top;
    mask.left = left;
    mask.bottom = bottom;
    mask.right = right;
    mask.width = right - left;
    mask.height = bottom - top;
    mask.default_color = default_color;
}

/// The additional-block dispatch (`psdparse.cpp:176-267`, `psdlayer.cpp`).
fn apply_additional(key: u32, payload: &[u8], layer: &mut Layer) {
    match key {
        // Adjust and fill layers are classified by their additional key
        // (`psdparse.cpp:176-203`).
        0x6772_646d // 'grdm'
        | 0x6c65_766c // 'levl'
        | 0x6375_7276 // 'curv'
        | 0x6875_6520 // 'hue '
        | 0x6875_6532 // 'hue2'
        | 0x626c_6e63 // 'blnc'
        | 0x6e76_7274 // 'nvrt'
        | 0x706f_7374 // 'post'
        | 0x7468_7273 // 'thrs'
        | 0x7365_6c63 // 'selc'
        | 0x6272_6974 // 'brit'
        | 0x6d69_7872 // 'mixr'
        | 0x636c_724c // 'clrL'
        | 0x7068_666c // 'phfl'
        | 0x626c_7768 // 'blwh'
        | 0x7669_6241 // 'vibA'
        | 0x6578_7041 // 'expA'
        => layer.layer_type = LAYER_TYPE_ADJUST,
        0x536f_436f // 'SoCo'
        | 0x4764_466c // 'GdFl'
        | 0x5074_466c // 'PtFl'
        => layer.layer_type = LAYER_TYPE_FILL,
        0x6c73_6374 // 'lsct'
        | 0x6c73_646b // 'lsdk'
        => load_section_divider(payload, layer),
        0x6c75_6e69 // 'luni'
        => {
            let mut cursor = Cursor::new(payload);
            if let Ok(name) = cursor.unicode_string() {
                layer.name_unicode = name;
            }
        }
        0x6c79_6964 // 'lyid'
        => {
            let mut cursor = Cursor::new(payload);
            if let Ok(id) = cursor.i32() {
                layer.layer_id = id;
            }
        }
        // 'shmd' (layer comps metadata) is part of the descriptor gap.
        _ => {}
    }
}

/// `loadLayerSectionDivider` (`psdlayer.cpp:7-31`).
fn load_section_divider(payload: &[u8], layer: &mut Layer) {
    let mut cursor = Cursor::new(payload);
    let Ok(kind) = cursor.i32() else {
        return;
    };
    match kind {
        1 | 2 => layer.layer_type = LAYER_TYPE_FOLDER,
        3 => layer.layer_type = LAYER_TYPE_HIDDEN,
        _ => {}
    }
    if payload.len() >= 12 {
        let _signature = cursor.i32();
        if let Ok(blend_key) = cursor.u32() {
            layer.blend_mode = blend_key_to_mode(blend_key);
        }
    }
    if payload.len() >= 16 {
        let _subtype = cursor.i32();
    }
}

/// `blendKeyToMode` (`psddata.h:112-143`).
fn blend_key_to_mode(key: u32) -> i64 {
    match key {
        0x6e6f_726d => 0,  // 'norm'
        0x6469_7373 => 1,  // 'diss'
        0x6461_726b => 2,  // 'dark'
        0x6d75_6c20 => 3,  // 'mul '
        0x6964_6976 => 4,  // 'idiv'
        0x6c62_726e => 5,  // 'lbrn'
        0x646b_436c => 24, // 'dkCl'
        0x6c69_7465 => 6,  // 'lite'
        0x7363_726e => 7,  // 'scrn'
        0x6469_7620 => 8,  // 'div '
        0x6c64_6467 => 9,  // 'lddg'
        0x6c74_436c => 25, // 'ltCl'
        0x6f76_6572 => 10, // 'over'
        0x734c_6974 => 11, // 'sLit'
        0x684c_6974 => 12, // 'hLit'
        0x764c_6974 => 13, // 'vLit'
        0x6c4c_6974 => 14, // 'lLit'
        0x704c_6974 => 15, // 'pLit'
        0x684d_6978 => 16, // 'hMix'
        0x6469_6666 => 17, // 'diff'
        0x736d_7564 => 18, // 'smud'
        0x6673_7562 => 26, // 'fsub'
        0x6664_6976 => 27, // 'fdiv'
        0x6875_6520 => 19, // 'hue '
        0x7361_7420 => 20, // 'sat '
        0x636f_6c72 => 21, // 'colr'
        0x6c75_6d20 => 22, // 'lum '
        0x7061_7373 => 23, // 'pass'
        _ => BLEND_MODE_INVALID,
    }
}

/// The group linkage (`psdparse.cpp:272-288`): a reverse pass where folder
/// dividers push and hidden dividers pop.
fn link_layer_groups(document: &mut PsdDocument) {
    let mut stack: Vec<Option<usize>> = vec![None];
    for index in (0..document.layers.len()).rev() {
        document.layers[index].parent = *stack.last().unwrap_or(&None);
        match document.layers[index].layer_type {
            LAYER_TYPE_FOLDER => stack.push(Some(index)),
            LAYER_TYPE_HIDDEN => {
                stack.pop();
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Channel decoding
// ---------------------------------------------------------------------------

fn channel_bytes(depth: i32, width: usize, height: usize) -> Option<usize> {
    match depth {
        1 => Some(width.div_ceil(8) * height),
        8 => Some(width * height),
        16 => Some(width * height * 2),
        32 => Some(width * height * 4),
        _ => None,
    }
}

/// One decoded channel plane.
#[derive(Clone)]
struct Plane {
    id: i32,
    data: Vec<u8>,
}

/// `getLayerImage`'s channel preparation (`psdimage.cpp:675-795`).
fn decode_layer_channels(
    document: &PsdDocument,
    layer: &Layer,
    mode: ImageMode,
) -> Option<Vec<Plane>> {
    let image_width = layer.width.max(0) as usize;
    let image_height = layer.height.max(0) as usize;
    let mask_width = layer.mask.width.max(0) as usize;
    let mask_height = layer.mask.height.max(0) as usize;
    let image_bytes = channel_bytes(document.header.depth, image_width, image_height)?;
    let mask_bytes = if document.header.depth == 1 {
        0
    } else {
        channel_bytes(document.header.depth, mask_width, mask_height)?
    };
    let mut planes = Vec::new();
    for channel in &layer.channels {
        if mode == ImageMode::Image && channel.is_mask() {
            continue;
        }
        if mode == ImageMode::Mask && !channel.is_mask() {
            continue;
        }
        let (size, width, height) = if channel.is_mask() {
            (mask_bytes, mask_width, mask_height)
        } else {
            (image_bytes, image_width, image_height)
        };
        let data = decode_channel(document, channel, size, width, height)?;
        planes.push(Plane {
            id: channel.id,
            data,
        });
    }
    Some(planes)
}

/// One channel's plane (`psdimage.cpp:742-790`).
fn decode_channel(
    document: &PsdDocument,
    channel: &ChannelInfo,
    size: usize,
    width: usize,
    height: usize,
) -> Option<Vec<u8>> {
    let start = channel.offset;
    let length = channel.length as usize;
    if start + 2 > document.channel_data.len() || length < 2 {
        return None;
    }
    let bytes = &document.channel_data[start..(start + length).min(document.channel_data.len())];
    let compression = u16::from_be_bytes([bytes[0], bytes[1]]);
    let data = &bytes[2..];
    let mut plane = vec![0u8; size];
    match compression {
        0 => {
            let count = size.min(data.len());
            plane[..count].copy_from_slice(&data[..count]);
        }
        1 => {
            decode_packbits(&mut plane, data, height, 1, 0, 0);
        }
        2 => {
            if !decode_zip_without_prediction(&mut plane, data) {
                return None;
            }
        }
        3 => {
            if !decode_zip_with_prediction(&mut plane, data, width, height, document.header.depth) {
                return None;
            }
        }
        // Any other compression id fills the plane with 0xff
        // (`psdimage.cpp:787-789`).
        _ => plane.fill(0xff),
    }
    Some(plane)
}

/// `decodePackBits` (`psdimage.cpp:511-549`): the PackBits row table is
/// `2*height*channels` bytes, the row data follows it at `lineDataOffset`
/// (which the merged-image path accumulates).
fn decode_packbits(
    out: &mut [u8],
    source: &[u8],
    height: usize,
    channels: usize,
    target_channel: usize,
    line_data_offset: usize,
) -> usize {
    let header_size = 2 * height * channels;
    let table_start = 2 * height * target_channel;
    let mut line_data = header_size + line_data_offset;
    let mut decoded = 0usize;
    let mut read_bytes = 0usize;
    for y in 0..height {
        if table_start + (y + 1) * 2 > source.len() {
            break;
        }
        let line_bytes =
            i16::from_be_bytes([source[table_start + y * 2], source[table_start + y * 2 + 1]])
                as i64;
        let mut x = 0i64;
        while x < line_bytes {
            let Some(&opcode) = source.get(line_data) else {
                return read_bytes;
            };
            line_data += 1;
            x += 1;
            let count = if opcode > 128 {
                // A run of `257 - opcode` copies of the next byte
                // (`psdimage.cpp:514-517`).
                let count = 257 - usize::from(opcode);
                let Some(&value) = source.get(line_data) else {
                    return read_bytes;
                };
                line_data += 1;
                x += 1;
                for index in 0..count {
                    if decoded + index >= out.len() {
                        break;
                    }
                    out[decoded + index] = value;
                }
                count
            } else if opcode < 128 {
                // A literal run of `opcode + 1` bytes (`psdimage.cpp:518-523`).
                let count = usize::from(opcode) + 1;
                for index in 0..count {
                    let Some(&value) = source.get(line_data) else {
                        return read_bytes;
                    };
                    line_data += 1;
                    if decoded + index >= out.len() {
                        continue;
                    }
                    out[decoded + index] = value;
                }
                x += count as i64;
                count
            } else {
                // 128 is a no-op that still advances the destination pointer
                // in the reference's loop (`decoded += length` with length
                // unchanged).
                128
            };
            decoded += count;
        }
        read_bytes += line_bytes.max(0) as usize;
    }
    read_bytes
}

/// `decodeZipWithoutPrediction` (`psdimage.cpp:553-581`): inflate exactly
/// `out.len()` bytes and require the stream to end there.
fn decode_zip_without_prediction(out: &mut [u8], source: &[u8]) -> bool {
    let mut decoder = ZlibDecoder::new(source);
    let mut filled = 0usize;
    while filled < out.len() {
        match decoder.read(&mut out[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(_) => return false,
        }
    }
    if filled != out.len() {
        return false;
    }
    // The reference requires `Z_STREAM_END` (`psdimage.cpp:578`): one more
    // read must hit the end of the stream.
    let mut extra = [0u8; 1];
    matches!(decoder.read(&mut extra), Ok(0))
}

/// `decodeZipWithPrediction` (`psdimage.cpp:584-656`): 16- and 32-bit only.
fn decode_zip_with_prediction(
    out: &mut [u8],
    source: &[u8],
    width: usize,
    height: usize,
    depth: i32,
) -> bool {
    match depth {
        16 => {
            if !decode_zip_without_prediction(out, source) {
                return false;
            }
            for row in 0..height {
                let base = row * width * 2;
                if base + width * 2 > out.len() {
                    break;
                }
                let mut x = 0usize;
                while x < (width.saturating_sub(1)) * 2 {
                    if base + x + 3 >= out.len() {
                        break;
                    }
                    let low = u16::from(out[base + x + 3]) + u16::from(out[base + x + 1]);
                    out[base + x + 2] = out[base + x + 2]
                        .wrapping_add(out[base + x])
                        .wrapping_add((low / 256) as u8);
                    out[base + x + 3] = low as u8;
                    x += 2;
                }
            }
            true
        }
        32 => {
            let mut buffer = vec![0u8; out.len()];
            if !decode_zip_without_prediction(&mut buffer, source) {
                return false;
            }
            for row in 0..height {
                let base = row * width * 4;
                if base + width * 4 > buffer.len() {
                    break;
                }
                for index in base + 1..base + width * 4 {
                    buffer[index] = buffer[index].wrapping_add(buffer[index - 1]);
                }
            }
            // De-planarize the four byte planes (`psdimage.cpp:624-654`).
            for row in 0..height {
                let base = row * width * 4;
                for x in 0..width {
                    for byte in 0..4 {
                        let value = buffer[base + byte * width + x];
                        out[base + x * 4 + byte] = value;
                    }
                }
            }
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Pixel composition
// ---------------------------------------------------------------------------

/// A 32-bit pixel in the engine's plane order (R, G, B, A).
fn pack_rgba(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
    [r, g, b, a]
}

/// The top byte of a big-endian sample of `depth` bits.
fn sample_high_byte(data: &[u8], index: usize, depth: i32) -> u8 {
    match depth {
        16 => {
            let offset = index * 2;
            data.get(offset).copied().unwrap_or(0)
        }
        32 => {
            // A big-endian float in [0, 1] scaled to 0..255
            // (`rgbaCompoToRgba32<uint32_t>`, `psdimage.cpp:39-60`).
            let offset = index * 4;
            let bytes = [
                data.get(offset + 3).copied().unwrap_or(0),
                data.get(offset + 2).copied().unwrap_or(0),
                data.get(offset + 1).copied().unwrap_or(0),
                data.get(offset).copied().unwrap_or(0),
            ];
            let value = f32::from_bits(u32::from_le_bytes(bytes));
            (value * 255.0) as u8
        }
        _ => data.get(index).copied().unwrap_or(0),
    }
}

/// `mergeMaskToAlpha` (`psdimage.cpp:399-505`).
fn merge_mask_to_alpha(
    alpha: &mut [u8],
    alpha_rect: (i32, i32, i32, i32),
    mask: &[u8],
    mask_rect: (i32, i32, i32, i32),
    default_color: u8,
) {
    let (al, at, ar, ab) = alpha_rect;
    let (ml, mt, mr, mb) = mask_rect;
    let aw = (ar - al).max(0) as usize;
    let ah = (ab - at).max(0) as usize;
    let mw = (mr - ml).max(0) as usize;
    let left = al.max(ml);
    let top = at.max(mt);
    let right = ar.min(mr);
    let bottom = ab.min(mb);
    let width = (right - left).max(0) as usize;
    let height = (bottom - top).max(0) as usize;
    if width == 0 || height == 0 {
        if default_color == 0 {
            alpha.fill(0);
        }
        return;
    }
    for y in 0..height {
        let alpha_row = (top - at) as usize + y;
        let mask_row = (top - mt) as usize + y;
        for x in 0..width {
            let alpha_index = alpha_row * aw + (left - al) as usize + x;
            let mask_index = mask_row * mw + (left - ml) as usize + x;
            let (Some(&a), Some(&m)) = (alpha.get(alpha_index), mask.get(mask_index)) else {
                continue;
            };
            // `*ap = *ap * *mp * f` with f = 1/255 for 8-bit samples.
            alpha[alpha_index] = ((u32::from(a) * u32::from(m)) / 255) as u8;
        }
    }
    if default_color != 0 {
        return;
    }
    // Everything outside the intersection is zeroed (`psdimage.cpp:443-504`).
    for y in 0..ah {
        for x in 0..aw {
            let inside = (top - at) as usize..(bottom - at) as usize;
            if !inside.contains(&y) || x < (left - al) as usize || x >= (right - al) as usize {
                alpha[y * aw + x] = 0;
            }
        }
    }
}

/// `getLayerImage`'s composition (`psdimage.cpp:808-927`).
fn compose_layer_image(
    document: &PsdDocument,
    layer: &Layer,
    mode: ImageMode,
    width: usize,
    height: usize,
) -> Option<Vec<u8>> {
    let mut planes = decode_layer_channels(document, layer, mode)?;
    if mode == ImageMode::Mask {
        // The mask plane is composed as a grayscale image
        // (`psdimage.cpp:171-177`).
        planes.first_mut()?.id = 0;
    }
    if mode == ImageMode::MaskedImage {
        let alpha = planes.iter().position(|plane| plane.id == -1);
        let mask = planes
            .iter()
            .position(|plane| plane.id == -2 || plane.id == -3);
        if let (Some(alpha), Some(mask)) = (alpha, mask) {
            let (alpha_rect, mask_rect) = (
                (layer.left, layer.top, layer.right, layer.bottom),
                (
                    layer.mask.left,
                    layer.mask.top,
                    layer.mask.right,
                    layer.mask.bottom,
                ),
            );
            let mask_plane = planes[mask].data.clone();
            merge_mask_to_alpha(
                &mut planes[alpha].data,
                alpha_rect,
                &mask_plane,
                mask_rect,
                layer.mask.default_color,
            );
        }
    }
    let mode = if mode == ImageMode::Mask {
        COLOR_MODE_GRAYSCALE
    } else {
        document.header.mode as i64
    };
    compose_channels(document, &planes, mode, width, height, false)
}

/// `getMergedImage` (`psdimage.cpp:942-1115`).
fn compose_merged_image(document: &PsdDocument) -> Option<Vec<u8>> {
    let merged = document.merged.as_ref()?;
    if merged.len() < 2 {
        return None;
    }
    let width = document.header.width.max(0) as usize;
    let height = document.header.height.max(0) as usize;
    let plane_size = channel_bytes(document.header.depth, width, height)?;
    let channels = document.header.channels.max(0) as usize;
    let compression = u16::from_be_bytes([merged[0], merged[1]]);
    let data = &merged[2..];
    let mut planes = Vec::with_capacity(channels);
    for index in 0..channels {
        let mut plane = vec![0u8; plane_size];
        match compression {
            0 => {
                let start = index * plane_size;
                let count = plane_size.min(data.len().saturating_sub(start));
                plane[..count].copy_from_slice(&data[start..start + count]);
            }
            1 => {
                let next = decode_packbits(&mut plane, data, height, channels, index, 0);
                let _ = next;
            }
            2 => {
                if !decode_zip_without_prediction(&mut plane, data) {
                    return None;
                }
            }
            3 => {
                let decoded = decode_zip_with_prediction(
                    &mut plane,
                    data,
                    width,
                    height,
                    document.header.depth,
                );
                if !decoded {
                    return None;
                }
            }
            _ => {}
        }
        // The merged image's channels are identified by ordinal; RGB treats
        // the fourth as alpha (`psdimage.cpp:1055-1057`).
        let id = if document.header.mode as i64 == COLOR_MODE_RGB && index == 3 {
            -1
        } else {
            index as i32
        };
        planes.push(Plane { id, data: plane });
    }
    // The merged RLE path needs the accumulated data offset per channel, the
    // way `psdimage.cpp:989-997` walks it.
    if compression == 1 {
        let mut next_offset = 0usize;
        for (index, plane) in planes.iter_mut().enumerate() {
            plane.data.fill(0);
            next_offset +=
                decode_packbits(&mut plane.data, data, height, channels, index, next_offset);
        }
    }
    compose_channels(
        document,
        &planes,
        document.header.mode as i64,
        width,
        height,
        true,
    )
}

/// The per-colour-mode merges (`psdimage.cpp:221-397`). `merged` selects the
/// merged-image channel-id rules (ordinal ids) over the layer rules.
fn compose_channels(
    document: &PsdDocument,
    planes: &[Plane],
    color_mode: i64,
    width: usize,
    height: usize,
    merged: bool,
) -> Option<Vec<u8>> {
    let _ = merged;
    let find = |id: i32| planes.iter().find(|plane| plane.id == id);
    let mut out = vec![0u8; width * height * 4];
    let depth = document.header.depth;
    let put = |out: &mut [u8], index: usize, pixel: [u8; 4]| {
        if index * 4 + 4 <= out.len() {
            out[index * 4..index * 4 + 4].copy_from_slice(&pixel);
        }
    };
    match color_mode {
        COLOR_MODE_BITMAP => {
            // `mergeChannelsBitmap` (`psdimage.cpp:221-251`): bit 7 first,
            // white for 0, black for 1, alpha opaque.
            let source = planes.first()?;
            let row_bytes = width.div_ceil(8);
            for y in 0..height {
                for x in 0..width {
                    let byte = source.data.get(y * row_bytes + x / 8).copied().unwrap_or(0);
                    let bit = (byte >> (7 - (x % 8))) & 1;
                    let gray = if bit == 0 { 0xff } else { 0x00 };
                    put(&mut out, y * width + x, pack_rgba(gray, gray, gray, 0xff));
                }
            }
        }
        COLOR_MODE_GRAYSCALE => {
            let gray = find(0)?;
            let alpha = find(-1);
            for y in 0..height {
                for x in 0..width {
                    let index = y * width + x;
                    let value = sample_high_byte(&gray.data, index, depth);
                    let a = alpha
                        .map(|plane| sample_high_byte(&plane.data, index, depth))
                        .unwrap_or(0xff);
                    put(&mut out, index, pack_rgba(value, value, value, a));
                }
            }
        }
        COLOR_MODE_INDEXED => {
            let source = planes.first()?;
            for y in 0..height {
                for x in 0..width {
                    let index = y * width + x;
                    let entry = source.data.get(index).copied().unwrap_or(0) as usize;
                    let color = document
                        .color_table
                        .get(entry)
                        .copied()
                        .unwrap_or([0, 0, 0, 0xff]);
                    put(
                        &mut out,
                        index,
                        pack_rgba(color[0], color[1], color[2], color[3]),
                    );
                }
            }
        }
        COLOR_MODE_RGB => {
            let (Some(r), Some(g), Some(b)) = (find(0), find(1), find(2)) else {
                return None;
            };
            let alpha = find(-1);
            for y in 0..height {
                for x in 0..width {
                    let index = y * width + x;
                    let a = alpha
                        .map(|plane| sample_high_byte(&plane.data, index, depth))
                        .unwrap_or(0xff);
                    put(
                        &mut out,
                        index,
                        pack_rgba(
                            sample_high_byte(&r.data, index, depth),
                            sample_high_byte(&g.data, index, depth),
                            sample_high_byte(&b.data, index, depth),
                            a,
                        ),
                    );
                }
            }
        }
        COLOR_MODE_CMYK => {
            let (Some(c), Some(m), Some(y), Some(k)) = (find(0), find(1), find(2), find(3)) else {
                return None;
            };
            let alpha = find(-1);
            for index in 0..width * height {
                let c = sample_high_byte(&c.data, index, depth);
                let m = sample_high_byte(&m.data, index, depth);
                let y = sample_high_byte(&y.data, index, depth);
                let k = sample_high_byte(&k.data, index, depth);
                let a = alpha
                    .map(|plane| sample_high_byte(&plane.data, index, depth))
                    .unwrap_or(0xff);
                let inv_k = 255u32 - u32::from(k);
                let r = ((inv_k * (255 - u32::from(c))) >> 8) as u8;
                let g = ((inv_k * (255 - u32::from(m))) >> 8) as u8;
                let b = ((inv_k * (255 - u32::from(y))) >> 8) as u8;
                put(&mut out, index, pack_rgba(r, g, b, a));
            }
        }
        // Multichannel, duotone and Lab write no pixels but still report
        // success (`psdimage.cpp:920-926`).
        _ => {}
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// The `psd://` storage media
// ---------------------------------------------------------------------------

/// The media registration (`PSDStorage`, `main.cpp:14-216`).
struct PsdMedia {
    cache: Mutex<BTreeMap<String, Arc<PsdDocument>>>,
    storage: Mutex<Option<Weak<dyn plugin_api::ProjectStoragePort>>>,
}

impl PsdMedia {
    fn new() -> Self {
        PsdMedia {
            cache: Mutex::new(BTreeMap::new()),
            storage: Mutex::new(None),
        }
    }

    /// `PSD::clearStorageCache` (`main.cpp:248-254`).
    fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    /// The document for a media name, parsing it on first use the way
    /// `PSDStorage::getPSD` builds a `new PSD()` for an unregistered name
    /// (`main.cpp:193-204`).
    fn document(&self, name: &str) -> Option<Arc<PsdDocument>> {
        let (domain, path) = split_name(name)?;
        if let Ok(cache) = self.cache.lock()
            && let Some(document) = cache.get(&domain)
        {
            return Some(Arc::clone(document));
        }
        let storage = self.storage.lock().ok()?.as_ref().and_then(Weak::upgrade)?;
        let bytes = storage.data(&domain).ok()?;
        let bytes = bytes.as_bytes().ok()?;
        let document = Arc::new(parse_document(bytes.as_ref()).ok()?);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(domain, Arc::clone(&document));
        }
        let _ = path;
        Some(document)
    }
}

/// `getPSD`'s split (`main.cpp:162-174`): lowercase, then at the first `/`.
fn split_name(name: &str) -> Option<(String, String)> {
    let name = name.to_lowercase();
    let (domain, path) = name.split_once('/')?;
    Some((domain.to_string(), path.to_string()))
}

/// The layer lookup maps `startStorage` builds (`psdclass.cpp:684-698`):
/// layer id and `path/name` to a layer index.
type LayerMaps = (
    BTreeMap<i32, usize>,
    BTreeMap<String, BTreeMap<String, usize>>,
);

/// The layer path lookup (`PSD::CheckExistentStorage`, `psdclass.cpp:688-720`).
#[derive(Debug)]
enum Wanted {
    ById(i32),
    ByName { path: String, name: String },
}

fn wanted(path: &str) -> Option<Wanted> {
    if let Some(rest) = path.strip_prefix("id/") {
        if rest.contains('/') {
            return None;
        }
        let base = rest.strip_suffix(".bmp")?;
        if base.is_empty() || !base.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        return Some(Wanted::ById(base.parse().ok()?));
    }
    let (path, file) = path.rsplit_once('/')?;
    // The first `.` must open the `.bmp` extension (`psdclass.cpp:702`).
    let (base, extension) = file.split_once('.')?;
    if extension != "bmp" {
        return None;
    }
    Some(Wanted::ByName {
        path: format!("{path}/"),
        name: base.to_string(),
    })
}

/// `PSD::pathname`/`path_layname` (`psdclass.cpp:596-619`) as the maps
/// `startStorage` builds (`:684-698`): the bottom-most layer wins a name
/// collision.
fn layer_maps(document: &PsdDocument) -> LayerMaps {
    let mut by_id = BTreeMap::new();
    let mut by_path: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for index in (0..document.layers.len()).rev() {
        let layer = &document.layers[index];
        if layer.layer_type != LAYER_TYPE_NORMAL {
            continue;
        }
        let mut path = String::from("root/");
        let mut parents = Vec::new();
        let mut parent = layer.parent;
        while let Some(pointer) = parent {
            parents.push(pointer);
            parent = document.layers[pointer].parent;
        }
        for pointer in parents.into_iter().rev() {
            path.push_str(&path_layer_name(&document.layers[pointer]));
            path.push('/');
        }
        by_path
            .entry(path)
            .or_default()
            .insert(path_layer_name(layer), index);
        by_id.insert(layer.layer_id, index);
    }
    (by_id, by_path)
}

/// `path_layname`: lowercased with `/` replaced by `_`
/// (`psdclass.cpp:596-603`).
fn path_layer_name(layer: &Layer) -> String {
    layer_name(layer).replace('/', "_").to_lowercase()
}

fn resolve_layer<'a>(document: &'a PsdDocument, maps: &LayerMaps, path: &str) -> Option<&'a Layer> {
    match wanted(path)? {
        Wanted::ById(id) => maps.0.get(&id).map(|index| &document.layers[*index]),
        Wanted::ByName { path, name } => maps
            .1
            .get(&path)
            .and_then(|names| names.get(&name))
            .map(|index| &document.layers[*index]),
    }
}

impl plugin_api::StorageMediaProvider for PsdMedia {
    fn media_name(&self) -> &str {
        MEDIA_NAME
    }

    /// The engine hands the media a weak handle to the built-in stack before
    /// inserting it; the reference reaches the same stack through
    /// `TVPCreateIStream` (`main.cpp:193-204`).
    fn attach_storage(&self, storage: Weak<dyn plugin_api::ProjectStoragePort>) {
        if let Ok(mut slot) = self.storage.lock() {
            *slot = Some(storage);
        }
    }

    /// `CheckExistentStorage` (`main.cpp:98-106`).
    fn exists(&self, name: &str) -> bool {
        let Some((_, path)) = split_name(name) else {
            return false;
        };
        let Some(document) = self.document(name) else {
            return false;
        };
        resolve_layer(&document, &layer_maps(&document), &path).is_some()
    }

    /// `Open` (`main.cpp:111-126`): a read-only 32-bit BMP with bottom-up
    /// rows (`psdclass.cpp:761-822`).
    fn open(&self, name: &str) -> io::Result<Box<dyn plugin_api::ResourceStream>> {
        let error = || {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{name}:cannot open psdfile"),
            )
        };
        let (_, path) = split_name(name).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("invalid path:{name}"))
        })?;
        let document = self.document(name).ok_or_else(error)?;
        let layer = resolve_layer(&document, &layer_maps(&document), &path).ok_or_else(error)?;
        if layer.layer_type != LAYER_TYPE_NORMAL || layer.width <= 0 || layer.height <= 0 {
            return Err(error());
        }
        let width = layer.width as usize;
        let height = layer.height as usize;
        let pixels = compose_layer_image(&document, layer, ImageMode::MaskedImage, width, height)
            .ok_or_else(error)?;
        let pitch = width * 4;
        let mut bmp = Vec::with_capacity(54 + pitch * height);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&((54 + pitch * height) as u32).to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&(width as i32).to_le_bytes());
        bmp.extend_from_slice(&(height as i32).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&0i32.to_le_bytes());
        bmp.extend_from_slice(&0i32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        // The reference writes the rows bottom-up through a negative pitch
        // (`psdclass.cpp:809-810`); BMP files are bottom-up, and converting
        // the engine's top-down RGBA plane is a row reversal.
        for y in (0..height).rev() {
            bmp.extend_from_slice(&pixels[y * pitch..(y + 1) * pitch]);
        }
        Ok(Box::new(io::Cursor::new(bmp)))
    }

    /// `GetListAt` (`psdclass.cpp:723-754`).
    fn list(&self, name: &str) -> io::Result<Vec<String>> {
        let Some((_, path)) = split_name(name) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("invalid path:{name}"),
            ));
        };
        let Some(document) = self.document(name) else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no psd"));
        };
        let maps = layer_maps(&document);
        if path == "id/" {
            return Ok(maps
                .0
                .keys()
                .map(|id| format!("{id}.bmp"))
                .collect::<Vec<_>>());
        }
        Ok(maps
            .1
            .get(&path)
            .map(|names| {
                names
                    .keys()
                    .map(|name| format!("{name}.bmp"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, time::SystemTime};

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths, plugin_api};
    use krkr_tjs2::runtime::{ObjectHandle, Variant};

    use super::PsdPlugin;

    fn test_root(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-psd-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    fn test_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine");
        engine
            .register_plugin(PsdPlugin::default())
            .expect("plugin");
        engine
    }

    fn be16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn be32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn be_i32(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    /// A Unicode string: `int32` count then big-endian UTF-16 units
    /// (`psdparse.h:181-187`).
    fn unicode(out: &mut Vec<u8>, text: &str) {
        let units: Vec<u16> = text.encode_utf16().collect();
        be32(out, units.len() as u32);
        for unit in units {
            be16(out, unit);
        }
    }

    /// One additional block: `8BIM` + key + u32 size + payload.
    fn additional(out: &mut Vec<u8>, key: &[u8; 4], payload: &[u8]) {
        out.extend_from_slice(b"8BIM");
        out.extend_from_slice(key);
        be32(out, payload.len() as u32);
        out.extend_from_slice(payload);
    }

    /// One image resource: `8BIM`, u16 id, empty name, u32 size, padded data
    /// (`psdparse.h:241-260`).
    fn image_resource(out: &mut Vec<u8>, id: u16, payload: &[u8]) {
        out.extend_from_slice(b"8BIM");
        be16(out, id);
        out.push(0);
        out.push(0);
        be32(out, payload.len() as u32);
        out.extend_from_slice(payload);
        if !payload.len().is_multiple_of(2) {
            out.push(0);
        }
    }

    struct LayerSpec {
        bounds: (i32, i32, i32, i32),
        /// `(channel id, blob)` — the blob carries the u16 compression id and
        /// the channel's bytes.
        planes: Vec<(i32, Vec<u8>)>,
        blend: &'static [u8; 4],
        opacity: u8,
        clipping: u8,
        flag: u8,
        name: &'static str,
        additions: Vec<(&'static [u8; 4], Vec<u8>)>,
    }

    fn layer_record(out: &mut Vec<u8>, spec: &LayerSpec) {
        let (top, left, bottom, right) = spec.bounds;
        be_i32(out, top);
        be_i32(out, left);
        be_i32(out, bottom);
        be_i32(out, right);
        be16(out, spec.planes.len() as u16);
        for (id, blob) in &spec.planes {
            be16(out, *id as u16);
            be32(out, blob.len() as u32);
        }
        out.extend_from_slice(b"8BIM");
        out.extend_from_slice(spec.blend);
        out.push(spec.opacity);
        out.push(spec.clipping);
        out.push(spec.flag);
        out.push(0);
        let mut extra = Vec::new();
        be32(&mut extra, 0); // mask record
        be32(&mut extra, 0); // blending ranges
        extra.push(spec.name.len() as u8);
        extra.extend_from_slice(spec.name.as_bytes());
        let padding = (4 - ((spec.name.len() + 1) & 3)) & 3;
        extra.extend(std::iter::repeat_n(0u8, padding));
        for (key, payload) in &spec.additions {
            additional(&mut extra, key, payload);
        }
        be32(out, extra.len() as u32);
        out.extend_from_slice(&extra);
    }

    /// The fixture every test shares: a 4x4 8-bit RGB document with four
    /// layers — two normal ones (one all-raw, one with a PackBits channel),
    /// an adjust layer and a folder that the reference's reverse linkage pass
    /// makes the parent of the three layers before it — plus a guide
    /// resource, a version-6 slice resource and a raw merged image.
    ///
    /// Hand-computed channel data:
    ///
    /// * layer 0 `background` (Unicode name `背景`) 2x2 at (0,0): R
    ///   16,32,48,64 · G 80,96,112,128 · B 144,160,176,192, all raw;
    /// * layer 1 `overlay` 2x2 at (1,1): R 1,2,3,4 raw · **G PackBits** —
    ///   row 0 a two-byte literal `[10,20]` (opcode `0x01`), row 1 a two-byte
    ///   run of `10` (opcode `0xFF`), the row table holding 3 and 2 · B
    ///   5,6,7,8 raw · A 255,128,64,32 raw;
    /// * merged image: R = x + 4y, G = 100 + x, B = 200 - y.
    fn fixture() -> Vec<u8> {
        let raw_plane = |values: [u8; 4]| {
            let mut blob = Vec::new();
            be16(&mut blob, 0);
            blob.extend(values);
            blob
        };
        let planes_0 = vec![
            (0, raw_plane([16, 32, 48, 64])),
            (1, raw_plane([80, 96, 112, 128])),
            (2, raw_plane([144, 160, 176, 192])),
        ];
        let packbits = {
            let mut blob = Vec::new();
            be16(&mut blob, 1);
            // The row byte-count table, then the two rows.
            be16(&mut blob, 3);
            be16(&mut blob, 2);
            blob.extend([0x01, 10, 20]);
            blob.extend([0xff, 10]);
            blob
        };
        let planes_1 = vec![
            (0, raw_plane([1, 2, 3, 4])),
            (1, packbits),
            (2, raw_plane([5, 6, 7, 8])),
            (-1, raw_plane([255, 128, 64, 32])),
        ];

        let specs = vec![
            LayerSpec {
                bounds: (0, 0, 2, 2),
                planes: planes_0,
                blend: b"norm",
                opacity: 255,
                clipping: 0,
                flag: 0,
                name: "background",
                additions: vec![
                    (b"lyid", 7i32.to_be_bytes().to_vec()),
                    (b"luni", {
                        let mut payload = Vec::new();
                        unicode(&mut payload, "背景");
                        payload
                    }),
                ],
            },
            LayerSpec {
                bounds: (1, 1, 3, 3),
                planes: planes_1,
                blend: b"norm",
                opacity: 200,
                clipping: 1,
                flag: 0,
                name: "overlay",
                additions: vec![(b"lyid", 42i32.to_be_bytes().to_vec())],
            },
            LayerSpec {
                bounds: (0, 0, 0, 0),
                planes: Vec::new(),
                blend: b"norm",
                opacity: 255,
                clipping: 0,
                flag: 0,
                name: "adjust",
                additions: vec![(b"levl", Vec::new())],
            },
            LayerSpec {
                bounds: (0, 0, 0, 0),
                planes: Vec::new(),
                blend: b"norm",
                opacity: 255,
                clipping: 0,
                flag: 0,
                name: "folder",
                additions: vec![
                    (b"lsct", {
                        let mut payload = Vec::new();
                        be_i32(&mut payload, 1);
                        payload.extend_from_slice(b"8BIM");
                        payload.extend_from_slice(b"norm");
                        be_i32(&mut payload, 0);
                        payload
                    }),
                    (b"lyid", 99i32.to_be_bytes().to_vec()),
                ],
            },
        ];

        let mut records = Vec::new();
        be16(&mut records, specs.len() as u16);
        for spec in &specs {
            layer_record(&mut records, spec);
        }
        let mut layer_info = records;
        for spec in &specs {
            for (_, blob) in &spec.planes {
                layer_info.extend(blob);
            }
        }

        let mut layer_and_mask = Vec::new();
        be32(&mut layer_and_mask, layer_info.len() as u32);
        layer_and_mask.extend(&layer_info);
        be32(&mut layer_and_mask, 0); // global layer mask info

        let mut resources = Vec::new();
        {
            // Guide resource 1032 (`psdresource.cpp:76-94`).
            let mut payload = Vec::new();
            be32(&mut payload, 1);
            be32(&mut payload, 8);
            be32(&mut payload, 16);
            be32(&mut payload, 2);
            be_i32(&mut payload, 5);
            payload.push(0);
            be_i32(&mut payload, 7);
            payload.push(1);
            image_resource(&mut resources, 1032, &payload);
        }
        {
            // Slice resource 1050, version 6 with no slices.
            let mut payload = Vec::new();
            be_i32(&mut payload, 6);
            be_i32(&mut payload, 0);
            be_i32(&mut payload, 0);
            be_i32(&mut payload, 4);
            be_i32(&mut payload, 4);
            unicode(&mut payload, "group");
            be_i32(&mut payload, 0);
            image_resource(&mut resources, 1050, &payload);
        }

        let mut out = Vec::new();
        out.extend_from_slice(b"8BPS");
        be16(&mut out, 1);
        out.extend_from_slice(&[0u8; 6]);
        be16(&mut out, 3);
        be32(&mut out, 4);
        be32(&mut out, 4);
        be16(&mut out, 8);
        be16(&mut out, 3);
        be32(&mut out, 0); // colour mode data
        be32(&mut out, resources.len() as u32);
        out.extend(&resources);
        be32(&mut out, layer_and_mask.len() as u32);
        out.extend(&layer_and_mask);
        be16(&mut out, 0); // merged image: raw
        for y in 0..4u8 {
            for x in 0..4u8 {
                out.push(x + 4 * y);
            }
        }
        for _ in 0..4u8 {
            for x in 0..4u8 {
                out.push(100 + x);
            }
        }
        for y in 0..4u8 {
            for _ in 0..4 {
                out.push(200 - y);
            }
        }
        out
    }

    fn fixture_engine(name: &str) -> (std::path::PathBuf, KrkrEngine) {
        let root = test_root(name);
        fs::write(root.join("fixture.psd"), fixture()).expect("write fixture");
        let engine = test_engine(&root);
        (root, engine)
    }

    /// Runs `body` as the body of an IIFE and returns its string result; the
    /// TJS dialect this engine implements has no for-in and `execute_expression`
    /// takes an expression, so every probe ends in `return …;`.
    fn probe(engine: &mut KrkrEngine, body: &str) -> String {
        let script = format!("(function() {{\n{body}\n}})()");
        match engine
            .execute_expression("inline.tjs", &script)
            .expect("script")
        {
            Variant::String(text) => text,
            other => panic!("expected a string, got {other:?}"),
        }
    }

    fn probe_value(engine: &mut KrkrEngine, body: &str) -> Variant {
        let script = format!("(function() {{\n{body}\n}})()");
        engine
            .execute_expression("inline.tjs", &script)
            .expect("script")
    }

    fn install_layer_harness(engine: &mut KrkrEngine, body: &str) -> String {
        probe(
            engine,
            &format!(
                "global.window = new Window();\n\
                 global.layer = new Layer(window, null);\n\
                 global.psd = new PSD();\n\
                 var loaded = psd.load(\"fixture.psd\");\n\
                 {body}"
            ),
        )
    }

    /// The class's dimensions are -1 before a load (`INTGETTER`,
    /// `psdclass.h:52-59`) and the document's afterwards; the layer list is
    /// in file order and the layer facts match the records.
    #[test]
    fn loading_reports_the_document_and_layers() {
        let (root, mut engine) = fixture_engine("metadata");
        let before = probe(
            &mut engine,
            "var psd = new PSD();\n\
             return psd.width + \":\" + psd.height + \":\" + psd.channels + \":\" + psd.depth + \
             \":\" + psd.color_mode + \":\" + psd.layer_count;",
        );
        assert_eq!(before, "-1:-1:-1:-1:-1:-1");

        let after = probe(
            &mut engine,
            "var psd = new PSD();\n\
             var ok = psd.load(\"fixture.psd\");\n\
             var text = ok + \":\" + psd.width + \":\" + psd.height + \":\" + psd.channels + \":\" + \
                 psd.depth + \":\" + psd.color_mode + \":\" + psd.layer_count + \":\";\n\
             for (var i = 0; i < psd.layer_count; i++) {\n\
                 text += psd.getLayerType(i) + \"/\" + psd.getLayerName(i) + \";\";\n\
             }\n\
             return text;",
        );
        assert_eq!(after, "1:4:4:3:8:3:4:0/背景;0/overlay;3/adjust;2/folder;");
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `getLayerInfo`'s dictionary (`psdclass.cpp:235-321`), field by field,
    /// including the group linkage and the KRKR blend value.
    #[test]
    fn layer_info_matches_the_reference_dictionary() {
        let (root, mut engine) = fixture_engine("info");
        let text = probe(
            &mut engine,
            "var psd = new PSD();\n\
             psd.load(\"fixture.psd\");\n\
             var info = psd.getLayerInfo(0);\n\
             var text = \"\";\n\
             text += info.left + \",\" + info.top + \",\" + info.right + \",\" + info.bottom + \";\";\n\
             text += info.width + \",\" + info.height + \";\";\n\
             text += info.opacity + \",\" + info.visible + \",\" + info.type + \",\" + info.blend_mode + \";\";\n\
             text += info.mask + \",\" + info.layer_type + \",\" + info.clipping + \",\" + info.layer_id + \";\";\n\
             text += info.obsolete + \",\" + info.transparency_protected + \",\" + info.pixel_data_irrelevant + \";\";\n\
             text += info.name + \",\" + info.group_layer_id + \";\";\n\
             var info1 = psd.getLayerInfo(1);\n\
             text += info1.left + \",\" + info1.top + \",\" + info1.opacity + \",\" + info1.clipping + \",\" + info1.layer_id + \";\";\n\
             var info3 = psd.getLayerInfo(3);\n\
             text += info3.layer_type + \",\" + info3.group_layer_id + \",\" + info3.name;\n\
             return text;",
        );
        assert_eq!(
            text,
            "0,0,2,2;2,2;255,1,0,0;0,0,0,7;0,0,0;背景,99;1,1,200,1,42;2,,folder"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The raw and PackBits channels decode to the hand-computed pixels, and
    /// the reference's destination properties reach the target layer.
    #[test]
    fn raw_and_rle_pixels_are_decoded() {
        let (root, mut engine) = fixture_engine("pixels");
        install_layer_harness(&mut engine, "psd.getLayerData(layer, 0);\nreturn \"\";");
        let layer = engine
            .tjs_runtime()
            .global_member("layer")
            .object_handle()
            .expect("layer");
        let (width, height, pixels) = read_layer(&mut engine, layer);
        assert_eq!((width, height), (2, 2));
        assert_eq!(
            pixels,
            vec![
                16, 80, 144, 255, //
                32, 96, 160, 255, //
                48, 112, 176, 255, //
                64, 128, 192, 255,
            ]
        );
        // The reference sets these on the target (`psdclass.cpp:383-395`).
        let properties = probe(
            &mut engine,
            "return layer.left + \",\" + layer.top + \",\" + layer.width + \",\" + layer.height + \
             \",\" + layer.opacity + \",\" + layer.type + \",\" + layer.visible + \",\" + layer.name + \
             \",\" + layer.imageLeft + \",\" + layer.imageTop + \",\" + layer.imageWidth + \",\" + \
             layer.imageHeight;",
        );
        assert_eq!(properties, "0,0,2,2,255,0,1,背景,0,0,2,2");

        probe(&mut engine, "psd.getLayerData(layer, 1);\nreturn \"\";");
        let (width, height, pixels) = read_layer(&mut engine, layer);
        assert_eq!((width, height), (2, 2));
        assert_eq!(
            pixels,
            vec![
                1, 10, 5, 255, //
                2, 20, 6, 128, //
                3, 10, 7, 64, //
                4, 10, 8, 32,
            ]
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `getLayerDataMask` on a layer without a mask channel builds the 1x1
    /// dummy in `defaultMaskColor` (`psdclass.cpp:356-360`, `:394-396`).
    #[test]
    fn a_zero_sized_mask_becomes_a_dummy_pixel() {
        let (root, mut engine) = fixture_engine("mask");
        install_layer_harness(&mut engine, "psd.getLayerDataMask(layer, 0);\nreturn \"\";");
        let layer = engine
            .tjs_runtime()
            .global_member("layer")
            .object_handle()
            .expect("layer");
        let (width, height, pixels) = read_layer(&mut engine, layer);
        assert_eq!((width, height), (1, 1));
        // The fixture's mask default colour is 0 (`psdclass.cpp:394-396`
        // writes the colour into B, G, R and 255 into A).
        assert_eq!(pixels, vec![0, 0, 0, 255]);
        assert_eq!(
            probe_value(&mut engine, "return layer.defaultMaskColor;"),
            Variant::Integer(0)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `getBlend` reads the merged image (`psdclass.cpp:526-542`) and resizes
    /// the target to the document size.
    #[test]
    fn the_merged_image_is_written_by_get_blend() {
        let (root, mut engine) = fixture_engine("blend");
        install_layer_harness(&mut engine, "return psd.getBlend(layer) + \"\";");
        let layer = engine
            .tjs_runtime()
            .global_member("layer")
            .object_handle()
            .expect("layer");
        let (width, height, pixels) = read_layer(&mut engine, layer);
        assert_eq!((width, height), (4, 4));
        // Pixel (1, 2): R = 1 + 8, G = 100 + 1, B = 200 - 2, alpha opaque.
        let index = (2 * 4 + 1) * 4;
        assert_eq!(pixels[index..index + 4], [9, 101, 198, 255]);
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The error texts the reference raises (`psdclass.cpp:179-188`, `:333`,
    /// `:340-344`).
    #[test]
    fn error_texts_match_the_reference() {
        let (root, mut engine) = fixture_engine("errors");
        let text = probe(
            &mut engine,
            "var texts = [];\n\
             var psd = new PSD();\n\
             try { psd.getLayerType(0); texts.add(\"ok\"); } catch (e) { texts.add(e.message); }\n\
             psd.load(\"fixture.psd\");\n\
             try { psd.getLayerType(9); texts.add(\"ok\"); } catch (e) { texts.add(e.message); }\n\
             try { psd.getLayerData(%[not => \"a layer\"], 0); texts.add(\"ok\"); } catch (e) { texts.add(e.message); }\n\
             try { psd.getLayerData(null, 0); texts.add(\"ok\"); } catch (e) { texts.add(e.message); }\n\
             try { psd.getLayerData(psd, 0); texts.add(\"ok\"); } catch (e) { texts.add(e.message); }\n\
             global.window = new Window();\n\
             global.layer = new Layer(window, null);\n\
             try { psd.getLayerData(layer, 2); texts.add(\"ok\"); } catch (e) { texts.add(e.message); }\n\
             try { psd.getLayerData(layer, 3); texts.add(\"ok\"); } catch (e) { texts.add(e.message); }\n\
             var joined = \"\";\n\
             for (var i = 0; i < texts.count; i++) { joined += texts[i] + \",\"; }\n\
             return joined;",
        );
        assert_eq!(
            text,
            "no data,not such layer,not layer,not layer,not layer,invalid layer type,invalid layer type,"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The slice and guide resources (`psdresource.cpp:9-94`) surface through
    /// `getSlices`/`getGuides`; `getLayerComp` answers `void` (the documented
    /// descriptor gap).
    #[test]
    fn slice_and_guide_resources_are_reported() {
        let (root, mut engine) = fixture_engine("resources");
        let text = probe(
            &mut engine,
            "var psd = new PSD();\n\
             psd.load(\"fixture.psd\");\n\
             var guides = psd.getGuides();\n\
             var slices = psd.getSlices();\n\
             return guides.horz_grid + \",\" + guides.vert_grid + \",\" + guides.vertical.count + \":\" + \
                 guides.vertical[0] + \",\" + guides.horizontal.count + \":\" + guides.horizontal[0] + \";\" + \
                 slices.left + \",\" + slices.top + \",\" + slices.right + \",\" + slices.bottom + \",\" + \
                 slices.name + \",\" + slices.slices.count + \";\" + typeof psd.getLayerComp();",
        );
        assert_eq!(text, "8,16,1:5,1:7;0,0,4,4,group,0;void");
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The `psd://` media (`main.cpp:98-135`, `psdclass.cpp:688-754`): the
    /// root path walks the folder parents, the `id/` form addresses a layer
    /// by its `lyid`, and a missing layer is not found.
    #[test]
    fn the_storage_media_serves_layer_bitmaps() {
        let (root, mut engine) = fixture_engine("media");
        // The folder is the parent of the three layers before it, so every
        // layer path lives under `root/folder/`.
        let exists = probe(
            &mut engine,
            "return Storages.isExistentStorage(\"psd://fixture.psd/root/folder/背景.bmp\") + \":\" + \
                 Storages.isExistentStorage(\"psd://fixture.psd/root/folder/overlay.bmp\") + \":\" + \
                 Storages.isExistentStorage(\"psd://fixture.psd/root/overlay.bmp\") + \":\" + \
                 Storages.isExistentStorage(\"psd://fixture.psd/id/7.bmp\") + \":\" + \
                 Storages.isExistentStorage(\"psd://fixture.psd/id/999.bmp\");",
        );
        assert_eq!(exists, "1:1:0:1:0");
        let listing = |engine: &mut KrkrEngine, directory: &str| {
            probe(
                engine,
                &format!(
                    "var entries = Storages.dirlist({directory:?});\n\
                     var text = \"\";\n\
                     for (var i = 0; i < entries.count; i++) {{ text += entries[i] + \",\"; }}\n\
                     return text;"
                ),
            )
        };
        assert_eq!(
            listing(&mut engine, "psd://fixture.psd/root/folder/"),
            "overlay.bmp,背景.bmp,"
        );
        assert_eq!(
            listing(&mut engine, "psd://fixture.psd/id/"),
            "7.bmp,42.bmp,"
        );

        // The bitmap is a 32-bit bottom-up BMP of the masked image; its first
        // pixel is the *bottom* row of layer 0 (`psdclass.cpp:761-822`).
        let storage = engine.host().project_storage().expect("storage");
        let bytes = storage
            .data("psd://fixture.psd/root/folder/背景.bmp")
            .expect("bmp");
        let bytes = bytes.as_bytes().expect("bytes");
        assert_eq!(&bytes[..2], b"BM");
        assert_eq!(
            u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]),
            2
        );
        assert_eq!(
            u32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]),
            2
        );
        assert_eq!(u16::from_le_bytes([bytes[28], bytes[29]]), 32);
        assert_eq!(&bytes[54..58], &[48, 112, 176, 255]);
        assert_eq!(&bytes[58..62], &[64, 128, 192, 255]);

        // A missing layer takes the media's own failure, whose message is the
        // reference's `%1:cannot open psdfile` (`main.cpp:124`). The media is
        // probed directly, with the same root attached, because the name it
        // owns only exists inside the scheme's name space.
        let port: std::sync::Arc<dyn plugin_api::ProjectStoragePort> =
            std::sync::Arc::new(ProjectStorage::for_root(&root).expect("storage"));
        let media = super::PsdMedia::new();
        plugin_api::StorageMediaProvider::attach_storage(&media, std::sync::Arc::downgrade(&port));
        let missing =
            plugin_api::StorageMediaProvider::open(&media, "fixture.psd/root/folder/absent.bmp")
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default();
        assert!(
            missing.contains("cannot open psdfile"),
            "unexpected error {missing}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `load` answers false for a missing file and for a document the parser
    /// rejects (`psdclass.cpp:153-156`).
    #[test]
    fn loading_invalid_documents_answers_false() {
        let root = test_root("invalid");
        fs::write(root.join("garbage.psd"), b"not a psd at all").expect("write");
        let mut engine = test_engine(&root);
        let text = probe(
            &mut engine,
            "var psd = new PSD();\n\
             var missing = psd.load(\"absent.psd\");\n\
             var garbage = psd.load(\"garbage.psd\");\n\
             return missing + \":\" + garbage + \":\" + psd.width + \":\" + psd.layer_count;",
        );
        assert_eq!(text, "0:0:-1:-1");
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Reads a layer's plane back through the engine's bitmap view.
    fn read_layer(engine: &mut KrkrEngine, layer: ObjectHandle) -> (u32, u32, Vec<u8>) {
        plugin_api::layer::layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            (view.bitmap.width, view.bitmap.height, view.pixels.to_vec())
        })
        .expect("layer bitmap")
    }
}
