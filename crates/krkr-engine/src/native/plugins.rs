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
    // KRKR only publishes a plugin's classes when its module is loaded here,
    // and `TVPLoadPlugin` returns early once the same module is loaded. Mirror
    // that: install on the first explicit link so a boot script that shadowed
    // a class name (a kirikiroid2 `patch.tjs` does) does not keep the stub.
    if let Some(plugin) = runtime.host_mut().plugin_to_install(&name) {
        plugin.register(runtime)?;
    }
    runtime.host_mut().link_plugin(&name);
    Ok(Variant::Void)
}

fn plugins_unlink(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = required_arg_string(&args, 0, "Plugins.unlink")?;
    // The reference unregisters a plugin's media as its module goes away
    // (`V2Unlink`, `varfile/Main.cpp:451-477`), so the plugin's `unregister`
    // runs before its name leaves the registry.
    if let Some(plugin) = runtime.host().plugin_to_unregister(&name) {
        plugin.unregister(runtime)?;
    }
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
