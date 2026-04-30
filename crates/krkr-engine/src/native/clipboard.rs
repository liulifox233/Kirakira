use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::install_static_object;

pub(crate) fn install_clipboard(runtime: &mut Runtime<KrkrHost>) {
    let clipboard = install_static_object(runtime, "Clipboard");
    runtime.register_object_native(clipboard, "hasFormat", clipboard_has_format);
    runtime.set_object_member(clipboard, "asText", Variant::String(String::new()));
}

fn clipboard_has_format(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let format = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0);
    Ok(Variant::Integer(i64::from(format == 1)))
}
