//! `AlphaMovie.dll` — the alpha-channel `.amv` movie player (`AlphaMovie` class).
//!
//! Reference: T.Imoto's krkrZ reimplementation of the original
//! (closed-source) kaede-software plugin — `AlphaMovie.cpp` (784 lines) and
//! `manual.tjs` from the upstream tree this port was given. The plugin is a
//! *distinct player*, not a wrapper around the engine's `VideoOverlay`: it
//! decodes the `.amv` container itself and blits each frame — with its per
//! pixel alpha — directly into a script-supplied `Layer`'s main image
//! (`AlphaMovie.cpp:709-733`), at an arbitrary position, whereas
//! `VideoOverlay` presents top-most texture quads decoded by the platform
//! video backend. Neither the container nor its codec is a standard one, so
//! the engine's `krkr-video` backends cannot serve it and the decoder lives
//! here.
//!
//! # Surface (`AlphaMovie.cpp:757-784`, `manual.tjs`)
//!
//! ```text
//! new AlphaMovie()
//! open(filename)                    parse the header and index the frames
//! showNextImage(layer) -> int       decode the current frame into `layer`,
//!                                   return its number, advance, wrap / hold /
//!                                   switch to setNextMovieFile at the end
//! clear()                           no-op (the reference decodes synchronously)
//! isPlaying() -> bool               play()/stop() toggle the flag
//! setPosition(x, y)                 blit origin; also the left/top members
//! setNextMovieFile(filename)        the movie played after the current one
//! numOfFrame (ro)  frame (rw)  loop (rw)  nextLoop (rw)  preloadSamples (rw, 5)
//! left (rw)  top (rw)  screenWidth (ro)  screenHeight (ro)
//! FPSScale (ro)  FPSRate (ro)
//! ```
//!
//! `showNextImage` is the whole engine of the plugin (`AlphaMovie.cpp:467-494`):
//!
//! * no frames → `AlphaMovie: no movie opened.`;
//! * past the last frame: a pending `setNextMovieFile` opens that movie and
//!   adopts `nextLoop`, else `loop` rewinds to the first frame, else the
//!   index stays on the last frame;
//! * the current frame is decoded and copied into the target layer (the
//!   frame's own rect offset by `left`/`top`, clipped to the layer);
//! * the frame's *number* (the `FRAM` chunk's own id, not its index) is
//!   returned and the index advances.
//!
//! # The `.amv` format (`readme.txt` §omv, `AlphaMovie.cpp:382-449`)
//!
//! `'AJPM'` header (magic, revision 0, header size, frame count, FPS scale /
//! rate, screen size, attribute flags) followed by two or three 64-byte
//! zig-zag quantisation tables and a chain of `'FRAM'` chunks. Each frame is
//! one of:
//!
//! * **JPEG alpha** (attribute bit 0): one baseline-JPEG entropy scan of four
//!   interleaved components — `Cb`, `Cr` (1x1, chroma tables), `Y`, `A` (2x2
//!   each, luma tables) at 4:2:0 — and no JPEG tables of its own;
//! * **zlib alpha** (attribute bit 1): the alpha plane is a zlib stream, the
//!   colour is the same scan with three components (`Cb`, `Cr`, `Y`).
//!
//! The codec's **DC prediction is non-standard**: predictors are shared per
//! Huffman table (one for `Y`+`A`, one for `Cb`+`Cr`) and reset once per
//! frame, where baseline JPEG keeps one per component (`AlphaMovie.cpp:15-24`,
//! `:284-305`). The decoder here reproduces that exactly, plus the reference's
//! own separable f64 IDCT with the `+128.5` level shift (`:210-229`) and its
//! standard Huffman tables and integer YCbCr conversion (`:69-109, :611-623`).
//!
//! # Port decisions
//!
//! * The reference holds an `iTJSBinaryStream` and seeks per frame; this port
//!   reads the whole file at `open` (`KrkrHost::read_binary_storage`, so the
//!   name resolves through project storage and XP3 like any `Storages.*`
//!   path) and decodes from the bytes. The `open` failure is the reference's
//!   `AlphaMovie: cannot open storage.`; a short read inside the format is
//!   `AlphaMovie: read error.` (`:553-558`).
//! * Frames are composed as **R,G,B,A** — the engine's layer byte order
//!   (`plugin_api::layer`) — where the reference's `memcpy`'d buffer is B,G,R,A
//!   (`dst[0] = b`, `:619-622`); the blit copies 4 bytes per pixel including
//!   alpha, clipped, with no blending, exactly like the reference (which means
//!   a premultiplied layer type receives the same raw bytes its unchecked
//!   `mainImageBufferForWrite` write would have produced — the reference has
//!   no premultiply step either).
//! * The layer is reached through [`krkr_engine::plugin_api::layer`] scoped
//!   views instead of `mainImageBufferForWrite`; a layer without an image is
//!   the reference's `AlphaMovie: target must be a Layer with image.`
//!   (`AlphaMovie.cpp:709-716`), and a non-object argument is the callback's
//!   `AlphaMovie: showNextImage requires a Layer.` (`:749`).
//! * `preloadSamples` is stored and reported (`setPreloadSamples`,
//!   `:518-519`); the reference never reads it either — the decoder is
//!   synchronous and `clear` is a no-op (`:500`).
//! * The zlib-alpha path needs a raw inflate; the plugin crate has no
//!   compression dependency, so the module carries its own RFC 1950/1951
//!   decoder (stored, fixed and dynamic blocks, Adler-32 verified) rather
//!   than widening the crate's dependency set. A stream that fails to inflate
//!   produces the reference's fallback: a fully opaque alpha plane
//!   (`:676-681`).

use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{LayerBitmapViewMut, layer_bitmap_read, layer_bitmap_write},
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "AlphaMovie class (.amv alpha-channel movie: player that decodes into an arbitrary Layer)",
    notes: "Real: header/frame-table parsing, both frame paths (JPEG-alpha's non-standard shared-DC-predictor baseline decode and zlib-alpha with the module's own inflate), the layer blit with clipping, and every member of the reference surface (open/clear/showNextImage/isPlaying/play/stop/setPosition/setNextMovieFile and the 11 properties). Divergences: the whole file is read at open instead of seeked per frame; the composed buffer is R,G,B,A (the engine's order) where the reference is B,G,R,A; the reference's unchecked reads are guarded, with its own error texts; preloadSamples is stored only (the reference never reads it either). The surface is the krkrZ reimplementation's, which matches the closed-source binary by construction; nothing was verified against the original DLL.",
    install: |engine| engine.register_plugin(AlphaMoviePlugin),
};

/// Canonical DLL name: what [`KrkrPlugin::name`] reports and what a game's
/// `Plugins.link("AlphaMovie.dll")` (or the `nene.dll` alias the catalog
/// resolves) selects through [`crate::catalog`]. The reference registers one
/// class, `AlphaMovie` (`AlphaMovie.cpp:759`); `nene.dll` is the same binary
/// under another name, not a second class.
pub(crate) const NAME: &str = "AlphaMovie.dll";

/// The class the reference registers (`NCB_REGISTER_CLASS(AlphaMovie)`).
const CLASS_NAME: &str = "AlphaMovie";

const AMV_MAGIC: u32 = 0x4d50_4a41; // 'AJPM' little-endian (`AlphaMovie.cpp:44`)
const FRAM_MAGIC: u32 = 0x4d41_5246; // 'FRAM' (`:45`)
const HEADER_LEN: usize = 0x28; // (`:383-431`)

pub struct AlphaMoviePlugin;

impl KrkrPlugin for AlphaMoviePlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_alpha_movie(runtime);
        Ok(())
    }
}

/// One movie's state — the reference's C++ members (`AlphaMovie.cpp:529-550`)
/// plus the bytes the port decodes from.
struct AlphaMovieState {
    /// The whole `.amv` file (the reference's seekable stream).
    data: Option<Arc<[u8]>>,
    /// `FirstFrameOfs`: the header size, where the `FRAM` chain starts.
    first_frame_offset: usize,
    num_of_frame: u32,
    width: u16,
    height: u16,
    fps_scale: u32,
    fps_rate: u32,
    /// `IsZlibAlpha`: attribute bit 1 instead of bit 0 (`:410-417`).
    zlib_alpha: bool,
    /// Up to three 64-byte zig-zag quantisation tables (`:422-430`).
    quant: [[u8; 64]; 3],
    frames: Vec<FrameInfo>,
    /// `CurrentIndex` — the frame `showNextImage` decodes next.
    current_index: i64,
    left: i32,
    top: i32,
    /// `Loop`.
    loop_playback: bool,
    /// `NextLoop`: adopted by the movie `setNextMovieFile` switches to.
    next_loop: bool,
    /// `PreloadSamples` (default 5, `:349, 518-519`); stored only.
    preload_samples: i64,
    playing: bool,
    next_movie_file: String,
}

impl Default for AlphaMovieState {
    /// The constructor's initialisation list (`AlphaMovie.cpp:344-350`).
    fn default() -> Self {
        Self {
            data: None,
            first_frame_offset: 0,
            num_of_frame: 0,
            width: 0,
            height: 0,
            fps_scale: 0,
            fps_rate: 0,
            zlib_alpha: false,
            quant: [[0; 64]; 3],
            frames: Vec::new(),
            current_index: 0,
            left: 0,
            top: 0,
            loop_playback: false,
            next_loop: false,
            preload_samples: 5,
            playing: false,
            next_movie_file: String::new(),
        }
    }
}

/// One `FRAM` chunk: where it starts and the frame number it carries
/// (`AlphaMovie.cpp:531`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameInfo {
    offset: usize,
    number: u32,
}

thread_local! {
    /// Per-object state, the reference's native instance data. Keyed by the
    /// TJS object handle; TJS runs on one thread, and handles are never reused
    /// by the runtime, so the map is exact for the life of the process.
    static MOVIES: RefCell<BTreeMap<ObjectHandle, AlphaMovieState>> =
        const { RefCell::new(BTreeMap::new()) };
}

fn install_alpha_movie(runtime: &mut Runtime<KrkrHost>) {
    // A script-provided `AlphaMovie` class (or an earlier registration of this
    // plugin) wins; the reference's registration is all-or-nothing too.
    if runtime.global_member(CLASS_NAME).object_handle().is_some() {
        return;
    }
    let class = runtime.alloc_native_constructor_with_arg_count(
        NativeArgCount::Any,
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = bound_instance(runtime, this_obj);
            MOVIES.with(|movies| {
                movies.borrow_mut().entry(instance).or_default();
            });
            install_alpha_movie_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, CLASS_NAME);
    install_alpha_movie_members(runtime, class);
    runtime.set_global_member(CLASS_NAME, Variant::Object(class));
    runtime
        .host_mut()
        .log("AlphaMovie: AlphaMovie class registered (.amv decode, both frame paths, Layer blit)");
}

fn bound_instance(runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> ObjectHandle {
    let instance = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(instance, CLASS_NAME);
    instance
}

/// The object a call acts on: the bound instance, else the object the member
/// was registered on (a method called on the class object).
fn receiver(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    default_object: ObjectHandle,
) -> ObjectHandle {
    this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or(default_object)
}

/// Borrows the state of `object`, creating the constructor's default first.
fn with_state<R>(object: &ObjectHandle, read: impl FnOnce(&AlphaMovieState) -> R) -> R {
    MOVIES.with(|movies| {
        let mut movies = movies.borrow_mut();
        read(movies.entry(*object).or_default())
    })
}

/// Mutates the state of `object`, creating the constructor's default first.
fn with_state_mut<R>(object: &ObjectHandle, update: impl FnOnce(&mut AlphaMovieState) -> R) -> R {
    MOVIES.with(|movies| {
        let mut movies = movies.borrow_mut();
        update(movies.entry(*object).or_default())
    })
}

/// Takes the state out of the map for a call that needs `&mut Runtime` while
/// it works (the reference's native methods do the same implicitly: the C++
/// object is borrowed for the call). A re-entrant call sees no state.
fn take_state(object: ObjectHandle) -> AlphaMovieState {
    MOVIES
        .with(|movies| movies.borrow_mut().remove(&object))
        .unwrap_or_default()
}

fn put_state(object: ObjectHandle, state: AlphaMovieState) {
    MOVIES.with(|movies| {
        movies.borrow_mut().insert(object, state);
    });
}

// ------------------------------------------------------------------ members

/// Registers the reference's whole surface on `handle` — the class object and
/// every instance, the way `tTJSNativeClass::FuncCall` copies a native class's
/// members onto an instance (`classes.rs:80-83`).
fn install_alpha_movie_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native_with_arg_count(
        handle,
        "open",
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| { alpha_movie_open(runtime, handle, this_obj, args) },
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "clear",
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            let this = receiver(runtime, this_obj, handle);
            with_state_mut(&this, |_state| ());
            Ok(Variant::Void)
        },
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "isPlaying",
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            let this = receiver(runtime, this_obj, handle);
            Ok(Variant::Integer(i64::from(with_state(&this, |state| {
                state.playing
            }))))
        },
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "play",
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            let this = receiver(runtime, this_obj, handle);
            with_state_mut(&this, |state| {
                state.current_index = 0; // `AlphaMovie.cpp:497`
                state.playing = true;
            });
            Ok(Variant::Void)
        },
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "stop",
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            let this = receiver(runtime, this_obj, handle);
            with_state_mut(&this, |state| state.playing = false); // `:498`
            Ok(Variant::Void)
        },
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setPosition",
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            let this = receiver(runtime, this_obj, handle);
            let x = args
                .first()
                .map(Variant::to_integer)
                .transpose()?
                .unwrap_or(0);
            let y = args
                .get(1)
                .map(Variant::to_integer)
                .transpose()?
                .unwrap_or(0);
            with_state_mut(&this, |state| {
                state.left = x as i32; // `:502`
                state.top = y as i32;
            });
            Ok(Variant::Void)
        },
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setNextMovieFile",
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            let this = receiver(runtime, this_obj, handle);
            let name = args
                .first()
                .map(Variant::to_tjs_string)
                .transpose()?
                .unwrap_or_default();
            with_state_mut(&this, |state| state.next_movie_file = name); // `:503`
            Ok(Variant::Void)
        },
    );
    // `RawCallback` with `numparams < 1 → TJS_E_BADPARAMCOUNT` (`.cpp:744`).
    runtime.register_object_native_with_arg_count(
        handle,
        "showNextImage",
        NativeArgCount::AtLeast(1),
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            alpha_movie_show_next_image(runtime, handle, this_obj, args)
        },
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "finalize",
        NativeArgCount::Any,
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              _args: Vec<Variant>| {
            let this = receiver(runtime, this_obj, handle);
            MOVIES.with(|movies| {
                movies.borrow_mut().remove(&this);
            });
            Ok(Variant::Void)
        },
    );

    // `Property(numOfFrame, get, (int)0)` and friends (`:773-783`): no setter.
    register_property(
        runtime,
        handle,
        "numOfFrame",
        |state| i64::from(state.num_of_frame),
        None,
    );
    register_property(
        runtime,
        handle,
        "screenWidth",
        |state| i64::from(state.width),
        None,
    );
    register_property(
        runtime,
        handle,
        "screenHeight",
        |state| i64::from(state.height),
        None,
    );
    register_property(
        runtime,
        handle,
        "FPSScale",
        |state| i64::from(state.fps_scale),
        None,
    );
    register_property(
        runtime,
        handle,
        "FPSRate",
        |state| i64::from(state.fps_rate),
        None,
    );
    register_property(
        runtime,
        handle,
        "frame",
        |state| state.current_index,
        Some(|state, value| {
            let value = value.to_integer()?;
            // `setFrame` clamps to [0, size-1] (`:508-513`).
            if state.frames.is_empty() {
                state.current_index = 0;
                return Ok(());
            }
            let last = state.frames.len() as i64 - 1;
            state.current_index = value.clamp(0, last);
            Ok(())
        }),
    );
    register_property(
        runtime,
        handle,
        "loop",
        |state| i64::from(state.loop_playback),
        Some(|state, value| {
            state.loop_playback = value.is_truthy(); // `:514-515`
            Ok(())
        }),
    );
    register_property(
        runtime,
        handle,
        "nextLoop",
        |state| i64::from(state.next_loop),
        Some(|state, value| {
            state.next_loop = value.is_truthy(); // `:516-517`
            Ok(())
        }),
    );
    register_property(
        runtime,
        handle,
        "preloadSamples",
        |state| state.preload_samples,
        Some(|state, value| {
            state.preload_samples = value.to_integer()?; // `:518-519`
            Ok(())
        }),
    );
    register_property(
        runtime,
        handle,
        "left",
        |state| i64::from(state.left),
        Some(|state, value| {
            state.left = value.to_integer()? as i32; // `:520-521`
            Ok(())
        }),
    );
    register_property(
        runtime,
        handle,
        "top",
        |state| i64::from(state.top),
        Some(|state, value| {
            state.top = value.to_integer()? as i32; // `:522-523`
            Ok(())
        }),
    );
}

/// One integer property over the state. `write: None` is the reference's
/// setter-less `Property(..., (int)0)`: a script write fails with
/// `TJS_E_ACCESSDENYED`, the engine's representation of a missing setter.
fn register_property(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    name: &'static str,
    read: fn(&AlphaMovieState) -> i64,
    write: Option<fn(&mut AlphaMovieState, Variant) -> Result<()>>,
) {
    let access = if write.is_some() {
        NativePropertyAccess::ReadWrite
    } else {
        NativePropertyAccess::ReadOnly
    };
    runtime.register_object_native_property_with_access(
        handle,
        name,
        access,
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let object = receiver(runtime, this_obj, handle);
            Ok(Variant::Integer(with_state(&object, read)))
        },
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            let Some(write) = write else {
                return Ok(());
            };
            let object = receiver(runtime, this_obj, handle);
            with_state_mut(&object, |state| write(state, value))
        },
    );
}

// ----------------------------------------------------------------- the calls

/// `open(filename)` (`AlphaMovie.cpp:365-380`): close the current movie, open
/// the storage, parse the header and index the frames.
fn alpha_movie_open(
    runtime: &mut Runtime<KrkrHost>,
    default_object: ObjectHandle,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let this = receiver(runtime, this_obj, default_object);
    let bytes = runtime
        .host()
        .read_binary_storage(&name)
        .map_err(|_| TjsError::runtime("AlphaMovie: cannot open storage."))?;
    let mut state = take_state(this);
    let result = load_movie(&mut state, bytes);
    put_state(this, state);
    result?;
    Ok(Variant::Void)
}

/// The `open` body: `closeStream(); Frames.clear(); CurrentIndex = 0;
/// Playing = false;` then parse (`AlphaMovie.cpp:365-380`). A parse failure
/// leaves the stream closed but the fields parsed so far, exactly like the
/// reference's assign-as-you-go members.
fn load_movie(state: &mut AlphaMovieState, bytes: Vec<u8>) -> Result<()> {
    state.data = None;
    state.frames.clear();
    state.current_index = 0;
    state.playing = false;

    let bytes: Arc<[u8]> = Arc::from(bytes);
    state.data = Some(Arc::clone(&bytes));
    let result = parse_header(state, &bytes).and_then(|()| scan_frames(state, &bytes));
    if result.is_err() {
        state.data = None;
    }
    result
}

/// Reads the movie `name` and replaces `state` with it (the reference's
/// `open` inside `showNextImage` for `setNextMovieFile`, `:471-477`).
fn open_movie(
    runtime: &mut Runtime<KrkrHost>,
    state: &mut AlphaMovieState,
    name: &str,
) -> Result<()> {
    let bytes = runtime
        .host()
        .read_binary_storage(name)
        .map_err(|_| TjsError::runtime("AlphaMovie: cannot open storage."))?;
    load_movie(state, bytes)
}

fn parse_header(state: &mut AlphaMovieState, data: &[u8]) -> Result<()> {
    let header = data.get(..HEADER_LEN).ok_or_else(read_error)?;
    if rd_u32(header, 0x00) != AMV_MAGIC {
        return Err(TjsError::runtime("This file is not Alpha Movie File."));
    }
    if rd_u32(header, 0x08) != 0 {
        return Err(TjsError::runtime("Invalid File revision number."));
    }
    let header_size = rd_u32(header, 0x0c);
    if header_size < HEADER_LEN as u32 {
        return Err(TjsError::runtime("Invalid header size."));
    }
    let quant_size = header_size - HEADER_LEN as u32;
    if quant_size != 0x80 && quant_size != 0xc0 {
        return Err(TjsError::runtime("Invalid Quantaization table size."));
    }
    state.first_frame_offset = header_size as usize;

    state.num_of_frame = rd_u32(header, 0x14);
    if state.num_of_frame == 0 {
        return Err(TjsError::runtime("Not found frame in this file."));
    }
    state.fps_scale = rd_u32(header, 0x18);
    state.fps_rate = rd_u32(header, 0x1c);
    if state.fps_scale == 0 || state.fps_rate == 0 {
        return Err(TjsError::runtime("Invalid frame rate."));
    }
    state.width = rd_u16(header, 0x20);
    state.height = rd_u16(header, 0x22);
    if state.width == 0 || state.height == 0 {
        return Err(TjsError::runtime("Screen size is zero ?"));
    }
    let flags = rd_u32(header, 0x24);
    if flags & 1 != 0 {
        state.zlib_alpha = false;
    } else if flags & 2 != 0 {
        state.zlib_alpha = true;
    } else {
        return Err(TjsError::runtime("Invalid Attribute."));
    }
    if (state.zlib_alpha && quant_size != 0x80) || (!state.zlib_alpha && quant_size != 0xc0) {
        return Err(TjsError::runtime("Invalid Quantaization table size."));
    }

    let quant = data
        .get(HEADER_LEN..HEADER_LEN + quant_size as usize)
        .ok_or_else(read_error)?;
    for (table, target) in state.quant.iter_mut().take(3).enumerate() {
        let range = table * 64..(table + 1) * 64;
        match quant.get(range) {
            Some(bytes) => target.copy_from_slice(bytes),
            None => *target = [0; 64], // the zlib path has no alpha table (`:429-430`)
        }
    }
    Ok(())
}

/// `scanFrames` (`AlphaMovie.cpp:434-449`): walk `NumOfFrame` `FRAM` chunks,
/// each advancing by its own size.
fn scan_frames(state: &mut AlphaMovieState, data: &[u8]) -> Result<()> {
    state.frames.clear();
    let mut offset = state.first_frame_offset;
    for _ in 0..state.num_of_frame {
        let header = data
            .get(offset..offset + 12)
            .ok_or_else(|| TjsError::runtime("AlphaMovie: read error."))?;
        if rd_u32(header, 0) != FRAM_MAGIC {
            return Err(TjsError::runtime("File format error."));
        }
        let size = rd_u32(header, 4) as usize;
        let number = rd_u32(header, 8);
        state.frames.push(FrameInfo { offset, number });
        offset = offset
            .checked_add(size + 8)
            .ok_or_else(|| TjsError::runtime("File format error."))?;
    }
    Ok(())
}

/// `AlphaMovie_showNextImage` (`AlphaMovie.cpp:739-754`) plus the layer check
/// its `getLayerWriteBuffer` performs before decoding (`:709-716`).
fn alpha_movie_show_next_image(
    runtime: &mut Runtime<KrkrHost>,
    default_object: ObjectHandle,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(layer) = args.first().and_then(Variant::object_handle) else {
        return Err(TjsError::runtime(
            "AlphaMovie: showNextImage requires a Layer.",
        ));
    };
    // `getLayerWriteBuffer`: the target must be a Layer with a main image
    // (`:314-337, 709-716`); the engine's scoped probe is that check.
    if layer_bitmap_read(runtime, layer, |_| ()).is_err() {
        return Err(TjsError::runtime(
            "AlphaMovie: target must be a Layer with image.",
        ));
    }
    let this = receiver(runtime, this_obj, default_object);
    let mut state = take_state(this);
    let result = show_next_image(runtime, layer, &mut state);
    put_state(this, state);
    result
}

fn show_next_image(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    state: &mut AlphaMovieState,
) -> Result<Variant> {
    if state.frames.is_empty() {
        return Err(TjsError::runtime("AlphaMovie: no movie opened."));
    }
    if state.current_index >= state.frames.len() as i64 {
        // `AlphaMovie.cpp:470-483`: next movie, else loop, else hold the last
        // frame.
        if !state.next_movie_file.is_empty() {
            let next = std::mem::take(&mut state.next_movie_file);
            let next_loop = state.next_loop;
            open_movie(runtime, state, &next)?;
            state.loop_playback = next_loop;
            state.current_index = 0;
        } else if state.loop_playback {
            state.current_index = 0;
        } else {
            state.current_index = state.frames.len() as i64 - 1;
        }
    }

    let index = state.current_index as usize;
    let frame = decode_frame(state, index)?;
    let number = state.frames[index].number;
    let (left, top) = (state.left, state.top); // `:717` after `this.Left/Top`
    layer_bitmap_write(runtime, layer, |view| blit_frame(view, &frame, left, top))
        .map_err(|_| TjsError::runtime("AlphaMovie: target must be a Layer with image."))?;

    // `:491-492`: the *number* is returned and the index advances.
    state.current_index += 1;
    Ok(Variant::Integer(i64::from(number)))
}

// ----------------------------------------------------------------- decoding

/// One decoded frame in the engine's R,G,B,A order, with the rect the frame
/// declares (`AlphaMovie.cpp:452-464`).
struct DecodedFrame {
    pixels: Vec<u8>,
    left: i32,
    top: i32,
    width: usize,
    height: usize,
}

/// `decodeFrame` (`AlphaMovie.cpp:452-464`): both frame paths fill a buffer
/// the size of the frame and report its rect.
fn decode_frame(state: &AlphaMovieState, index: usize) -> Result<DecodedFrame> {
    if index >= state.frames.len() {
        return Err(TjsError::runtime("AlphaMovie: frame index out of range."));
    }
    let data = state
        .data
        .as_deref()
        .ok_or_else(|| TjsError::runtime("AlphaMovie: stream not opened."))?;
    let frame = state.frames[index];
    if state.zlib_alpha {
        decode_zlib_alpha(data, frame.offset, &state.quant)
    } else {
        decode_jpeg_alpha(data, frame.offset, &state.quant)
    }
}

/// The JPEG-alpha path (`AlphaMovie.cpp:560-625`): four interleaved
/// components `Cb, Cr, Y, A` at 4:2:0, composed to RGBA.
fn decode_jpeg_alpha(data: &[u8], offset: usize, quant: &[[u8; 64]; 3]) -> Result<DecodedFrame> {
    let header = data
        .get(offset..offset + 20)
        .ok_or_else(|| TjsError::runtime("AlphaMovie: read error."))?;
    if rd_u32(header, 0) != FRAM_MAGIC {
        return Err(format_error());
    }
    let size = rd_u32(header, 4) as usize;
    let left = i32::from(rd_s16(header, 12));
    let top = i32::from(rd_s16(header, 14));
    let width = usize::from(rd_u16(header, 16));
    let height = usize::from(rd_u16(header, 18));
    // `:579`: the entropy covers whole MCUs, 16x16.
    if width & 0x0f != 0 || height & 0x0f != 0 || width == 0 || height == 0 {
        return Err(format_error());
    }
    // `:582-587`: `payloadLen = size - 12` bytes after the 20-byte header.
    let payload = size
        .checked_sub(12)
        .ok_or_else(|| TjsError::runtime("AlphaMovie: read error."))?;
    let entropy = data
        .get(offset + 20..offset + 20 + payload)
        .ok_or_else(|| TjsError::runtime("AlphaMovie: read error."))?;

    let huffman = standard_huffman();
    let mut components = [
        Component::new(1, 1, quant[1], 1, &huffman.dc_chroma, &huffman.ac_chroma), // Cb
        Component::new(1, 1, quant[1], 1, &huffman.dc_chroma, &huffman.ac_chroma), // Cr
        Component::new(2, 2, quant[0], 0, &huffman.dc_luma, &huffman.ac_luma),     // Y
        Component::new(2, 2, quant[2], 0, &huffman.dc_luma, &huffman.ac_luma),     // A
    ];
    decode_scan(entropy, width, height, &mut components);
    let [cb, cr, y, a] = components;
    let pixels = compose_rgba(width, height, &y, &cb, &cr, |x, row| {
        a.plane[row * a.plane_width + x]
    });
    Ok(DecodedFrame {
        pixels,
        left,
        top,
        width,
        height,
    })
}

/// The zlib-alpha path (`AlphaMovie.cpp:627-704`): a zlib-compressed 8-bit
/// alpha plane followed by the colour scan (`Cb, Cr, Y`).
fn decode_zlib_alpha(data: &[u8], offset: usize, quant: &[[u8; 64]; 3]) -> Result<DecodedFrame> {
    let header = data
        .get(offset..offset + 24)
        .ok_or_else(|| TjsError::runtime("AlphaMovie: read error."))?;
    if rd_u32(header, 0) != FRAM_MAGIC {
        return Err(format_error());
    }
    let size = rd_u32(header, 4) as usize;
    let left = i32::from(rd_s16(header, 12));
    let top = i32::from(rd_s16(header, 14));
    let width = usize::from(rd_u16(header, 16));
    let height = usize::from(rd_u16(header, 18));
    let alpha_len = rd_u32(header, 20) as usize;
    if width & 0x0f != 0 || height & 0x0f != 0 || width == 0 || height == 0 {
        return Err(format_error());
    }

    // `:648-661`: alpha first, colour = `size - alphaZlibSize - 16` bytes.
    let alpha_compressed = data
        .get(offset + 24..offset + 24 + alpha_len)
        .ok_or_else(|| TjsError::runtime("AlphaMovie: read error."))?;
    let color_len = size
        .checked_sub(alpha_len + 16)
        .ok_or_else(|| TjsError::runtime("AlphaMovie: read error."))?;
    let color = data
        .get(offset + 24 + alpha_len..offset + 24 + alpha_len + color_len)
        .ok_or_else(|| TjsError::runtime("AlphaMovie: read error."))?;

    let huffman = standard_huffman();
    let mut components = [
        Component::new(1, 1, quant[1], 1, &huffman.dc_chroma, &huffman.ac_chroma), // Cb
        Component::new(1, 1, quant[1], 1, &huffman.dc_chroma, &huffman.ac_chroma), // Cr
        Component::new(2, 2, quant[0], 0, &huffman.dc_luma, &huffman.ac_luma),     // Y
    ];
    decode_scan(color, width, height, &mut components);

    // `:674-681`: the inflated plane, or a fully opaque one when it fails.
    let alpha = inflate_zlib(alpha_compressed, width * height)
        .filter(|alpha| alpha.len() == width * height)
        .unwrap_or_else(|| vec![255u8; width * height]);

    let [cb, cr, y] = components;
    let pixels = compose_rgba(width, height, &y, &cb, &cr, |x, row| alpha[row * width + x]);
    Ok(DecodedFrame {
        pixels,
        left,
        top,
        width,
        height,
    })
}

/// `blitToLayer` (`AlphaMovie.cpp:709-733`): overwrite the frame's rect at
/// `left + frame.left, top + frame.top`, clipped to the layer.
fn blit_frame(view: &mut LayerBitmapViewMut<'_>, frame: &DecodedFrame, left: i32, top: i32) {
    let layer_width = view.bitmap.width as i32;
    let layer_height = view.bitmap.height as i32;
    let pitch = view.bitmap.pitch as usize;

    let mut dx = left + frame.left;
    let mut dy = top + frame.top;
    let (mut sx, mut sy) = (0i32, 0i32);
    let (mut copy_width, mut copy_height) = (frame.width as i32, frame.height as i32);
    if dx < 0 {
        sx = -dx;
        copy_width += dx;
        dx = 0;
    }
    if dy < 0 {
        sy = -dy;
        copy_height += dy;
        dy = 0;
    }
    if dx + copy_width > layer_width {
        copy_width = layer_width - dx;
    }
    if dy + copy_height > layer_height {
        copy_height = layer_height - dy;
    }
    if copy_width <= 0 || copy_height <= 0 {
        return;
    }

    for row in 0..copy_height as usize {
        let source = ((sy as usize + row) * frame.width + sx as usize) * 4;
        let destination = (dy as usize + row) * pitch + dx as usize * 4;
        let len = copy_width as usize * 4;
        let (Some(source), Some(destination)) = (
            frame.pixels.get(source..source + len),
            view.pixels.get_mut(destination..destination + len),
        ) else {
            return;
        };
        destination.copy_from_slice(source);
    }
}

// ------------------------------------------------------------------- format

fn read_error() -> TjsError {
    TjsError::runtime("AlphaMovie: read error.")
}

fn format_error() -> TjsError {
    TjsError::runtime("File format error.")
}

fn rd_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn rd_s16(data: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes([data[offset], data[offset + 1]])
}

fn rd_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn clip8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

// ------------------------------------------------------- baseline JPEG core

/// `ZIGZAG` (`AlphaMovie.cpp:54-63`): zig-zag index → natural position.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// The standard baseline Huffman tables (`AlphaMovie.cpp:69-109`); `.amv`
/// frames carry none of their own.
struct StandardHuffman {
    dc_luma: HuffTable,
    dc_chroma: HuffTable,
    ac_luma: HuffTable,
    ac_chroma: HuffTable,
}

fn standard_huffman() -> &'static StandardHuffman {
    static TABLES: OnceLock<StandardHuffman> = OnceLock::new();
    TABLES.get_or_init(|| StandardHuffman {
        dc_luma: HuffTable::build(
            &[0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        ),
        dc_chroma: HuffTable::build(
            &[0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        ),
        ac_luma: HuffTable::build(&STD_AC_LUMINANCE_BITS, &STD_AC_LUMINANCE_VAL),
        ac_chroma: HuffTable::build(&STD_AC_CHROMINANCE_BITS, &STD_AC_CHROMINANCE_VAL),
    })
}

const STD_AC_LUMINANCE_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const STD_AC_LUMINANCE_VAL: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
const STD_AC_CHROMINANCE_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const STD_AC_CHROMINANCE_VAL: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// A canonical Huffman decoding table, `mincode`/`maxcode`/`valptr` style
/// (`AlphaMovie.cpp:117-152`).
struct HuffTable {
    mincode: [i32; 17],
    maxcode: [i32; 18],
    valptr: [i32; 17],
    huffval: [u8; 256],
}

impl HuffTable {
    fn build(bits16: &[u8; 16], values: &[u8]) -> Self {
        let mut huffsize = [0usize; 257];
        let mut huffcode = [0i32; 257];
        let mut count = 0usize;
        for length in 1..=16usize {
            for _ in 0..bits16[length - 1] {
                huffsize[count] = length;
                count += 1;
            }
        }
        huffsize[count] = 0;
        let symbols = count;

        let mut code = 0i32;
        let mut size = huffsize[0];
        let mut index = 0usize;
        while huffsize[index] != 0 {
            while huffsize[index] == size {
                huffcode[index] = code;
                code += 1;
                index += 1;
            }
            code <<= 1;
            size += 1;
        }

        let mut table = HuffTable {
            mincode: [0; 17],
            maxcode: [-1; 18],
            valptr: [0; 17],
            huffval: [0; 256],
        };
        let mut position = 0usize;
        for length in 1..=16usize {
            if bits16[length - 1] != 0 {
                table.valptr[length] = position as i32;
                table.mincode[length] = huffcode[position];
                position += usize::from(bits16[length - 1]);
                table.maxcode[length] = huffcode[position - 1];
            }
        }
        table.maxcode[17] = i32::MAX;
        table.huffval[..symbols].copy_from_slice(&values[..symbols]);
        table
    }
}

/// The reference's bit reader (`AlphaMovie.cpp:154-186`): MSB-first within a
/// byte, `0xFF00` unstuffed, a marker (or exhaustion) reads as zeroes.
struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
    accumulator: u32,
    count: i32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            accumulator: 0,
            count: 0,
        }
    }

    fn bit(&mut self) -> i32 {
        if self.count == 0 {
            if self.position >= self.data.len() {
                return 0;
            }
            let byte = self.data[self.position];
            self.position += 1;
            if byte == 0xff && self.position < self.data.len() && self.data[self.position] == 0x00 {
                self.position += 1;
            }
            self.accumulator = u32::from(byte);
            self.count = 8;
        }
        self.count -= 1;
        ((self.accumulator >> self.count) & 1) as i32
    }

    fn bits(&mut self, count: i32) -> i32 {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit();
        }
        value
    }

    fn decode(&mut self, table: &HuffTable) -> i32 {
        let mut length = 1usize;
        let mut code = self.bit();
        while code > table.maxcode[length] {
            code = (code << 1) | self.bit();
            length += 1;
            if length > 16 {
                return 0;
            }
        }
        let index = table.valptr[length] + code - table.mincode[length];
        i32::from(table.huffval[index as usize])
    }
}

/// `receive_extend` (`AlphaMovie.cpp:188-193`).
fn receive_extend(reader: &mut BitReader<'_>, size: i32) -> i32 {
    if size == 0 {
        return 0;
    }
    let value = reader.bits(size);
    if value < (1 << (size - 1)) {
        value + (-1 << size) + 1
    } else {
        value
    }
}

/// One component's scan parameters and decoded plane (`AlphaMovie.cpp:231-239`).
struct Component {
    h: usize,
    v: usize,
    quant: [u8; 64],
    prediction_index: usize,
    dc: &'static HuffTable,
    ac: &'static HuffTable,
    plane_width: usize,
    plane_height: usize,
    plane: Vec<u8>,
}

impl Component {
    fn new(
        h: usize,
        v: usize,
        quant: [u8; 64],
        prediction_index: usize,
        dc: &'static HuffTable,
        ac: &'static HuffTable,
    ) -> Self {
        Self {
            h,
            v,
            quant,
            prediction_index,
            dc,
            ac,
            plane_width: 0,
            plane_height: 0,
            plane: Vec::new(),
        }
    }
}

/// `decodeScan` (`AlphaMovie.cpp:264-306`): the whole interleaved entropy
/// scan, with the two shared DC predictors reset per frame.
fn decode_scan(entropy: &[u8], width: usize, height: usize, components: &mut [Component]) {
    let hmax = components.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = components.iter().map(|c| c.v).max().unwrap_or(1);
    let mcu_width = width / (hmax * 8);
    let mcu_height = height / (vmax * 8);

    for component in components.iter_mut() {
        component.plane_width = mcu_width * component.h * 8;
        component.plane_height = mcu_height * component.v * 8;
        component.plane = vec![0; component.plane_width * component.plane_height];
    }

    let mut reader = BitReader::new(entropy);
    let mut predictors = [0i32; 2];
    let mut coefficients = [0i32; 64];
    let mut block = [0u8; 64];
    for my in 0..mcu_height {
        for mx in 0..mcu_width {
            for component in components.iter_mut() {
                for by in 0..component.v {
                    for bx in 0..component.h {
                        decode_block(
                            &mut reader,
                            component,
                            &mut predictors[component.prediction_index],
                            &mut coefficients,
                        );
                        idct8x8(&coefficients, &mut block);
                        let px = (mx * component.h + bx) * 8;
                        let py = (my * component.v + by) * 8;
                        for row in 0..8 {
                            let start = (py + row) * component.plane_width + px;
                            component.plane[start..start + 8]
                                .copy_from_slice(&block[row * 8..row * 8 + 8]);
                        }
                    }
                }
            }
        }
    }
}

/// `decodeBlock` (`AlphaMovie.cpp:242-262`): DC with the shared predictor,
/// then AC with ZRL runs and EOB.
fn decode_block(
    reader: &mut BitReader<'_>,
    component: &Component,
    predictor: &mut i32,
    coefficients: &mut [i32; 64],
) {
    coefficients.fill(0);
    let t = reader.decode(component.dc);
    let difference = receive_extend(reader, t);
    *predictor += difference;
    coefficients[0] = *predictor * i32::from(component.quant[0]);

    let mut k = 1usize;
    while k < 64 {
        let rs = reader.decode(component.ac);
        let run = rs >> 4;
        let size = rs & 15;
        if size == 0 {
            if run == 15 {
                k += 16; // ZRL
                continue;
            }
            break; // EOB
        }
        k += run as usize;
        if k > 63 {
            break;
        }
        let value = receive_extend(reader, size);
        coefficients[ZIGZAG[k]] = value * i32::from(component.quant[k]);
        k += 1;
    }
}

/// The cosine table `g_cosT` (`AlphaMovie.cpp:195-207`):
/// `0.5 * c(u) * cos((2x+1) * u * pi / 16)`.
fn cosine_table() -> &'static [[f64; 8]; 8] {
    static TABLE: OnceLock<[[f64; 8]; 8]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [[0.0f64; 8]; 8];
        for (x, row) in table.iter_mut().enumerate() {
            for (u, cell) in row.iter_mut().enumerate() {
                let cu = if u == 0 {
                    1.0 / std::f64::consts::SQRT_2
                } else {
                    1.0
                };
                *cell =
                    0.5 * cu * ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI / 16.0).cos();
            }
        }
        table
    })
}

/// `idct8x8` (`AlphaMovie.cpp:210-229`): the reference's separable f64 IDCT
/// with the level shift and clamp.
fn idct8x8(coefficients: &[i32; 64], output: &mut [u8; 64]) {
    let cos = cosine_table();
    let mut tmp = [0.0f64; 64];
    for v in 0..8 {
        let row = &coefficients[v * 8..v * 8 + 8];
        for x in 0..8 {
            let mut sum = 0.0;
            for u in 0..8 {
                sum += cos[x][u] * f64::from(row[u]);
            }
            tmp[v * 8 + x] = sum;
        }
    }
    for x in 0..8 {
        for y in 0..8 {
            let mut sum = 0.0;
            for v in 0..8 {
                sum += cos[y][v] * tmp[v * 8 + x];
            }
            output[y * 8 + x] = clip8((sum + 128.0 + 0.5).floor() as i32);
        }
    }
}

/// `:602-624` / `:683-703`: YCbCr + alpha → R,G,B,A in the engine's byte order
/// (the reference writes B,G,R,A).
fn compose_rgba(
    width: usize,
    height: usize,
    y: &Component,
    cb: &Component,
    cr: &Component,
    alpha_at: impl Fn(usize, usize) -> u8,
) -> Vec<u8> {
    let mut pixels = vec![0u8; width * height * 4];
    for row in 0..height {
        for column in 0..width {
            let yy = i32::from(y.plane[row * y.plane_width + column]);
            let cbv = i32::from(cb.plane[(row / 2) * cb.plane_width + column / 2]);
            let crv = i32::from(cr.plane[(row / 2) * cr.plane_width + column / 2]);
            let red = clip8(yy + ((91_881 * (crv - 128)) >> 16));
            let green = clip8(yy - ((22_554 * (cbv - 128) + 46_802 * (crv - 128)) >> 16));
            let blue = clip8(yy + ((116_130 * (cbv - 128)) >> 16));
            let offset = (row * width + column) * 4;
            pixels[offset] = red;
            pixels[offset + 1] = green;
            pixels[offset + 2] = blue;
            pixels[offset + 3] = alpha_at(column, row);
        }
    }
    pixels
}

// ------------------------------------------------------------------- inflate

/// The RFC 1950 (zlib) wrapper around [`inflate`]: what the reference's
/// `uncompress` does for the zlib-alpha plane (`AlphaMovie.cpp:676-681`).
fn inflate_zlib(input: &[u8], limit: usize) -> Option<Vec<u8>> {
    if input.len() < 6 {
        return None;
    }
    let (cmf, flg) = (input[0], input[1]);
    if cmf & 0x0f != 8 {
        return None; // CM must be deflate
    }
    if cmf >> 4 > 7 {
        return None; // window size above 32 KiB
    }
    if (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return None; // header check
    }
    if flg & 0x20 != 0 {
        return None; // preset dictionary
    }
    let mut output = Vec::new();
    inflate(&input[2..], limit, &mut output)?;
    let trailer = input.get(input.len() - 4..)?;
    let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    (adler32(&output) == expected).then_some(output)
}

/// Adler-32 (RFC 1950 §9).
fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// RFC 1951 inflate: stored, fixed and dynamic blocks, at most `limit` bytes
/// of output.
fn inflate(input: &[u8], limit: usize, output: &mut Vec<u8>) -> Option<()> {
    let mut reader = DeflateReader {
        data: input,
        position: 0,
        byte: 0,
        bits: 0,
    };
    loop {
        let last = reader.read_bits(1)?;
        match reader.read_bits(2)? {
            0 => {
                reader.align();
                let length = reader.read_bits(16)? as usize;
                let complement = reader.read_bits(16)?;
                if length != ((!complement) & 0xffff) as usize {
                    return None;
                }
                if output.len() + length > limit {
                    return None;
                }
                for _ in 0..length {
                    output.push(reader.read_byte()?);
                }
            }
            1 => {
                let (literal, distance) = fixed_trees();
                inflate_block(&mut reader, literal, distance, output, limit)?;
            }
            2 => {
                let literal_lengths = reader.read_bits(5)? as usize + 257;
                let distance_lengths = reader.read_bits(5)? as usize + 1;
                let code_lengths = reader.read_bits(4)? as usize + 4;
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut code_length_codes = [0u8; 19];
                for index in 0..code_lengths {
                    code_length_codes[ORDER[index]] = reader.read_bits(3)? as u8;
                }
                let code_table = InflateTree::build(&code_length_codes)?;
                let total = literal_lengths + distance_lengths;
                let mut lengths = vec![0u8; total];
                let mut index = 0usize;
                while index < total {
                    let symbol = code_table.decode(&mut reader)?;
                    match symbol {
                        0..=15 => {
                            lengths[index] = symbol as u8;
                            index += 1;
                        }
                        16 => {
                            if index == 0 {
                                return None;
                            }
                            let previous = lengths[index - 1];
                            let repeat = 3 + reader.read_bits(2)? as usize;
                            if index + repeat > total {
                                return None;
                            }
                            for _ in 0..repeat {
                                lengths[index] = previous;
                                index += 1;
                            }
                        }
                        17 => {
                            let repeat = 3 + reader.read_bits(3)? as usize;
                            if index + repeat > total {
                                return None;
                            }
                            index += repeat;
                        }
                        18 => {
                            let repeat = 11 + reader.read_bits(7)? as usize;
                            if index + repeat > total {
                                return None;
                            }
                            index += repeat;
                        }
                        _ => return None,
                    }
                }
                let literal = InflateTree::build(&lengths[..literal_lengths])?;
                let distance = InflateTree::build(&lengths[literal_lengths..])?;
                inflate_block(&mut reader, &literal, &distance, output, limit)?;
            }
            _ => return None,
        }
        if last == 1 {
            return Some(());
        }
    }
}

/// One compressed block: literal runs, length/distance pairs (copied byte by
/// byte, so overlaps repeat as the format requires), until end-of-block.
fn inflate_block(
    reader: &mut DeflateReader<'_>,
    literal: &InflateTree,
    distance: &InflateTree,
    output: &mut Vec<u8>,
    limit: usize,
) -> Option<()> {
    loop {
        let symbol = literal.decode(reader)?;
        if symbol < 256 {
            if output.len() >= limit {
                return None;
            }
            output.push(symbol as u8);
        } else if symbol == 256 {
            return Some(());
        } else {
            let (base, extra) = *LENGTH_TABLE.get(symbol as usize - 257)?;
            let length = base + reader.read_bits(extra)? as usize;
            let (base, extra) = *DISTANCE_TABLE.get(distance.decode(reader)? as usize)?;
            let distance = base + reader.read_bits(extra)? as usize;
            if distance == 0 || distance > output.len() {
                return None;
            }
            if output.len() + length > limit {
                return None;
            }
            for _ in 0..length {
                let byte = output[output.len() - distance];
                output.push(byte);
            }
        }
    }
}

/// RFC 1951 §3.2.5 length codes for symbols 257..=285.
const LENGTH_TABLE: [(usize, u32); 29] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 1),
    (13, 1),
    (15, 1),
    (17, 1),
    (19, 2),
    (23, 2),
    (27, 2),
    (31, 2),
    (35, 3),
    (43, 3),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 4),
    (115, 4),
    (131, 5),
    (163, 5),
    (195, 5),
    (227, 5),
    (258, 0),
];

/// RFC 1951 §3.2.5 distance codes.
const DISTANCE_TABLE: [(usize, u32); 30] = [
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 1),
    (7, 1),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 4),
    (49, 4),
    (65, 5),
    (97, 5),
    (129, 6),
    (193, 6),
    (257, 7),
    (385, 7),
    (513, 8),
    (769, 8),
    (1025, 9),
    (1537, 9),
    (2049, 10),
    (3073, 10),
    (4097, 11),
    (6145, 11),
    (8193, 12),
    (12289, 12),
    (16385, 13),
    (24577, 13),
];

/// The fixed Huffman tables (RFC 1951 §3.2.6).
fn fixed_trees() -> (&'static InflateTree, &'static InflateTree) {
    static TREES: OnceLock<(InflateTree, InflateTree)> = OnceLock::new();
    let trees = TREES.get_or_init(|| {
        let mut literal_lengths = [0u8; 288];
        for (index, length) in literal_lengths.iter_mut().enumerate() {
            *length = match index {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        let distance_lengths = [5u8; 30];
        (
            InflateTree::build(&literal_lengths).expect("the fixed literal table is valid"),
            InflateTree::build(&distance_lengths).expect("the fixed distance table is valid"),
        )
    });
    (&trees.0, &trees.1)
}

/// A canonical Huffman decoding table (counts/symbols style, RFC 1951 §3.2.2).
struct InflateTree {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl InflateTree {
    fn build(lengths: &[u8]) -> Option<Self> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            if length > 15 {
                return None; // not a valid code length
            }
            counts[length as usize] += 1;
        }
        // Reject oversubscribed codes (a longer code than the space allows).
        let mut left = 1i32;
        for &count in counts.iter().skip(1) {
            left <<= 1;
            left -= i32::from(count);
            if left < 0 {
                return None;
            }
        }
        let mut offsets = [0u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let symbols_len = lengths.iter().filter(|length| **length > 0).count();
        let mut symbols = vec![0u16; symbols_len];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length > 0 {
                symbols[offsets[length as usize] as usize] = symbol as u16;
                offsets[length as usize] += 1;
            }
        }
        Some(Self { counts, symbols })
    }

    fn decode(&self, reader: &mut DeflateReader<'_>) -> Option<u16> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0usize;
        for length in 1..=15usize {
            code |= reader.read_bit()? as i32;
            let count = i32::from(self.counts[length]);
            if code - first < count {
                return self.symbols.get(index + (code - first) as usize).copied();
            }
            index += count as usize;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

/// Deflate's LSB-first bit reader (RFC 1951 §3.1.1).
struct DeflateReader<'a> {
    data: &'a [u8],
    position: usize,
    byte: u32,
    bits: u32,
}

impl DeflateReader<'_> {
    fn read_bit(&mut self) -> Option<u32> {
        if self.bits == 0 {
            if self.position >= self.data.len() {
                return None;
            }
            self.byte = u32::from(self.data[self.position]);
            self.position += 1;
            self.bits = 8;
        }
        // DEFLATE packs bits into bytes lowest-first (RFC 1951 §3.1.1).
        let bit = self.byte & 1;
        self.byte >>= 1;
        self.bits -= 1;
        Some(bit)
    }

    fn read_bits(&mut self, count: u32) -> Option<u32> {
        let mut value = 0;
        for index in 0..count {
            value |= self.read_bit()? << index;
        }
        Some(value)
    }

    fn read_byte(&mut self) -> Option<u8> {
        if self.position >= self.data.len() {
            return None;
        }
        let byte = self.data[self.position];
        self.position += 1;
        Some(byte)
    }

    fn align(&mut self) {
        self.bits = 0;
    }
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::*;
    use crate::catalog;

    // ------------------------------------------------------------- fixtures

    /// A minimal MSB-first bit writer for the fixture entropy streams (the
    /// reference's `BitReader` is MSB-first within a byte).
    struct BitWriter {
        bytes: Vec<u8>,
        current: u8,
        count: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                count: 0,
            }
        }

        fn bits(&mut self, value: u32, length: u32) {
            for shift in (0..length).rev() {
                let bit = ((value >> shift) & 1) as u8;
                self.current = (self.current << 1) | bit;
                self.count += 1;
                if self.count == 8 {
                    self.bytes.push(self.current);
                    self.current = 0;
                    self.count = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.count > 0 {
                self.bytes.push(self.current << (8 - self.count));
            }
            self.bytes
        }
    }

    /// The entropy of an all-zero frame: every block's DC difference is 0 and
    /// every block ends immediately with EOB, so each block is exactly the two
    /// shortest codes of its tables (`AlphaMovie.cpp:242-262`). The standard
    /// tables give `Cb/Cr` DC `00`, chroma AC EOB `00`, and luma DC `00` with
    /// luma AC EOB `1010` (`:69-109`). `with_alpha` selects the four-component
    /// scan (`Cb, Cr, Y*4, A*4`, `:589-600`) or the colour-only scan
    /// (`Cb, Cr, Y*4`, `:664-671`).
    fn zero_entropy(mcus: usize, with_alpha: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        for _ in 0..mcus {
            writer.bits(0b00, 2); // Cb DC, category 0
            writer.bits(0b00, 2); // Cb AC, EOB
            writer.bits(0b00, 2); // Cr DC
            writer.bits(0b00, 2); // Cr AC, EOB
            for _ in 0..4 {
                writer.bits(0b00, 2); // Y DC (shares the luma predictor with A)
                writer.bits(0b1010, 4); // Y AC, EOB
            }
            if with_alpha {
                for _ in 0..4 {
                    writer.bits(0b00, 2); // A DC, same luma tables/predictor
                    writer.bits(0b1010, 4); // A AC, EOB
                }
            }
        }
        writer.finish()
    }

    /// An `'AJPM'` header (`AlphaMovie.cpp:382-431`) with identity
    /// quantisation tables.
    fn header(width: u16, height: u16, frames: u32, zlib_alpha: bool) -> Vec<u8> {
        let quant_size: u32 = if zlib_alpha { 0x80 } else { 0xc0 };
        let header_size = HEADER_LEN as u32 + quant_size;
        let mut bytes = vec![0u8; header_size as usize];
        bytes[0..4].copy_from_slice(&AMV_MAGIC.to_le_bytes());
        bytes[0x0c..0x10].copy_from_slice(&header_size.to_le_bytes());
        bytes[0x14..0x18].copy_from_slice(&frames.to_le_bytes());
        bytes[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes()); // FPS 1/30
        bytes[0x1c..0x20].copy_from_slice(&30u32.to_le_bytes());
        bytes[0x20..0x22].copy_from_slice(&width.to_le_bytes());
        bytes[0x22..0x24].copy_from_slice(&height.to_le_bytes());
        bytes[0x24..0x28].copy_from_slice(&(if zlib_alpha { 2u32 } else { 1u32 }).to_le_bytes());
        for byte in &mut bytes[HEADER_LEN..] {
            *byte = 1;
        }
        bytes
    }

    /// A JPEG-alpha frame (`AlphaMovie.cpp:560-625`), `size = 12 + entropy`.
    fn jpeg_alpha_frame(number: u32, left: i16, top: i16, width: u16, height: u16) -> Vec<u8> {
        let mcus = (width as usize / 16) * (height as usize / 16);
        let entropy = zero_entropy(mcus, true);
        let mut frame = Vec::new();
        frame.extend_from_slice(&FRAM_MAGIC.to_le_bytes());
        frame.extend_from_slice(&(12 + entropy.len() as u32).to_le_bytes());
        frame.extend_from_slice(&number.to_le_bytes());
        frame.extend_from_slice(&left.to_le_bytes());
        frame.extend_from_slice(&top.to_le_bytes());
        frame.extend_from_slice(&width.to_le_bytes());
        frame.extend_from_slice(&height.to_le_bytes());
        frame.extend_from_slice(&entropy);
        frame
    }

    /// A zlib stream carrying `data` as stored (uncompressed) blocks.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        if data.is_empty() {
            out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
        } else {
            let mut chunks = data.chunks(0xffff).peekable();
            while let Some(chunk) = chunks.next() {
                out.push(u8::from(chunks.peek().is_none()));
                out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
                out.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
                out.extend_from_slice(chunk);
            }
        }
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }

    /// A zlib-alpha frame (`AlphaMovie.cpp:627-704`): `size = 16 + alpha +
    /// colour`.
    fn zlib_alpha_frame(
        number: u32,
        left: i16,
        top: i16,
        width: u16,
        height: u16,
        alpha: &[u8],
    ) -> Vec<u8> {
        let mcus = (width as usize / 16) * (height as usize / 16);
        let color = zero_entropy(mcus, false);
        let alpha_stream = zlib_stored(alpha);
        let mut frame = Vec::new();
        frame.extend_from_slice(&FRAM_MAGIC.to_le_bytes());
        frame.extend_from_slice(
            &(16 + alpha_stream.len() as u32 + color.len() as u32).to_le_bytes(),
        );
        frame.extend_from_slice(&number.to_le_bytes());
        frame.extend_from_slice(&left.to_le_bytes());
        frame.extend_from_slice(&top.to_le_bytes());
        frame.extend_from_slice(&width.to_le_bytes());
        frame.extend_from_slice(&height.to_le_bytes());
        frame.extend_from_slice(&(alpha_stream.len() as u32).to_le_bytes());
        frame.extend_from_slice(&alpha_stream);
        frame.extend_from_slice(&color);
        frame
    }

    fn movie(frames: &[Vec<u8>], width: u16, height: u16, zlib_alpha: bool) -> Vec<u8> {
        let mut bytes = header(width, height, frames.len() as u32, zlib_alpha);
        for frame in frames {
            bytes.extend_from_slice(frame);
        }
        bytes
    }

    fn engine_with(movies: &[(&str, Vec<u8>)]) -> KrkrEngine {
        let storage = krkr_assets::ProjectStorage::from_memory(
            movies.iter().map(|(path, bytes)| (*path, bytes.clone())),
        );
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(AlphaMoviePlugin).expect("plugin");
        engine
    }

    /// A 48x48 opaque-white layer plus a movie object.
    const SETUP: &str = r#"
        global.layer = new Layer();
        layer.setImageSize(48, 48);
        layer.fillRect(0, 0, 48, 48, 0xffffffff);
        global.movie = new AlphaMovie();
        global.returned = -1;
    "#;

    fn integer(engine: &mut KrkrEngine, expression: &str) -> i64 {
        engine
            .execute_expression("probe.tjs", expression)
            .expect("expression")
            .to_integer()
            .expect("integer")
    }

    fn text(engine: &mut KrkrEngine, expression: &str) -> String {
        engine
            .execute_expression("probe.tjs", expression)
            .expect("expression")
            .to_tjs_string()
            .expect("string")
    }

    // ---------------------------------------------------------------- tests

    #[test]
    fn registers_the_class_surface_and_the_aliases() {
        let engine = engine_with(&[]);
        assert_eq!(catalog::canonical_name(NAME), Some(NAME));
        assert_eq!(catalog::canonical_name("alphamovie.dll"), Some(NAME));
        assert_eq!(catalog::canonical_name("nene.dll"), Some(NAME));
        assert_eq!(catalog::canonical_name("Nene.dll"), Some(NAME));
        assert!(catalog::is_same_plugin("nene.dll", "AlphaMovie.dll"));

        let runtime = engine.tjs_runtime();
        let class = runtime
            .global_member(CLASS_NAME)
            .object_handle()
            .expect("AlphaMovie class");
        for name in [
            "open",
            "clear",
            "isPlaying",
            "play",
            "stop",
            "setPosition",
            "setNextMovieFile",
            "showNextImage",
            "numOfFrame",
            "frame",
            "loop",
            "nextLoop",
            "preloadSamples",
            "left",
            "top",
            "screenWidth",
            "screenHeight",
            "FPSScale",
            "FPSRate",
        ] {
            assert!(
                !matches!(runtime.object_member(class, name), Variant::Void),
                "member `{name}` is not registered",
            );
        }
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("AlphaMovie class registered")),
            "no registration line",
        );
    }

    #[test]
    fn a_new_movie_has_the_constructor_defaults() {
        let mut engine = engine_with(&[]);
        engine
            .execute_script("new.tjs", "global.m = new AlphaMovie();")
            .expect("script");
        assert_eq!(integer(&mut engine, "m.numOfFrame"), 0);
        assert_eq!(integer(&mut engine, "m.frame"), 0);
        assert_eq!(integer(&mut engine, "m.loop"), 0);
        assert_eq!(integer(&mut engine, "m.nextLoop"), 0);
        assert_eq!(integer(&mut engine, "m.preloadSamples"), 5);
        assert_eq!(integer(&mut engine, "m.left"), 0);
        assert_eq!(integer(&mut engine, "m.top"), 0);
        assert_eq!(integer(&mut engine, "m.screenWidth"), 0);
        assert_eq!(integer(&mut engine, "m.screenHeight"), 0);
        assert_eq!(integer(&mut engine, "m.FPSScale"), 0);
        assert_eq!(integer(&mut engine, "m.FPSRate"), 0);
        assert_eq!(integer(&mut engine, "m.isPlaying()"), 0);
        // `clear()` is a no-op in the reference (`AlphaMovie.cpp:500`).
        engine
            .execute_script("clear.tjs", "m.clear();")
            .expect("clear");
    }

    #[test]
    fn open_reads_the_header_and_the_frame_table() {
        let fixture = movie(
            &[
                jpeg_alpha_frame(4, 0, 0, 16, 16),
                jpeg_alpha_frame(9, 0, 0, 16, 16),
            ],
            16,
            16,
            false,
        );
        let mut engine = engine_with(&[("movie.amv", fixture)]);
        engine
            .execute_script(
                "open.tjs",
                "global.m = new AlphaMovie(); m.open(\"movie.amv\");",
            )
            .expect("open");
        assert_eq!(integer(&mut engine, "m.numOfFrame"), 2);
        assert_eq!(integer(&mut engine, "m.screenWidth"), 16);
        assert_eq!(integer(&mut engine, "m.screenHeight"), 16);
        assert_eq!(integer(&mut engine, "m.FPSScale"), 1);
        assert_eq!(integer(&mut engine, "m.FPSRate"), 30);
        assert_eq!(integer(&mut engine, "m.frame"), 0);
    }

    /// The JPEG-alpha path end to end: the IDCT of an all-zero coefficient
    /// block is the reference's level shift (`AlphaMovie.cpp:226`), so every
    /// sample lands on 128 and the composed pixel is (128,128,128,128).
    #[test]
    fn show_next_image_decodes_the_jpeg_alpha_path_into_the_layer() {
        let fixture = movie(&[jpeg_alpha_frame(7, 0, 0, 16, 16)], 16, 16, false);
        let mut engine = engine_with(&[("movie.amv", fixture)]);
        engine
            .execute_script(
                "show.tjs",
                &format!("{SETUP} movie.open(\"movie.amv\"); global.returned = movie.showNextImage(layer);"),
            )
            .expect("show");
        assert_eq!(
            integer(&mut engine, "returned"),
            7,
            "the FRAM number is returned"
        );
        assert_eq!(integer(&mut engine, "layer.getMainPixel(0, 0)"), 0x80_8080);
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(15, 15)"),
            0x80_8080
        );
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(0, 0)"), 128);
        // Outside the frame rect the layer keeps its fill.
        assert_eq!(
            integer(&mut engine, "layer.getMainPixel(16, 16)"),
            0xff_ffff
        );
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(16, 16)"), 255);
        assert_eq!(integer(&mut engine, "movie.frame"), 1, "the index advanced");
    }

    /// The zlib-alpha path: the colour is the same all-zero scan, the alpha
    /// comes from the module's inflate.
    #[test]
    fn show_next_image_decodes_the_zlib_alpha_path_into_the_layer() {
        let mut alpha = vec![0u8; 16 * 16];
        alpha[0] = 255;
        alpha[15] = 128;
        alpha[16] = 64;
        let fixture = movie(&[zlib_alpha_frame(3, 0, 0, 16, 16, &alpha)], 16, 16, true);
        let mut engine = engine_with(&[("movie.amv", fixture)]);
        engine
            .execute_script(
                "show.tjs",
                &format!("{SETUP} movie.open(\"movie.amv\"); global.returned = movie.showNextImage(layer);"),
            )
            .expect("show");
        assert_eq!(integer(&mut engine, "returned"), 3);
        assert_eq!(integer(&mut engine, "layer.getMainPixel(0, 0)"), 0x80_8080);
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(0, 0)"), 255);
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(15, 0)"), 128);
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(0, 1)"), 64);
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(1, 0)"), 0);
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(16, 16)"), 255);
    }

    #[test]
    fn show_next_image_advances_wraps_and_holds() {
        let fixture = movie(
            &[
                jpeg_alpha_frame(5, 0, 0, 16, 16),
                jpeg_alpha_frame(9, 0, 0, 16, 16),
            ],
            16,
            16,
            false,
        );
        let mut engine = engine_with(&[("movie.amv", fixture)]);
        engine
            .execute_script(
                "hold.tjs",
                &format!(
                    "{SETUP} movie.open(\"movie.amv\"); global.seq = \"\";
                     for (var i = 0; i < 4; i++) seq += movie.showNextImage(layer) + \" \";"
                ),
            )
            .expect("hold run");
        assert_eq!(
            text(&mut engine, "seq"),
            "5 9 9 9 ",
            "no loop holds the last frame"
        );
        engine
            .execute_script(
                "wrap.tjs",
                "movie.loop = true; movie.frame = 0; global.seq = \"\";
                 for (var i = 0; i < 4; i++) seq += movie.showNextImage(layer) + \" \";",
            )
            .expect("wrap run");
        assert_eq!(
            text(&mut engine, "seq"),
            "5 9 5 9 ",
            "loop rewinds to the first frame"
        );
    }

    #[test]
    fn set_next_movie_file_switches_at_the_end_and_adopts_next_loop() {
        let first = movie(
            &[
                jpeg_alpha_frame(1, 0, 0, 16, 16),
                jpeg_alpha_frame(2, 0, 0, 16, 16),
            ],
            16,
            16,
            false,
        );
        let second = movie(
            &[
                jpeg_alpha_frame(7, 0, 0, 16, 16),
                jpeg_alpha_frame(8, 0, 0, 16, 16),
            ],
            16,
            16,
            false,
        );
        let mut engine = engine_with(&[("a.amv", first), ("b.amv", second)]);
        engine
            .execute_script(
                "next.tjs",
                &format!(
                    "{SETUP} movie.open(\"a.amv\"); movie.setNextMovieFile(\"b.amv\");
                     movie.nextLoop = true; global.seq = \"\";
                     for (var i = 0; i < 5; i++) seq += movie.showNextImage(layer) + \" \";"
                ),
            )
            .expect("next run");
        assert_eq!(text(&mut engine, "seq"), "1 2 7 8 7 ");
        assert_eq!(
            integer(&mut engine, "movie.numOfFrame"),
            2,
            "the second movie is open"
        );
        assert_eq!(
            integer(&mut engine, "movie.loop"),
            1,
            "nextLoop became loop"
        );
    }

    /// `setPosition` shifts the frame rect and the blit clips it
    /// (`AlphaMovie.cpp:717-731`); a checkerboard alpha makes the shift
    /// visible.
    #[test]
    fn set_position_offsets_and_clips_the_blit() {
        let mut alpha = vec![0u8; 16 * 16];
        for row in 0..16 {
            for column in 0..16 {
                if (row + column) % 2 == 0 {
                    alpha[row * 16 + column] = 255;
                }
            }
        }
        let fixture = movie(&[zlib_alpha_frame(1, 0, 0, 16, 16, &alpha)], 16, 16, true);
        let mut engine = engine_with(&[("movie.amv", fixture)]);
        engine
            .execute_script(
                "clip.tjs",
                &format!(
                    "{SETUP} movie.open(\"movie.amv\"); movie.setPosition(-8, -8);
                     global.returned = movie.showNextImage(layer);"
                ),
            )
            .expect("clip run");
        assert_eq!(integer(&mut engine, "movie.left"), -8);
        assert_eq!(integer(&mut engine, "movie.top"), -8);
        // Frame pixel (8,8) lands on layer (0,0): 8+8 even → opaque.
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(0, 0)"), 255);
        // Frame pixel (9,8) lands on layer (1,0): odd → transparent.
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(1, 0)"), 0);
        assert_eq!(integer(&mut engine, "layer.getMainPixel(1, 0)"), 0x80_8080);
        // The 8x8 rect covers (0..8, 0..8) only; beyond it the fill survives.
        assert_eq!(integer(&mut engine, "layer.getMainPixel(8, 0)"), 0xff_ffff);
        assert_eq!(integer(&mut engine, "layer.getMainPixel(0, 8)"), 0xff_ffff);
    }

    #[test]
    fn the_frame_property_seeks_and_clamps() {
        let fixture = movie(
            &[
                jpeg_alpha_frame(5, 0, 0, 16, 16),
                jpeg_alpha_frame(9, 0, 0, 16, 16),
            ],
            16,
            16,
            false,
        );
        let mut engine = engine_with(&[("movie.amv", fixture)]);
        engine
            .execute_script(
                "seek.tjs",
                &format!(
                    "{SETUP} movie.open(\"movie.amv\"); movie.frame = 1;
                     global.second = movie.showNextImage(layer);
                     movie.frame = 99; global.clamped_high = movie.frame;
                     movie.frame = -3; global.clamped_low = movie.frame;"
                ),
            )
            .expect("seek run");
        assert_eq!(
            integer(&mut engine, "second"),
            9,
            "frame = 1 decodes the second frame"
        );
        assert_eq!(integer(&mut engine, "clamped_high"), 1);
        assert_eq!(integer(&mut engine, "clamped_low"), 0);
    }

    #[test]
    fn play_stop_toggle_the_playing_flag() {
        let fixture = movie(&[jpeg_alpha_frame(1, 0, 0, 16, 16)], 16, 16, false);
        let mut engine = engine_with(&[("movie.amv", fixture)]);
        engine
            .execute_script(
                "play.tjs",
                &format!(
                    "{SETUP} movie.open(\"movie.amv\"); movie.frame = 0;
                     global.before = movie.isPlaying(); movie.play();
                     global.during = movie.isPlaying(); global.after_play_frame = movie.frame;
                     movie.stop(); global.after = movie.isPlaying();"
                ),
            )
            .expect("play run");
        assert_eq!(integer(&mut engine, "before"), 0);
        assert_eq!(integer(&mut engine, "during"), 1);
        assert_eq!(integer(&mut engine, "after_play_frame"), 0, "play rewinds");
        assert_eq!(integer(&mut engine, "after"), 0);
    }

    #[test]
    fn the_readonly_properties_reject_writes_and_the_others_stick() {
        let mut engine = engine_with(&[]);
        engine
            .execute_script(
                "props.tjs",
                r#"
                global.m = new AlphaMovie();
                global.denied = "";
                try { m.numOfFrame = 3; denied += "numOfFrame;"; } catch (e) { }
                try { m.screenWidth = 3; denied += "screenWidth;"; } catch (e) { }
                try { m.screenHeight = 3; denied += "screenHeight;"; } catch (e) { }
                try { m.FPSScale = 3; denied += "FPSScale;"; } catch (e) { }
                try { m.FPSRate = 3; denied += "FPSRate;"; } catch (e) { }
                m.loop = 1; m.nextLoop = 1; m.preloadSamples = 9;
                m.left = 4; m.top = 6; m.setPosition(7, 8);
                global.loop = m.loop; global.nextLoop = m.nextLoop;
                global.preload = m.preloadSamples; global.left = m.left; global.top = m.top;
                "#,
            )
            .expect("props script");
        assert_eq!(
            text(&mut engine, "denied"),
            "",
            "a read-only write must throw"
        );
        assert_eq!(integer(&mut engine, "loop"), 1);
        assert_eq!(integer(&mut engine, "nextLoop"), 1);
        assert_eq!(integer(&mut engine, "preload"), 9);
        assert_eq!(
            integer(&mut engine, "left"),
            7,
            "setPosition writes left/top"
        );
        assert_eq!(integer(&mut engine, "top"), 8);
    }

    #[test]
    fn open_and_show_next_image_report_the_references_errors() {
        let good = movie(&[jpeg_alpha_frame(1, 0, 0, 16, 16)], 16, 16, false);

        // Header-level failures, each a one-field edit of the good fixture.
        type Patch = (&'static str, Box<dyn Fn(&mut Vec<u8>)>);
        let patches: Vec<Patch> = vec![
            (
                "This file is not Alpha Movie File.",
                Box::new(|bytes| bytes[0] = 0),
            ),
            (
                "Invalid File revision number.",
                Box::new(|bytes| bytes[0x08] = 1),
            ),
            (
                "Invalid header size.",
                Box::new(|bytes| bytes[0x0c..0x10].copy_from_slice(&0x20u32.to_le_bytes())),
            ),
            (
                "Invalid Quantaization table size.",
                Box::new(|bytes| {
                    bytes[0x0c..0x10].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes())
                }),
            ),
            (
                "Not found frame in this file.",
                Box::new(|bytes| bytes[0x14..0x18].copy_from_slice(&0u32.to_le_bytes())),
            ),
            (
                "Invalid frame rate.",
                Box::new(|bytes| bytes[0x18..0x1c].copy_from_slice(&0u32.to_le_bytes())),
            ),
            (
                "Screen size is zero ?",
                Box::new(|bytes| bytes[0x20..0x22].copy_from_slice(&0u16.to_le_bytes())),
            ),
            (
                "Invalid Attribute.",
                Box::new(|bytes| bytes[0x24..0x28].copy_from_slice(&0u32.to_le_bytes())),
            ),
            (
                "File format error.",
                Box::new(|bytes| {
                    let offset = HEADER_LEN + 0xc0;
                    bytes[offset] = 0;
                }),
            ),
        ];
        for (message, patch) in patches {
            let mut bytes = good.clone();
            patch(&mut bytes);
            let mut engine = engine_with(&[("movie.amv", bytes)]);
            let error = engine
                .execute_script(
                    "bad.tjs",
                    "global.m = new AlphaMovie(); m.open(\"movie.amv\");",
                )
                .expect_err(message);
            assert_eq!(error.message, message, "fixture for `{message}`");
        }

        // Storage and call-level failures.
        let mut engine = engine_with(&[("movie.amv", good)]);
        let error = engine
            .execute_script(
                "missing.tjs",
                "var m = new AlphaMovie(); m.open(\"missing.amv\");",
            )
            .expect_err("missing storage");
        assert_eq!(error.message, "AlphaMovie: cannot open storage.");

        engine.execute_script("setup.tjs", SETUP).expect("setup");
        let error = engine
            .execute_script("empty.tjs", "movie.showNextImage(layer);")
            .expect_err("no movie");
        assert_eq!(error.message, "AlphaMovie: no movie opened.");
        let error = engine
            .execute_script("noarg.tjs", "movie.showNextImage(0);")
            .expect_err("non-object argument");
        assert_eq!(error.message, "AlphaMovie: showNextImage requires a Layer.");
        let error = engine
            .execute_script("nodraw.tjs", "movie.showNextImage(%[]);")
            .expect_err("non-layer argument");
        assert_eq!(
            error.message,
            "AlphaMovie: target must be a Layer with image."
        );

        let error = engine
            .execute_script(
                "freed.tjs",
                "movie.open(\"movie.amv\"); layer.freeImage(); movie.showNextImage(layer);",
            )
            .expect_err("a freed image is not drawable");
        assert_eq!(
            error.message,
            "AlphaMovie: target must be a Layer with image."
        );

        let error = engine
            .execute_script("short.tjs", "movie.showNextImage();")
            .expect_err("no arguments");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
    }

    /// The module's own inflate against three block kinds: stored (built by
    /// the fixture writer), fixed and dynamic (fixtures produced by python3's
    /// `zlib.compress` — `Z_FIXED` strategy and default level 9 — embedded
    /// with their plaintext properties).
    #[test]
    fn inflate_decodes_stored_fixed_and_dynamic_blocks() {
        let plane: Vec<u8> = (0..256u32).map(|index| (index * 7 % 256) as u8).collect();
        let stream = zlib_stored(&plane);
        assert_eq!(inflate_zlib(&stream, plane.len()), Some(plane.clone()));
        assert_eq!(
            inflate_zlib(&stream, plane.len() - 1),
            None,
            "the limit holds"
        );

        const FIXED: [u8; 18] = [
            0x78, 0x01, 0x63, 0x60, 0xa0, 0x3d, 0xf8, 0x4f, 0x07, 0xc0, 0x4e, 0x26, 0x00, 0x00,
            0x9f, 0x71, 0x65, 0x25,
        ];
        let mut expected = vec![0u8; 100];
        expected.extend(vec![255u8; 100]);
        expected.extend(vec![7u8; 56]);
        assert_eq!(inflate_zlib(&FIXED, 256), Some(expected));

        const DYNAMIC: [u8; 70] = [
            0x78, 0xda, 0x3d, 0x8c, 0xb1, 0x11, 0xc0, 0x30, 0x0c, 0x02, 0xe1, 0x47, 0xc8, 0x42,
            0x1e, 0x20, 0xe3, 0x65, 0x97, 0x4c, 0x96, 0x2a, 0x95, 0x25, 0xec, 0x33, 0xc5, 0xeb,
            0x04, 0x42, 0xb6, 0x4a, 0x0d, 0x32, 0xb8, 0x65, 0xe2, 0xf1, 0x35, 0x90, 0xed, 0x4e,
            0x8a, 0xe4, 0xee, 0xcf, 0xa6, 0x47, 0x5b, 0x9d, 0x56, 0x05, 0xd6, 0x15, 0x79, 0xf4,
            0xae, 0xc4, 0x07, 0xbb, 0x5c, 0xba, 0xa4, 0x71, 0x5c, 0x4d, 0x27, 0x21, 0x04, 0x0e,
        ];
        let decoded = inflate_zlib(&DYNAMIC, 128).expect("a dynamic block");
        assert_eq!(decoded.len(), 128);
        assert_eq!(adler32(&decoded), 0x2721_040e, "the fixture's own checksum");
        assert_eq!(&decoded[..8], &[1, 1, 0, 0, 0, 0, 1, 0]);

        let mut corrupt = FIXED;
        corrupt[10] ^= 1;
        assert_eq!(inflate_zlib(&corrupt, 256), None, "Adler-32 catches damage");
    }

    /// The zlib-alpha fallback the reference takes when the stream cannot be
    /// inflated (`AlphaMovie.cpp:676-681`): a fully opaque plane.
    #[test]
    fn a_broken_alpha_stream_falls_back_to_opaque() {
        let mut frame = zlib_alpha_frame(1, 0, 0, 16, 16, &[0u8; 16 * 16]);
        // Corrupt the stored block's first payload byte (24 bytes of frame
        // header + 5 bytes of zlib wrapper before it).
        frame[24 + 5] = 0x55;
        let fixture = movie(&[frame], 16, 16, true);
        let mut engine = engine_with(&[("movie.amv", fixture)]);
        engine
            .execute_script(
                "fallback.tjs",
                &format!("{SETUP} movie.open(\"movie.amv\"); movie.showNextImage(layer);"),
            )
            .expect("fallback run");
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(0, 0)"), 255);
        assert_eq!(integer(&mut engine, "layer.getMaskPixel(15, 15)"), 255);
    }
}
