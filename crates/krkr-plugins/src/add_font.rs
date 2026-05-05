use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

pub struct AddFontPlugin;

impl KrkrPlugin for AddFontPlugin {
    fn name(&self) -> &str {
        "addFont.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(system) = runtime.global_member("System") else {
            return Err(TjsError::runtime(
                "addFont.dll requires the native System object to be installed first",
            ));
        };
        runtime.register_object_native(system, "addFont", system_add_font);
        Ok(())
    }
}

fn system_add_font(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(storage) = args.first() else {
        return Err(TjsError::runtime("System.addFont requires a storage name"));
    };
    let storage = storage.to_tjs_string()?;
    let Ok(bytes) = runtime.host().read_binary_storage(&storage) else {
        return Ok(Variant::Void);
    };
    let loaded = runtime
        .host_mut()
        .font_system_mut()
        .load_font_data(storage.clone(), bytes)
        .is_ok();
    if loaded {
        runtime.host_mut().log(&format!(
            "System.addFont loaded through addFont.dll: {storage}"
        ));
    }
    Ok(Variant::Integer(i64::from(loaded)))
}
