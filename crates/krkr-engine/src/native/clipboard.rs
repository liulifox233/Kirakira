use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{install_static_object, native_void};

pub(crate) fn install_clipboard(runtime: &mut Runtime<KrkrHost>) {
    let clipboard = install_static_object(runtime, "Clipboard");
    // `tTJSNC_Clipboard` declares an empty `finalize` with
    // `TJS_DECL_EMPTY_FINALIZE_METHOD` (`ClipboardIntf.cpp:41`); scripts reach
    // it as `Clipboard.finalize(...)` while tearing a session down.
    runtime.register_object_native(clipboard, "finalize", native_void);
    // `hasFormat` declares `if(numparams < 1) return TJS_E_BADPARAMCOUNT;`
    // (`utils/ClipboardIntf.cpp:55`), so the floor sits at the registration
    // site.
    runtime.register_object_native_with_arg_count(
        clipboard,
        "hasFormat",
        NativeArgCount::AtLeast(1),
        clipboard_has_format,
    );
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

#[cfg(test)]
mod tests {
    use crate::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    /// M175.  `Clipboard.hasFormat` declares
    /// `if(numparams < 1) return TJS_E_BADPARAMCOUNT;`
    /// (`utils/ClipboardIntf.cpp:55`), so a short call reports
    /// `TJS_E_BADPARAMCOUNT` (-1004) before the handler runs.
    #[test]
    fn clipboard_has_format_floor_rejects_short_calls() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "clipboard_floors.tjs",
                r#"
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                var rejected = message(function() { Clipboard.hasFormat(); });
                var accepted = Clipboard.hasFormat(1) + ":" + Clipboard.hasFormat(0);
                return rejected + "|" + accepted;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("Invalid argument count|1:0".to_string())
        );
        // The identity from Rust: the dispatch check answers
        // `TJS_E_BADPARAMCOUNT` (-1004) before the handler.
        let error = engine
            .execute_expression("clipboard_floors.tjs", "Clipboard.hasFormat()")
            .expect_err("a short hasFormat call must fail");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        assert_eq!(error.tjs_error_code(), Some(-1004));
        assert_eq!(error.message, "Invalid argument count");
    }
}
