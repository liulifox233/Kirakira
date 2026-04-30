use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{install_static_object, required_arg_string};

pub(crate) fn install_plugins(runtime: &mut Runtime<KrkrHost>) {
    let plugins = install_static_object(runtime, "Plugins");
    runtime.register_object_native(plugins, "link", plugins_link);
    runtime.register_object_native(plugins, "unlink", plugins_unlink);
    runtime.register_object_native(plugins, "getList", plugins_get_list);
}

fn plugins_link(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Plugins.link")?;
    runtime.host_mut().link_plugin(&name);
    Ok(Variant::Void)
}

fn plugins_unlink(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Plugins.unlink")?;
    Ok(Variant::Integer(i64::from(
        runtime.host_mut().unlink_plugin(&name),
    )))
}

fn plugins_get_list(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let values = runtime
        .host()
        .linked_plugins()
        .map(|name| Variant::String(name.to_string()))
        .collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}
