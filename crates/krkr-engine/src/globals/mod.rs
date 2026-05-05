mod constants;

use krkr_tjs2::runtime::{Runtime, Variant};

use crate::{
    host::KrkrHost,
    native::{
        ASYNC_TRIGGER_CLASS, BASIC_DRAW_DEVICE_CLASS, BITMAP_CLASS, BITMAP_LAYER_TREE_OWNER_CLASS,
        FONT_CLASS, IMAGE_FUNCTION_CLASS, LAYER_CLASS, MENU_ITEM_CLASS, PHASE_VOCODER_CLASS,
        RECT_CLASS, TIMER_CLASS, VIDEO_OVERLAY_CLASS, WAVE_SOUND_BUFFER_CLASS, WINDOW_CLASS,
        install_clipboard, install_debug, install_kag_parser, install_native_class,
        install_plugins, install_scripts, install_storages, install_system,
    },
};

use constants::install_tvp_constants;

pub(crate) fn install_tvp_globals(runtime: &mut Runtime<KrkrHost>) {
    install_tvp_constants(runtime);
    install_debug(runtime);
    install_system(runtime);
    install_storages(runtime);
    install_scripts(runtime);
    install_plugins(runtime);
    install_kag_parser(runtime);
    install_clipboard(runtime);

    let font = install_native_class(runtime, &FONT_CLASS, true);
    let layer = install_native_class(runtime, &LAYER_CLASS, true);
    let _ = (font, layer);
    install_native_class(runtime, &TIMER_CLASS, true);
    install_native_class(runtime, &ASYNC_TRIGGER_CLASS, true);
    install_native_class(runtime, &RECT_CLASS, true);
    install_native_class(runtime, &BITMAP_CLASS, true);
    install_native_class(runtime, &IMAGE_FUNCTION_CLASS, true);
    install_native_class(runtime, &BITMAP_LAYER_TREE_OWNER_CLASS, true);
    install_native_class(runtime, &VIDEO_OVERLAY_CLASS, true);
    install_native_class(runtime, &MENU_ITEM_CLASS, true);

    let wave = install_native_class(runtime, &WAVE_SOUND_BUFFER_CLASS, true);
    let phase = install_native_class(runtime, &PHASE_VOCODER_CLASS, false);
    runtime.set_object_member(wave, "PhaseVocoder", Variant::Object(phase));

    let window = install_native_class(runtime, &WINDOW_CLASS, true);
    let draw_device = install_native_class(runtime, &BASIC_DRAW_DEVICE_CLASS, false);
    runtime.set_object_member(window, "BasicDrawDevice", Variant::Object(draw_device));
    runtime.set_object_member(
        window,
        "PassThroughDrawDevice",
        Variant::Object(draw_device),
    );
    runtime.set_object_member(draw_device, "dtNone", Variant::Integer(0));
    runtime.set_object_member(draw_device, "dtDrawDib", Variant::Integer(1));
    runtime.set_object_member(draw_device, "dtDBGDI", Variant::Integer(2));
    runtime.set_object_member(draw_device, "dtDBDD", Variant::Integer(3));
    runtime.set_object_member(draw_device, "dtDBD3D", Variant::Integer(4));
}
