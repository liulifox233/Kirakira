use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{install_static_object, native_void};

pub(crate) fn install_debug(runtime: &mut Runtime<KrkrHost>) {
    let debug = install_static_object(runtime, "Debug");
    // `tTJSNC_Debug` declares an empty `finalize` with
    // `TJS_DECL_EMPTY_FINALIZE_METHOD` (`DebugIntf.cpp:626`); a script may
    // call `Debug.finalize()` while tearing a session down.
    runtime.register_object_native(debug, "finalize", native_void);
    // The `tTJSNC_Debug` members that declare
    // `if(numparams < 1) return TJS_E_BADPARAMCOUNT;` carry the floor at the
    // registration site (`utils/DebugIntf.cpp:640-725`).
    runtime.register_object_native_with_arg_count(
        debug,
        "message",
        NativeArgCount::AtLeast(1),
        debug_message,
    );
    runtime.register_object_native_with_arg_count(
        debug,
        "notice",
        NativeArgCount::AtLeast(1),
        debug_message,
    );
    runtime.register_object_native(debug, "startLogToFile", native_void);
    runtime.register_object_native(debug, "logAsError", debug_message);
    runtime.register_object_native_with_arg_count(
        debug,
        "addLoggingHandler",
        NativeArgCount::AtLeast(1),
        native_void,
    );
    runtime.register_object_native_with_arg_count(
        debug,
        "removeLoggingHandler",
        NativeArgCount::AtLeast(1),
        native_void,
    );
    runtime.register_object_native(debug, "getLastLog", debug_get_last_log);
    runtime.set_object_member(debug, "logLocation", Variant::String(String::new()));
    runtime.set_object_member(debug, "logToFileOnError", Variant::Integer(0));
    runtime.set_object_member(debug, "clearLogFileOnError", Variant::Integer(0));
}

fn debug_message(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let message = args
        .iter()
        .map(Variant::to_tjs_string)
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    runtime.host_mut().log(&message);
    Ok(Variant::Void)
}

fn debug_get_last_log(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        runtime.host().logs().last().cloned().unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use crate::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    /// M175.  The `tTJSNC_Debug` methods that declare
    /// `if(numparams < 1) return TJS_E_BADPARAMCOUNT;`
    /// (`utils/DebugIntf.cpp:640-725`) carry the floor at their registration
    /// site, so a short call reports `TJS_E_BADPARAMCOUNT` (-1004) before the
    /// handler runs.
    #[test]
    fn debug_method_floors_reject_short_calls() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "debug_floors.tjs",
                r#"
                function message(body) {
                    try { body(); } catch (e) { return e.message; }
                    return "";
                }
                return [
                    message(function() { Debug.message(); }),
                    message(function() { Debug.notice(); }),
                    message(function() { Debug.addLoggingHandler(); }),
                    message(function() { Debug.removeLoggingHandler(); })
                ].join("|");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(["Invalid argument count"; 4].join("|"))
        );
        // The identity from Rust: the dispatch check answers
        // `TJS_E_BADPARAMCOUNT` (-1004) before the handler.
        let error = engine
            .execute_expression("debug_floors.tjs", "Debug.message()")
            .expect_err("a short message call must fail");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        assert_eq!(error.tjs_error_code(), Some(-1004));
        assert_eq!(error.message, "Invalid argument count");
    }

    /// The other half of the contract: every floor accepts the reference
    /// arity (`Debug.message`/`notice` are variadic from one argument up).
    #[test]
    fn debug_reference_arity_calls_are_not_rejected() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let value = engine
            .execute_script(
                "debug_floors_exact.tjs",
                r#"
                var problems = "";
                function check(body) {
                    try { body(); } catch (e) {
                        if (e.message === "Invalid argument count") { problems += "bad; "; }
                    }
                }
                check(function() { Debug.message("one"); });
                check(function() { Debug.message("one", "two"); });
                check(function() { Debug.notice("one"); });
                check(function() { Debug.notice("one", "two"); });
                check(function() { Debug.addLoggingHandler(function(text) {}); });
                check(function() { Debug.removeLoggingHandler(function(text) {}); });
                return problems;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String(String::new()));
    }
}
