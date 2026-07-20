//! No-op stub of wamsoft `layerExDraw.dll` (layerExDraw/main.cpp).
//!
//! Registers the `GdiPlus` namespace (PointF/RectF/Matrix/Image/Font/
//! Appearance/Path classes, enum constants, font statics) and attaches the
//! layerExDraw member surface onto the engine's global `Layer` class. No
//! GDI+ drawing happens: `Layer.draw*` methods only return the `RectF`
//! update region a caller would expect from the argument list. The geometry
//! classes (PointF, RectF, Matrix) are functional pure math so scripts can
//! keep computing with them; Image/Font/Appearance/Path are data-less stubs.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeFunction, ObjectHandle, Runtime, Variant},
};

pub struct LayerExDrawPlugin;

impl KrkrPlugin for LayerExDrawPlugin {
    fn name(&self) -> &str {
        "layerExDraw.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_gdi_plus(runtime);
        install_layer_ex_draw(runtime);
        Ok(())
    }
}

// ---------------------------------------------------------------- GdiPlus

fn install_gdi_plus(runtime: &mut Runtime<KrkrHost>) {
    let gdi_plus = match runtime.global_member("GdiPlus") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "GdiPlus");
            runtime.set_global_member("GdiPlus", Variant::Object(handle));
            handle
        }
    };

    for &(name, value) in GDIPLUS_ENUM_CONSTANTS {
        runtime.set_object_member(gdi_plus, name, Variant::Integer(value));
    }

    runtime.register_object_native(gdi_plus, "addPrivateFont", add_private_font);
    runtime.register_object_native(gdi_plus, "getFontList", get_font_list);

    let point_f = point_f_constructor(runtime);
    let rect_f = rect_f_constructor(runtime);
    let matrix = matrix_constructor(runtime);
    let image = image_constructor(runtime);
    let font = font_constructor(runtime);
    let appearance = appearance_constructor(runtime);
    let path = path_constructor(runtime);
    runtime.set_object_member(gdi_plus, "PointF", Variant::Object(point_f));
    runtime.set_object_member(gdi_plus, "RectF", Variant::Object(rect_f));
    runtime.set_object_member(gdi_plus, "Matrix", Variant::Object(matrix));
    runtime.set_object_member(gdi_plus, "Image", Variant::Object(image));
    runtime.set_object_member(gdi_plus, "Font", Variant::Object(font));
    runtime.set_object_member(gdi_plus, "Appearance", Variant::Object(appearance));
    runtime.set_object_member(gdi_plus, "Path", Variant::Object(path));
}

/// Enum members of the `GdiPlus` class, transcribed in order from the
/// `ENUM(...)` block of the reference main.cpp (GDI+ numeric values).
const GDIPLUS_ENUM_CONSTANTS: &[(&str, i64)] = &[
    // Status
    ("Ok", 0),
    ("GenericError", 1),
    ("InvalidParameter", 2),
    ("OutOfMemory", 3),
    ("ObjectBusy", 4),
    ("InsufficientBuffer", 5),
    ("NotImplemented", 6),
    ("Win32Error", 7),
    ("WrongState", 8),
    ("Aborted", 9),
    ("FileNotFound", 10),
    ("ValueOverflow", 11),
    ("AccessDenied", 12),
    ("UnknownImageFormat", 13),
    ("FontFamilyNotFound", 14),
    ("FontStyleNotFound", 15),
    ("NotTrueTypeFont", 16),
    ("UnsupportedGdiplusVersion", 17),
    ("GdiplusNotInitialized", 18),
    ("PropertyNotFound", 19),
    ("PropertyNotSupported", 20),
    // FontStyle
    ("FontStyleRegular", 0),
    ("FontStyleBold", 1),
    ("FontStyleItalic", 2),
    ("FontStyleBoldItalic", 3),
    ("FontStyleUnderline", 4),
    ("FontStyleStrikeout", 8),
    // BrushType
    ("BrushTypeSolidColor", 0),
    ("BrushTypeHatchFill", 1),
    ("BrushTypeTextureFill", 2),
    ("BrushTypePathGradient", 3),
    ("BrushTypeLinearGradient", 4),
    // DashCap
    ("DashCapFlat", 0),
    ("DashCapRound", 2),
    ("DashCapTriangle", 3),
    // DashStyle
    ("DashStyleSolid", 0),
    ("DashStyleDash", 1),
    ("DashStyleDot", 2),
    ("DashStyleDashDot", 3),
    ("DashStyleDashDotDot", 4),
    // HatchStyle
    ("HatchStyleHorizontal", 0),
    ("HatchStyleVertical", 1),
    ("HatchStyleForwardDiagonal", 2),
    ("HatchStyleBackwardDiagonal", 3),
    ("HatchStyleCross", 4),
    ("HatchStyleDiagonalCross", 5),
    ("HatchStyle05Percent", 6),
    ("HatchStyle10Percent", 7),
    ("HatchStyle20Percent", 8),
    ("HatchStyle25Percent", 9),
    ("HatchStyle30Percent", 10),
    ("HatchStyle40Percent", 11),
    ("HatchStyle50Percent", 12),
    ("HatchStyle60Percent", 13),
    ("HatchStyle70Percent", 14),
    ("HatchStyle75Percent", 15),
    ("HatchStyle80Percent", 16),
    ("HatchStyle90Percent", 17),
    ("HatchStyleLightDownwardDiagonal", 18),
    ("HatchStyleLightUpwardDiagonal", 19),
    ("HatchStyleDarkDownwardDiagonal", 20),
    ("HatchStyleDarkUpwardDiagonal", 21),
    ("HatchStyleWideDownwardDiagonal", 22),
    ("HatchStyleWideUpwardDiagonal", 23),
    ("HatchStyleLightVertical", 24),
    ("HatchStyleLightHorizontal", 25),
    ("HatchStyleNarrowVertical", 26),
    ("HatchStyleNarrowHorizontal", 27),
    ("HatchStyleDarkVertical", 28),
    ("HatchStyleDarkHorizontal", 29),
    ("HatchStyleDashedDownwardDiagonal", 30),
    ("HatchStyleDashedUpwardDiagonal", 31),
    ("HatchStyleDashedHorizontal", 32),
    ("HatchStyleDashedVertical", 33),
    ("HatchStyleSmallConfetti", 34),
    ("HatchStyleLargeConfetti", 35),
    ("HatchStyleZigZag", 36),
    ("HatchStyleWave", 37),
    ("HatchStyleDiagonalBrick", 38),
    ("HatchStyleHorizontalBrick", 39),
    ("HatchStyleWeave", 40),
    ("HatchStylePlaid", 41),
    ("HatchStyleDivot", 42),
    ("HatchStyleDottedGrid", 43),
    ("HatchStyleDottedDiamond", 44),
    ("HatchStyleShingle", 45),
    ("HatchStyleTrellis", 46),
    ("HatchStyleSphere", 47),
    ("HatchStyleSmallGrid", 48),
    ("HatchStyleSmallCheckerBoard", 49),
    ("HatchStyleLargeCheckerBoard", 50),
    ("HatchStyleOutlinedDiamond", 51),
    ("HatchStyleSolidDiamond", 52),
    ("HatchStyleTotal", 53),
    ("HatchStyleLargeGrid", 4), // HatchStyleCross
    ("HatchStyleMin", 0),       // HatchStyleHorizontal
    ("HatchStyleMax", 52),      // HatchStyleSolidDiamond
    // LinearGradientMode
    ("LinearGradientModeHorizontal", 0),
    ("LinearGradientModeVertical", 1),
    ("LinearGradientModeForwardDiagonal", 2),
    ("LinearGradientModeBackwardDiagonal", 3),
    // LineCap
    ("LineCapFlat", 0),
    ("LineCapSquare", 1),
    ("LineCapRound", 2),
    ("LineCapTriangle", 3),
    ("LineCapNoAnchor", 16),
    ("LineCapSquareAnchor", 17),
    ("LineCapRoundAnchor", 18),
    ("LineCapDiamondAnchor", 19),
    ("LineCapArrowAnchor", 20),
    // LineJoin
    ("LineJoinMiter", 0),
    ("LineJoinBevel", 1),
    ("LineJoinRound", 2),
    ("LineJoinMiterClipped", 3),
    // PenAlignment
    ("PenAlignmentCenter", 0),
    ("PenAlignmentInset", 1),
    // WrapMode
    ("WrapModeTile", 0),
    ("WrapModeTileFlipX", 1),
    ("WrapModeTileFlipY", 2),
    ("WrapModeTileFlipXY", 3),
    ("WrapModeClamp", 4),
    // MatrixOrder
    ("MatrixOrderPrepend", 0),
    ("MatrixOrderAppend", 1),
    // ImageType
    ("ImageTypeUnknown", 0),
    ("ImageTypeBitmap", 1),
    ("ImageTypeMetafile", 2),
    // RotateFlipType
    ("RotateNoneFlipNone", 0),
    ("Rotate90FlipNone", 1),
    ("Rotate180FlipNone", 2),
    ("Rotate270FlipNone", 3),
    ("RotateNoneFlipX", 4),
    ("Rotate90FlipX", 5),
    ("Rotate180FlipX", 6),
    ("Rotate270FlipX", 7),
    ("RotateNoneFlipY", 6),
    ("Rotate90FlipY", 7),
    ("Rotate180FlipY", 4),
    ("Rotate270FlipY", 5),
    ("RotateNoneFlipXY", 2),
    ("Rotate90FlipXY", 3),
    ("Rotate180FlipXY", 0),
    ("Rotate270FlipXY", 1),
    // SmoothingMode
    ("SmoothingModeInvalid", -1),
    ("SmoothingModeDefault", 0),
    ("SmoothingModeHighSpeed", 1),
    ("SmoothingModeHighQuality", 2),
    ("SmoothingModeNone", 3),
    ("SmoothingModeAntiAlias", 4),
    // TextRenderingHint
    ("TextRenderingHintSystemDefault", 0),
    ("TextRenderingHintSingleBitPerPixelGridFit", 1),
    ("TextRenderingHintSingleBitPerPixel", 2),
    ("TextRenderingHintAntiAliasGridFit", 3),
    ("TextRenderingHintAntiAlias", 4),
    ("TextRenderingHintClearTypeGridFit", 5),
];

/// GdiPlus.addPrivateFont: the reference loads the font into a private
/// collection; this stub only logs.
fn add_private_font(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = arg_string(&args, 0);
    runtime.host_mut().log(&format!(
        "layerExDraw.dll: GdiPlus.addPrivateFont({name}) is a no-op"
    ));
    Ok(Variant::Void)
}

/// GdiPlus.getFontList: no font enumeration support; always an empty Array.
fn get_font_list(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

// ---------------------------------------------------------------- PointF

fn point_f_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "PointF");
            install_point_f_members(runtime, instance);
            runtime.set_object_member(instance, "x", Variant::Real(arg_real(&args, 0)));
            runtime.set_object_member(instance, "y", Variant::Real(arg_real(&args, 1)));
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "PointF");
    install_point_f_members(runtime, handle);
    handle
}

fn install_point_f_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for name in ["x", "y"] {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, Variant::Real(0.0));
        }
    }
    runtime.register_object_native(handle, "Equals", point_f_equals);
}

fn new_point_f(runtime: &mut Runtime<KrkrHost>, x: f64, y: f64) -> ObjectHandle {
    let handle = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(handle, "PointF");
    install_point_f_members(runtime, handle);
    runtime.set_object_member(handle, "x", Variant::Real(x));
    runtime.set_object_member(handle, "y", Variant::Real(y));
    handle
}

fn point_f_equals(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let equals = this_real(runtime, this_obj, "x") == variant_real(runtime, args.first(), "x")
        && this_real(runtime, this_obj, "y") == variant_real(runtime, args.first(), "y");
    Ok(Variant::Integer(i64::from(equals)))
}

// ---------------------------------------------------------------- RectF

const RECT_MEMBERS: [&str; 4] = ["x", "y", "width", "height"];

fn rect_f_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "RectF");
            install_rect_f_members(runtime, instance);
            for (index, name) in RECT_MEMBERS.iter().enumerate() {
                runtime.set_object_member(instance, *name, Variant::Real(arg_real(&args, index)));
            }
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "RectF");
    install_rect_f_members(runtime, handle);
    handle
}

/// Builds a RectF-shaped object exactly the way the RectF constructor does;
/// used for every `GdiPlus.RectF` value returned from Layer draw methods.
fn new_rect_f(
    runtime: &mut Runtime<KrkrHost>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> ObjectHandle {
    let handle = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(handle, "RectF");
    install_rect_f_members(runtime, handle);
    store_rect(runtime, handle, [x, y, width, height]);
    handle
}

fn install_rect_f_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for name in RECT_MEMBERS {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, Variant::Real(0.0));
        }
    }
    runtime.register_object_native_property(
        handle,
        "left",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Real(this_real(runtime, this_obj, "x")))
        },
        keep_setter,
    );
    runtime.register_object_native_property(
        handle,
        "top",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Real(this_real(runtime, this_obj, "y")))
        },
        keep_setter,
    );
    runtime.register_object_native_property(
        handle,
        "right",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Real(
                this_real(runtime, this_obj, "x") + this_real(runtime, this_obj, "width"),
            ))
        },
        keep_setter,
    );
    runtime.register_object_native_property(
        handle,
        "bottom",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            Ok(Variant::Real(
                this_real(runtime, this_obj, "y") + this_real(runtime, this_obj, "height"),
            ))
        },
        keep_setter,
    );
    runtime.register_object_native_property(
        handle,
        "location",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let x = this_real(runtime, this_obj, "x");
            let y = this_real(runtime, this_obj, "y");
            Ok(Variant::Object(new_point_f(runtime, x, y)))
        },
        keep_setter,
    );
    runtime.register_object_native_property(
        handle,
        "bounds",
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let rect = this_rect(runtime, this_obj);
            Ok(Variant::Object(new_rect_f(
                runtime, rect[0], rect[1], rect[2], rect[3],
            )))
        },
        keep_setter,
    );
    runtime.register_object_native(handle, "Clone", rect_clone);
    runtime.register_object_native(handle, "Equals", rect_equals);
    runtime.register_object_native(handle, "Inflate", rect_inflate);
    runtime.register_object_native(handle, "InflatePoint", rect_inflate_point);
    runtime.register_object_native(handle, "IntersectsWith", rect_intersects_with);
    runtime.register_object_native(handle, "IsEmptyArea", rect_is_empty_area);
    runtime.register_object_native(handle, "Offset", rect_offset);
    runtime.register_object_native(handle, "Union", rect_union);
}

fn store_rect(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle, rect: [f64; 4]) {
    for (index, name) in RECT_MEMBERS.iter().enumerate() {
        runtime.set_object_member(handle, *name, Variant::Real(rect[index]));
    }
}

fn this_rect(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> [f64; 4] {
    let mut rect = [0.0; 4];
    if let Some(handle) = this_obj {
        for (index, name) in RECT_MEMBERS.iter().enumerate() {
            rect[index] = runtime.object_member(handle, name).to_real().unwrap_or(0.0);
        }
    }
    rect
}

fn variant_rect(runtime: &Runtime<KrkrHost>, value: Option<&Variant>) -> [f64; 4] {
    match value {
        Some(Variant::Object(handle)) => this_rect(runtime, Some(*handle)),
        _ => [0.0; 4],
    }
}

fn rect_clone(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let rect = this_rect(runtime, this_obj);
    Ok(Variant::Object(new_rect_f(
        runtime, rect[0], rect[1], rect[2], rect[3],
    )))
}

fn rect_equals(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let rect = this_rect(runtime, this_obj);
    let other = variant_rect(runtime, args.first());
    Ok(Variant::Integer(i64::from(rect == other)))
}

fn rect_inflate(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    inflate_this(runtime, this_obj, arg_real(&args, 0), arg_real(&args, 1));
    Ok(Variant::Void)
}

fn rect_inflate_point(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let dx = variant_real(runtime, args.first(), "x");
    let dy = variant_real(runtime, args.first(), "y");
    inflate_this(runtime, this_obj, dx, dy);
    Ok(Variant::Void)
}

fn inflate_this(runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, dx: f64, dy: f64) {
    if let Some(this) = this_obj {
        let rect = this_rect(runtime, Some(this));
        store_rect(
            runtime,
            this,
            [
                rect[0] - dx,
                rect[1] - dy,
                rect[2] + 2.0 * dx,
                rect[3] + 2.0 * dy,
            ],
        );
    }
}

fn rect_intersects_with(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let rect = this_rect(runtime, this_obj);
    let other = variant_rect(runtime, args.first());
    let intersects = rect[0] < other[0] + other[2]
        && rect[1] < other[1] + other[3]
        && rect[0] + rect[2] > other[0]
        && rect[1] + rect[3] > other[1];
    Ok(Variant::Integer(i64::from(intersects)))
}

fn rect_is_empty_area(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let rect = this_rect(runtime, this_obj);
    Ok(Variant::Integer(i64::from(
        rect[2] <= 0.0 || rect[3] <= 0.0,
    )))
}

fn rect_offset(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        let rect = this_rect(runtime, Some(this));
        store_rect(
            runtime,
            this,
            [
                rect[0] + arg_real(&args, 0),
                rect[1] + arg_real(&args, 1),
                rect[2],
                rect[3],
            ],
        );
    }
    Ok(Variant::Void)
}

fn rect_union(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let a = variant_rect(runtime, args.get(1));
    let b = variant_rect(runtime, args.get(2));
    let left = a[0].min(b[0]);
    let top = a[1].min(b[1]);
    let width = (a[0] + a[2]).max(b[0] + b[2]) - left;
    let height = (a[1] + a[3]).max(b[1] + b[3]) - top;
    if let Some(Variant::Object(dst)) = args.first() {
        store_rect(runtime, *dst, [left, top, width, height]);
    }
    Ok(Variant::Integer(i64::from(width > 0.0 && height > 0.0)))
}

// ---------------------------------------------------------------- Matrix

const MATRIX_MEMBERS: [&str; 6] = ["m11", "m12", "m21", "m22", "dx", "dy"];
const MATRIX_IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn matrix_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let elements = match args.len() {
                0 => MATRIX_IDENTITY,
                6 => [
                    arg_real(&args, 0),
                    arg_real(&args, 1),
                    arg_real(&args, 2),
                    arg_real(&args, 3),
                    arg_real(&args, 4),
                    arg_real(&args, 5),
                ],
                _ => return Err(TjsError::runtime("invalid parameter")),
            };
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Matrix");
            install_matrix_members(runtime, instance);
            store_matrix(runtime, instance, elements);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Matrix");
    install_matrix_members(runtime, handle);
    handle
}

fn install_matrix_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for (index, name) in MATRIX_MEMBERS.iter().enumerate() {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, *name, Variant::Real(MATRIX_IDENTITY[index]));
        }
    }
    runtime.register_object_native(handle, "OffsetX", matrix_offset_x);
    runtime.register_object_native(handle, "OffsetY", matrix_offset_y);
    runtime.register_object_native(handle, "Equals", matrix_equals);
    runtime.register_object_native(handle, "SetElements", matrix_set_elements);
    runtime.register_object_native(handle, "GetLastStatus", zero);
    runtime.register_object_native(handle, "Invert", matrix_invert);
    runtime.register_object_native(handle, "IsIdentity", matrix_is_identity);
    runtime.register_object_native(handle, "IsInvertible", matrix_is_invertible);
    runtime.register_object_native(handle, "Multiply", matrix_multiply);
    runtime.register_object_native(handle, "Reset", matrix_reset);
    runtime.register_object_native(handle, "Rotate", matrix_rotate);
    runtime.register_object_native(handle, "RotateAt", matrix_rotate_at);
    runtime.register_object_native(handle, "Scale", matrix_scale);
    runtime.register_object_native(handle, "Shear", matrix_shear);
    runtime.register_object_native(handle, "Translate", matrix_translate);
}

fn store_matrix(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle, matrix: [f64; 6]) {
    for (index, name) in MATRIX_MEMBERS.iter().enumerate() {
        runtime.set_object_member(handle, *name, Variant::Real(matrix[index]));
    }
}

fn this_matrix(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> [f64; 6] {
    let mut matrix = [0.0; 6];
    if let Some(handle) = this_obj {
        for (index, name) in MATRIX_MEMBERS.iter().enumerate() {
            matrix[index] = runtime.object_member(handle, name).to_real().unwrap_or(0.0);
        }
    }
    matrix
}

/// Row-vector affine multiply (`p * a * b`): applies `a` first, then `b`.
/// Elements are laid out as [m11, m12, m21, m22, dx, dy], matching GDI+.
fn matrix_mul(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

/// GDI+ prepend semantics: `transform` is applied before the current matrix.
fn matrix_premultiply(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    transform: [f64; 6],
) {
    if let Some(this) = this_obj {
        let matrix = this_matrix(runtime, Some(this));
        store_matrix(runtime, this, matrix_mul(transform, matrix));
    }
}

fn matrix_offset_x(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Real(this_matrix(runtime, this_obj)[4]))
}

fn matrix_offset_y(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Real(this_matrix(runtime, this_obj)[5]))
}

fn matrix_equals(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let matrix = this_matrix(runtime, this_obj);
    let other = match args.first() {
        Some(Variant::Object(handle)) => this_matrix(runtime, Some(*handle)),
        _ => [f64::NAN; 6],
    };
    Ok(Variant::Integer(i64::from(matrix == other)))
}

fn matrix_set_elements(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        store_matrix(
            runtime,
            this,
            [
                arg_real(&args, 0),
                arg_real(&args, 1),
                arg_real(&args, 2),
                arg_real(&args, 3),
                arg_real(&args, 4),
                arg_real(&args, 5),
            ],
        );
    }
    Ok(Variant::Void)
}

fn matrix_invert(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        let m = this_matrix(runtime, Some(this));
        let det = m[0] * m[3] - m[1] * m[2];
        if det != 0.0 {
            store_matrix(
                runtime,
                this,
                [
                    m[3] / det,
                    -m[1] / det,
                    -m[2] / det,
                    m[0] / det,
                    (m[2] * m[5] - m[3] * m[4]) / det,
                    (m[1] * m[4] - m[0] * m[5]) / det,
                ],
            );
        }
    }
    Ok(Variant::Void)
}

fn matrix_is_identity(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(i64::from(
        this_matrix(runtime, this_obj) == MATRIX_IDENTITY,
    )))
}

fn matrix_is_invertible(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let m = this_matrix(runtime, this_obj);
    Ok(Variant::Integer(i64::from(
        m[0] * m[3] - m[1] * m[2] != 0.0,
    )))
}

fn matrix_multiply(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(Variant::Object(other)) = args.first() {
        let transform = this_matrix(runtime, Some(*other));
        matrix_premultiply(runtime, this_obj, transform);
    }
    Ok(Variant::Void)
}

fn matrix_reset(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        store_matrix(runtime, this, MATRIX_IDENTITY);
    }
    Ok(Variant::Void)
}

fn matrix_rotate(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (sin, cos) = arg_real(&args, 0).to_radians().sin_cos();
    matrix_premultiply(runtime, this_obj, [cos, sin, -sin, cos, 0.0, 0.0]);
    Ok(Variant::Void)
}

fn matrix_rotate_at(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let (sin, cos) = arg_real(&args, 0).to_radians().sin_cos();
    let cx = variant_real(runtime, args.get(1), "x");
    let cy = variant_real(runtime, args.get(1), "y");
    // Rotation about (cx, cy): translate to the origin, rotate, translate
    // back; the combined transform is then prepended like the reference.
    let rotate_at = matrix_mul(
        matrix_mul(
            [1.0, 0.0, 0.0, 1.0, -cx, -cy],
            [cos, sin, -sin, cos, 0.0, 0.0],
        ),
        [1.0, 0.0, 0.0, 1.0, cx, cy],
    );
    matrix_premultiply(runtime, this_obj, rotate_at);
    Ok(Variant::Void)
}

fn matrix_scale(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let transform = [arg_real(&args, 0), 0.0, 0.0, arg_real(&args, 1), 0.0, 0.0];
    matrix_premultiply(runtime, this_obj, transform);
    Ok(Variant::Void)
}

fn matrix_shear(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let transform = [1.0, arg_real(&args, 1), arg_real(&args, 0), 1.0, 0.0, 0.0];
    matrix_premultiply(runtime, this_obj, transform);
    Ok(Variant::Void)
}

fn matrix_translate(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let transform = [1.0, 0.0, 0.0, 1.0, arg_real(&args, 0), arg_real(&args, 1)];
    matrix_premultiply(runtime, this_obj, transform);
    Ok(Variant::Void)
}

// ---------------------------------------------------------------- Image

fn image_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Image");
            install_image_members(runtime, instance);
            match args.first() {
                None => {}
                Some(Variant::String(name)) => load_image(runtime, instance, name)?,
                Some(_) => return Err(TjsError::runtime("invalid parameter")),
            }
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Image");
    install_image_members(runtime, handle);
    handle
}

/// The reference decodes the image through GDI+; this stub only verifies the
/// storage is readable and keeps a zero-sized image.
fn load_image(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle, name: &str) -> Result<()> {
    if runtime.host().read_binary_storage(name).is_err() {
        return Err(TjsError::runtime(format!("cannot open:{name}")));
    }
    runtime.set_object_member(instance, "width", Variant::Integer(0));
    runtime.set_object_member(instance, "height", Variant::Integer(0));
    Ok(())
}

fn install_image_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for name in ["width", "height"] {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, Variant::Integer(0));
        }
    }
    runtime.register_object_native(handle, "load", image_load);
    runtime.register_object_native(handle, "Clone", image_clone);
    runtime.register_object_native(handle, "GetBounds", image_get_bounds);
    for name in [
        "GetFlags",
        "GetHeight",
        "GetLastStatus",
        "GetPixelFormat",
        "GetType",
        "GetWidth",
    ] {
        runtime.register_object_native(handle, name, zero);
    }
    for name in ["GetHorizontalResolution", "GetVerticalResolution"] {
        runtime.register_object_native(handle, name, real_zero);
    }
    runtime.register_object_native(handle, "RotateFlip", native_void);
}

fn image_load(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = arg_string(&args, 0);
    if let Some(this) = this_obj {
        load_image(runtime, this, &name)?;
    }
    Ok(Variant::Void)
}

fn image_clone(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let instance = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(instance, "Image");
    install_image_members(runtime, instance);
    if let Some(this) = this_obj {
        for name in ["width", "height"] {
            let value = runtime.object_member(this, name);
            runtime.set_object_member(instance, name, value);
        }
    }
    Ok(Variant::Object(instance))
}

fn image_get_bounds(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)))
}

// ---------------------------------------------------------------- Font

fn font_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            if args.len() != 3 {
                return Err(TjsError::runtime("invalid parameter"));
            }
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Font");
            install_font_members(runtime, instance);
            runtime.set_object_member(
                instance,
                "familyName",
                Variant::String(arg_string(&args, 0)),
            );
            runtime.set_object_member(instance, "emSize", Variant::Real(arg_real(&args, 1)));
            runtime.set_object_member(instance, "style", Variant::Integer(arg_int(&args, 2)));
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Font");
    install_font_members(runtime, handle);
    handle
}

fn install_font_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for (name, value) in [
        ("familyName", Variant::String(String::new())),
        ("emSize", Variant::Real(12.0)),
        ("style", Variant::Integer(0)),
        ("forceSelfPathDraw", Variant::Integer(0)),
    ] {
        if matches!(runtime.object_member(handle, name), Variant::Void) {
            runtime.set_object_member(handle, name, value);
        }
    }
    // Font metric queries stay zeroed: no font backend is consulted.
    for name in [
        "ascent",
        "descent",
        "ascentLeading",
        "descentLeading",
        "lineSpacing",
    ] {
        runtime.register_object_native_property(handle, name, real_zero_getter, keep_setter);
    }
}

// ---------------------------------------------------------------- Appearance

fn appearance_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Appearance");
            install_appearance_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Appearance");
    install_appearance_members(runtime, handle);
    handle
}

fn install_appearance_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    runtime.register_object_native(handle, "clear", native_void);
    runtime.register_object_native(handle, "addBrush", appearance_add_brush);
    runtime.register_object_native(handle, "addPen", native_void);
}

/// Mirrors createBrush() in the reference: a dictionary argument is
/// dispatched on its `type` member (default solid); anything outside the
/// BrushType range 0..=4 is rejected.
fn appearance_add_brush(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(Variant::Object(brush)) = args.first() {
        let brush_type = runtime
            .object_member(*brush, "type")
            .to_integer()
            .unwrap_or(0);
        if !(0..=4).contains(&brush_type) {
            return Err(TjsError::runtime("invalid brush type"));
        }
    }
    Ok(Variant::Void)
}

// ---------------------------------------------------------------- Path

fn path_constructor(runtime: &mut Runtime<KrkrHost>) -> ObjectHandle {
    let handle = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = fresh_instance(runtime, this_obj);
            runtime.add_object_class_info(instance, "Path");
            install_path_members(runtime, instance);
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(handle, "Path");
    install_path_members(runtime, handle);
    handle
}

fn install_path_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", native_void);
    for name in [
        "startFigure",
        "closeFigure",
        "drawArc",
        "drawPie",
        "drawBezier",
        "drawBeziers",
        "drawClosedCurve",
        "drawClosedCurve2",
        "drawCurve",
        "drawCurve2",
        "drawCurve3",
        "drawEllipse",
        "drawLine",
        "drawLines",
        "drawPolygon",
        "drawRectangle",
        "drawRectangles",
    ] {
        runtime.register_object_native(handle, name, native_void);
    }
}

// ---------------------------------------------------------------- Layer

fn install_layer_ex_draw(runtime: &mut Runtime<KrkrHost>) {
    let Variant::Object(layer) = runtime.global_member("Layer") else {
        return;
    };

    // Plain rw property members with the reference defaults
    // (updateWhenDraw=true, SmoothingModeAntiAlias, TextRenderingHintAntiAlias).
    for (name, value) in [
        ("updateWhenDraw", 1),
        ("smoothingMode", 4),
        ("textRenderingHint", 4),
        ("record", 0),
    ] {
        if matches!(runtime.object_member(layer, name), Variant::Void) {
            runtime.set_object_member(layer, name, Variant::Integer(value));
        }
    }

    for &(name, value) in ENCODER_VALUE_CONSTANTS {
        runtime.set_object_member(layer, name, Variant::Integer(value));
    }

    // View/world transform control and clear: void no-ops.
    for name in [
        "setViewTransform",
        "resetViewTransform",
        "rotateViewTransform",
        "scaleViewTransform",
        "translateViewTransform",
        "setTransform",
        "resetTransform",
        "rotateTransform",
        "scaleTransform",
        "translateTransform",
        "clear",
    ] {
        register_unless_closure(runtime, layer, name, native_void);
    }

    // Draw methods with an explicit destination rectangle echo x/y/width/
    // height into the returned RectF (argument positions per the reference
    // LayerExDraw.hpp signatures; drawImage's size comes from the zero-sized
    // stub image, and drawImageRect copies unscaled so swidth/sheight apply).
    register_unless_closure(
        runtime,
        layer,
        "drawArc",
        layer_draw_rect(Some(1), Some(2), Some(3), Some(4)),
    );
    register_unless_closure(
        runtime,
        layer,
        "drawPie",
        layer_draw_rect(Some(1), Some(2), Some(3), Some(4)),
    );
    register_unless_closure(
        runtime,
        layer,
        "drawEllipse",
        layer_draw_rect(Some(1), Some(2), Some(3), Some(4)),
    );
    register_unless_closure(
        runtime,
        layer,
        "drawRectangle",
        layer_draw_rect(Some(1), Some(2), Some(3), Some(4)),
    );
    register_unless_closure(
        runtime,
        layer,
        "drawImage",
        layer_draw_rect(Some(0), Some(1), None, None),
    );
    register_unless_closure(
        runtime,
        layer,
        "drawImageRect",
        layer_draw_rect(Some(0), Some(1), Some(5), Some(6)),
    );
    register_unless_closure(
        runtime,
        layer,
        "drawImageStretch",
        layer_draw_rect(Some(0), Some(1), Some(2), Some(3)),
    );
    for name in [
        "drawPath",
        "drawBezier",
        "drawBeziers",
        "drawClosedCurve",
        "drawClosedCurve2",
        "drawCurve",
        "drawCurve2",
        "drawCurve3",
        "drawLine",
        "drawLines",
        "drawPolygon",
        "drawRectangles",
        "drawPathString",
        "drawString",
        "drawImageAffine",
    ] {
        register_unless_closure(
            runtime,
            layer,
            name,
            layer_draw_rect(None, None, None, None),
        );
    }
    register_unless_closure(runtime, layer, "measureString", layer_measure_string);
    register_unless_closure(
        runtime,
        layer,
        "measureStringInternal",
        layer_measure_string,
    );

    register_unless_closure(runtime, layer, "getRecordImage", native_void);
    register_unless_closure(runtime, layer, "redrawRecord", zero);
    register_unless_closure(runtime, layer, "saveRecord", zero);
    // The reference loadRecord() always returns false, even on success.
    register_unless_closure(runtime, layer, "loadRecord", zero);
    register_unless_closure(runtime, layer, "saveImage", layer_save_image);
    register_unless_closure(runtime, layer, "getColorRegionRects", empty_array);
}

/// GDI+ EncoderValue constants attached to Layer (the reference `ENUM(...)`
/// block, main.cpp:889-906; the enum starts after ColorTypeCMYK/ColorTypeYCCK).
const ENCODER_VALUE_CONSTANTS: &[(&str, i64)] = &[
    ("EncoderValueCompressionLZW", 2),
    ("EncoderValueCompressionCCITT3", 3),
    ("EncoderValueCompressionCCITT4", 4),
    ("EncoderValueCompressionRle", 5),
    ("EncoderValueCompressionNone", 6),
    ("EncoderValueScanMethodInterlaced", 7),
    ("EncoderValueScanMethodNonInterlaced", 8),
    ("EncoderValueVersionGif87", 9),
    ("EncoderValueVersionGif89", 10),
    ("EncoderValueRenderProgressive", 11),
    ("EncoderValueRenderNonProgressive", 12),
    ("EncoderValueTransformRotate90", 13),
    ("EncoderValueTransformRotate180", 14),
    ("EncoderValueTransformRotate270", 15),
    ("EncoderValueTransformFlipHorizontal", 16),
    ("EncoderValueTransformFlipVertical", 17),
];

/// Builds a Layer draw-method stub returning a fresh `GdiPlus.RectF`; the
/// given argument positions are echoed into x/y/width/height (as Reals),
/// missing positions stay zero.
fn layer_draw_rect(
    x: Option<usize>,
    y: Option<usize>,
    width: Option<usize>,
    height: Option<usize>,
) -> impl Fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant> {
    move |runtime, _this_obj, args| {
        let pick = |index: Option<usize>| index.map(|i| arg_real(&args, i)).unwrap_or(0.0);
        Ok(Variant::Object(new_rect_f(
            runtime,
            pick(x),
            pick(y),
            pick(width),
            pick(height),
        )))
    }
}

fn layer_measure_string(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    runtime
        .host_mut()
        .log("layerExDraw.dll: measureString is not implemented; returning an empty RectF");
    Ok(Variant::Object(new_rect_f(runtime, 0.0, 0.0, 0.0, 0.0)))
}

fn layer_save_image(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = arg_string(&args, 0);
    runtime
        .host_mut()
        .log(&format!("layerExDraw.dll: saveImage({name}) is a no-op"));
    Ok(Variant::Integer(0))
}

// ---------------------------------------------------------------- helpers

/// Constructor receiver: use the VM-provided `this` when it is a real
/// instance object, otherwise allocate a fresh one (motion_player idiom).
fn fresh_instance(runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> ObjectHandle {
    this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object())
}

/// Registers a native method on the engine-owned Layer class unless a script
/// already defined that member as a Closure.
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

fn arg_real(args: &[Variant], index: usize) -> f64 {
    args.get(index)
        .map(|value| value.to_real().unwrap_or(0.0))
        .unwrap_or(0.0)
}

fn arg_int(args: &[Variant], index: usize) -> i64 {
    args.get(index)
        .map(|value| value.to_integer().unwrap_or(0))
        .unwrap_or(0)
}

fn arg_string(args: &[Variant], index: usize) -> String {
    args.get(index)
        .map(|value| value.to_tjs_string().unwrap_or_default())
        .unwrap_or_default()
}

fn this_real(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, name: &str) -> f64 {
    match this_obj {
        Some(handle) => runtime.object_member(handle, name).to_real().unwrap_or(0.0),
        None => 0.0,
    }
}

fn variant_real(runtime: &Runtime<KrkrHost>, value: Option<&Variant>, name: &str) -> f64 {
    match value {
        Some(Variant::Object(handle)) => runtime
            .object_member(*handle, name)
            .to_real()
            .unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Setter for read-only native properties: ignores the assigned value.
fn keep_setter(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _value: Variant,
) -> Result<()> {
    Ok(())
}

fn real_zero_getter(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    Ok(Variant::Real(0.0))
}

fn real_zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Real(0.0))
}

fn zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

fn empty_array(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}
