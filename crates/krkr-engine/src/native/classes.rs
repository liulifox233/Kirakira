use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::register_stub_method;

pub(crate) fn install_native_class(
    runtime: &mut Runtime<KrkrHost>,
    spec: &'static NativeClassSpec,
    global: bool,
) -> ObjectHandle {
    let handle = runtime.alloc_native_function(
        move |runtime: &mut Runtime<KrkrHost>,
              _this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| { construct_native_instance(runtime, spec, args) },
    );
    runtime.add_object_class_info(handle, spec.name);
    install_methods(runtime, handle, spec.name, spec.static_methods);
    install_properties(runtime, handle, spec.static_properties);
    if global {
        runtime.set_global_member(spec.name, Variant::Object(handle));
    }
    handle
}

fn construct_native_instance(
    runtime: &mut Runtime<KrkrHost>,
    spec: &'static NativeClassSpec,
    args: Vec<Variant>,
) -> Result<Variant> {
    let handle = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(handle, spec.name);
    runtime.set_object_member(
        handle,
        "__className",
        Variant::String(spec.name.to_string()),
    );
    install_methods(runtime, handle, spec.name, spec.methods);
    install_properties(runtime, handle, spec.properties);
    apply_constructor_defaults(runtime, handle, spec.name, &args)?;
    Ok(Variant::Object(handle))
}

fn install_methods(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &'static str,
    methods: &'static [&'static str],
) {
    for method in methods {
        register_stub_method(runtime, handle, class_name, method);
    }
}

fn install_properties(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    properties: &'static [&'static str],
) {
    for property in properties {
        runtime.set_object_member(handle, *property, Variant::Void);
    }
}

fn apply_constructor_defaults(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    class_name: &str,
    args: &[Variant],
) -> Result<()> {
    match class_name {
        "Rect" => {
            let left = args
                .first()
                .map(Variant::to_integer)
                .transpose()?
                .unwrap_or(0);
            let top = args
                .get(1)
                .map(Variant::to_integer)
                .transpose()?
                .unwrap_or(0);
            let right = args
                .get(2)
                .map(Variant::to_integer)
                .transpose()?
                .unwrap_or(0);
            let bottom = args
                .get(3)
                .map(Variant::to_integer)
                .transpose()?
                .unwrap_or(0);
            runtime.set_object_member(handle, "left", Variant::Integer(left));
            runtime.set_object_member(handle, "top", Variant::Integer(top));
            runtime.set_object_member(handle, "right", Variant::Integer(right));
            runtime.set_object_member(handle, "bottom", Variant::Integer(bottom));
            runtime.set_object_member(handle, "width", Variant::Integer(right - left));
            runtime.set_object_member(handle, "height", Variant::Integer(bottom - top));
        }
        "Timer" => {
            runtime.set_object_member(handle, "enabled", Variant::Integer(0));
            runtime.set_object_member(handle, "interval", Variant::Integer(1000));
            runtime.set_object_member(handle, "capacity", Variant::Integer(1));
            runtime.set_object_member(handle, "mode", Variant::Integer(0));
        }
        "Window" => {
            runtime.set_object_member(handle, "visible", Variant::Integer(0));
            runtime.set_object_member(handle, "caption", Variant::String(String::new()));
            runtime.set_object_member(handle, "width", Variant::Integer(0));
            runtime.set_object_member(handle, "height", Variant::Integer(0));
        }
        "Layer" | "Bitmap" | "BitmapLayerTreeOwner" => {
            runtime.set_object_member(handle, "width", Variant::Integer(0));
            runtime.set_object_member(handle, "height", Variant::Integer(0));
        }
        "WaveSoundBuffer" => {
            runtime.set_object_member(handle, "status", Variant::String("unload".to_string()));
            runtime.set_object_member(handle, "volume", Variant::Integer(100000));
            runtime.set_object_member(handle, "pan", Variant::Integer(0));
        }
        _ => {}
    }
    Ok(())
}

pub(crate) struct NativeClassSpec {
    name: &'static str,
    methods: &'static [&'static str],
    properties: &'static [&'static str],
    static_methods: &'static [&'static str],
    static_properties: &'static [&'static str],
}

pub(crate) static TIMER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Timer",
    methods: &["onTimer"],
    properties: &["interval", "enabled", "capacity", "mode"],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static ASYNC_TRIGGER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "AsyncTrigger",
    methods: &["trigger", "cancel", "onFire"],
    properties: &["cached", "mode"],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static RECT_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Rect",
    methods: &[
        "isEmpty",
        "setSize",
        "setOffset",
        "addOffset",
        "clear",
        "set",
        "clip",
        "union",
        "intersects",
        "included",
        "includedPos",
        "equal",
    ],
    properties: &[
        "width",
        "height",
        "left",
        "top",
        "right",
        "bottom",
        "nativeArray",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static BITMAP_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Bitmap",
    methods: &[
        "getPixel",
        "setPixel",
        "getMaskPixel",
        "setMaskPixel",
        "independ",
        "setSize",
        "copyFrom",
        "save",
        "load",
        "loadAsync",
        "loadHeader",
        "getSaveOption",
        "onLoaded",
    ],
    properties: &[
        "width",
        "height",
        "buffer",
        "bufferForWrite",
        "bufferPitch",
        "loading",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static IMAGE_FUNCTION_CLASS: NativeClassSpec = NativeClassSpec {
    name: "ImageFunction",
    methods: &[
        "operateAffine",
        "operateRect",
        "operateStretch",
        "flipLR",
        "flipUD",
        "adjustGamma",
        "doBoxBlur",
        "doGrayScale",
        "fillRect",
        "colorRect",
        "drawText",
        "drawGlyph",
    ],
    properties: &[],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static BITMAP_LAYER_TREE_OWNER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "BitmapLayerTreeOwner",
    methods: &[
        "fireClick",
        "fireDoubleClick",
        "fireMouseDown",
        "fireMouseUp",
        "fireMouseMove",
        "fireMouseWheel",
        "fireReleaseCapture",
        "fireMouseOutOfWindow",
        "fireTouchDown",
        "fireTouchUp",
        "fireTouchMove",
        "fireTouchScaling",
        "fireTouchRotate",
        "fireMultiTouch",
        "fireKeyDown",
        "fireKeyUp",
        "fireKeyPress",
        "fireDisplayRotate",
        "fireRecheckInputState",
        "onSetMouseCursor",
        "onGetCursorPos",
        "onSetCursorPos",
        "onReleaseMouseCapture",
        "onSetHintText",
        "onResizeLayer",
        "onChangeLayerImage",
        "onSetAttentionPoint",
        "onDisableAttentionPoint",
        "onSetImeMode",
        "onResetImeMode",
    ],
    properties: &[
        "width",
        "height",
        "bitmap",
        "layerTreeOwnerInterface",
        "focusedLayer",
        "primaryLayer",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static WINDOW_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Window",
    methods: &[
        "close",
        "beginMove",
        "bringToFront",
        "update",
        "showModal",
        "setMaskRegion",
        "removeMaskRegion",
        "add",
        "remove",
        "setSize",
        "setMinSize",
        "setMaxSize",
        "setPos",
        "setLayerPos",
        "setInnerSize",
        "setZoom",
        "hideMouseCursor",
        "postInputEvent",
        "onResize",
        "onMouseEnter",
        "onMouseLeave",
        "onClick",
        "onDoubleClick",
        "onMouseDown",
        "onMouseUp",
        "onMouseMove",
        "onMouseWheel",
        "onTouchDown",
        "onTouchUp",
        "onTouchMove",
        "onTouchScaling",
        "onTouchRotate",
        "onMultiTouch",
        "onKeyDown",
        "onKeyUp",
        "onKeyPress",
        "onFileDrop",
        "onCloseQuery",
        "onPopupHide",
        "onActivate",
        "onDeactivate",
        "onDisplayRotate",
        "findFullScreenCandidates",
        "registerMessageReceiver",
        "getTouchPoint",
        "getTouchVelocity",
        "getMouseVelocity",
        "resetMouseVelocity",
    ],
    properties: &[
        "visible",
        "caption",
        "width",
        "height",
        "minWidth",
        "minHeight",
        "maxWidth",
        "maxHeight",
        "left",
        "top",
        "focusable",
        "layerLeft",
        "layerTop",
        "innerSunken",
        "innerWidth",
        "innerHeight",
        "zoomNumer",
        "zoomDenom",
        "borderStyle",
        "stayOnTop",
        "showScrollBars",
        "useMouseKey",
        "trapKey",
        "imeMode",
        "mouseCursorState",
        "fullScreen",
        "mainWindow",
        "focusedLayer",
        "primaryLayer",
        "waitVSync",
        "layerTreeOwnerInterface",
        "HWND",
        "drawDevice",
        "touchScaleThreshold",
        "touchRotateThreshold",
        "touchPointCount",
        "hintDelay",
        "enableTouch",
        "displayOrientation",
        "displayRotate",
    ],
    static_methods: &[],
    static_properties: &["mainWindow"],
};

pub(crate) static LAYER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Layer",
    methods: &[
        "moveBefore",
        "moveBehind",
        "bringToBack",
        "bringToFront",
        "saveLayerImage",
        "loadImages",
        "loadProvinceImage",
        "getMainPixel",
        "setMainPixel",
        "getMaskPixel",
        "setMaskPixel",
        "getProvincePixel",
        "setProvincePixel",
        "getLayerAt",
        "setPos",
        "setSize",
        "setSizeToImageSize",
        "setImagePos",
        "setImageSize",
        "independMainImage",
        "independProvinceImage",
        "setClip",
        "fillRect",
        "colorRect",
        "drawText",
        "drawGlyph",
        "piledCopy",
        "copyRect",
        "copy9Patch",
        "operateRect",
        "stretchCopy",
        "operateStretch",
        "affineCopy",
        "operateAffine",
        "doBoxBlur",
        "adjustGamma",
        "doGrayScale",
        "flipLR",
        "flipUD",
        "convertType",
        "update",
        "setCursorPos",
        "releaseCapture",
        "releaseTouchCapture",
        "focus",
        "focusPrev",
        "focusNext",
        "setMode",
        "removeMode",
        "setAttentionPos",
        "beginTransition",
        "stopTransition",
        "assignImages",
        "dump",
        "copyToBitmapFromMainImage",
        "copyFromBitmapToMainImage",
        "onHitTest",
        "onClick",
        "onDoubleClick",
        "onMouseDown",
        "onMouseUp",
        "onMouseMove",
        "onMouseEnter",
        "onMouseLeave",
        "onTouchDown",
        "onTouchUp",
        "onTouchMove",
        "onTouchScaling",
        "onTouchRotate",
        "onMultiTouch",
        "onBlur",
        "onFocus",
        "onNodeEnabled",
        "onNodeDisabled",
        "onKeyDown",
        "onKeyUp",
        "onKeyPress",
        "onMouseWheel",
        "onSearchPrevFocusable",
        "onSearchNextFocusable",
        "onBeforeFocus",
        "onPaint",
        "onTransitionCompleted",
    ],
    properties: &[
        "parent",
        "children",
        "order",
        "absolute",
        "absoluteOrderMode",
        "visible",
        "cached",
        "nodeVisible",
        "opacity",
        "window",
        "isPrimary",
        "left",
        "top",
        "width",
        "height",
        "imageLeft",
        "imageTop",
        "imageWidth",
        "imageHeight",
        "type",
        "face",
        "holdAlpha",
        "clipLeft",
        "clipTop",
        "clipWidth",
        "clipHeight",
        "imageModified",
        "hitType",
        "hitThreshold",
        "cursor",
        "cursorX",
        "cursorY",
        "hint",
        "showParentHint",
        "ignoreHintSensing",
        "focusable",
        "prevFocusable",
        "nextFocusable",
        "joinFocusChain",
        "nodeFocusable",
        "focused",
        "enabled",
        "nodeEnabled",
        "attentionLeft",
        "attentionTop",
        "useAttention",
        "imeMode",
        "callOnPaint",
        "font",
        "name",
        "neutralColor",
        "hasImage",
        "mainImageBuffer",
        "mainImageBufferForWrite",
        "mainImageBufferPitch",
        "provinceImageBuffer",
        "provinceImageBufferForWrite",
        "provinceImageBufferPitch",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static FONT_CLASS: NativeClassSpec = NativeClassSpec {
    name: "Font",
    methods: &[
        "getTextWidth",
        "getTextHeight",
        "getEscWidthX",
        "getEscWidthY",
        "getEscHeightX",
        "getEscHeightY",
        "getGlyphDrawRect",
        "getList",
        "mapPrerenderedFont",
        "unmapPrerenderedFont",
    ],
    properties: &[
        "face",
        "height",
        "bold",
        "italic",
        "strikeout",
        "underline",
        "angle",
        "faceIsFileName",
        "rasterizer",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static WAVE_SOUND_BUFFER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "WaveSoundBuffer",
    methods: &[
        "open",
        "play",
        "stop",
        "fade",
        "stopFade",
        "setPos",
        "onStatusChanged",
        "onFadeCompleted",
        "onLabel",
        "freeDirectSound",
        "getVisBuffer",
    ],
    properties: &[
        "position",
        "samplePosition",
        "paused",
        "totalTime",
        "looping",
        "volume",
        "volume2",
        "pan",
        "posX",
        "posY",
        "posZ",
        "status",
        "frequency",
        "bits",
        "channels",
        "flags",
        "labels",
        "filters",
        "globalVolume",
        "globalFocusMode",
        "useVisBuffer",
    ],
    static_methods: &["freeDirectSound"],
    static_properties: &["globalVolume", "globalFocusMode", "useVisBuffer"],
};

pub(crate) static PHASE_VOCODER_CLASS: NativeClassSpec = NativeClassSpec {
    name: "PhaseVocoder",
    methods: &[],
    properties: &["interface", "window", "overlap", "pitch", "time"],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static VIDEO_OVERLAY_CLASS: NativeClassSpec = NativeClassSpec {
    name: "VideoOverlay",
    methods: &[
        "open",
        "play",
        "stop",
        "close",
        "setPos",
        "setSize",
        "setBounds",
        "pause",
        "rewind",
        "prepare",
        "setSegmentLoop",
        "cancelSegmentLoop",
        "setPeriodEvent",
        "cancelPeriodEvent",
        "selectAudioStream",
        "setMixingLayer",
        "resetMixingLayer",
        "onStatusChanged",
        "onCallbackCommand",
        "onPeriod",
        "onFrameUpdate",
    ],
    properties: &[
        "position",
        "left",
        "top",
        "width",
        "height",
        "originalWidth",
        "originalHeight",
        "visible",
        "loop",
        "frame",
        "fps",
        "numberOfFrame",
        "totalTime",
        "layer1",
        "layer2",
        "mode",
        "playRate",
        "segmentLoopStartFrame",
        "segmentLoopEndFrame",
        "periodEventFrame",
        "audioBalance",
        "audioVolume",
        "numberOfAudioStream",
        "enabledAudioStream",
        "numberOfVideoStream",
        "enabledVideoStream",
        "mixingMovieAlpha",
        "mixingMovieBGColor",
        "contrastRangeMin",
        "contrastRangeMax",
        "contrastDefaultValue",
        "contrastStepSize",
        "contrast",
        "brightnessRangeMin",
        "brightnessRangeMax",
        "brightnessDefaultValue",
        "brightnessStepSize",
        "brightness",
        "hueRangeMin",
        "hueRangeMax",
        "hueDefaultValue",
        "hueStepSize",
        "hue",
        "saturationRangeMin",
        "saturationRangeMax",
        "saturationDefaultValue",
        "saturationStepSize",
        "saturation",
    ],
    static_methods: &[],
    static_properties: &[],
};

pub(crate) static BASIC_DRAW_DEVICE_CLASS: NativeClassSpec = NativeClassSpec {
    name: "BasicDrawDevice",
    methods: &["recreate"],
    properties: &["interface"],
    static_methods: &[],
    static_properties: &[],
};
