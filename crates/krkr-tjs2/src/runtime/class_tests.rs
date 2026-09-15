//! krkrz's empty native `finalize` on the classes TJS2 itself registers.
//!
//! `TJS_DECL_EMPTY_FINALIZE_METHOD` (`tjsNative.h:380-383`) declares a native
//! method named `finalize` whose whole body is `return TJS_S_OK;`:
//!
//! | class | declaration |
//! | --- | --- |
//! | `Exception` | `tjsException.cpp:30` |
//! | `Math` | `tjsMath.cpp:108` |
//! | `RegExp` | `tjsRegExp.cpp:210` |
//! | `Date` | `tjsDate.cpp:51` |
//! | `RandomGenerator` | `tjsRandomGenerator.cpp:267` |
//!
//! The declaration carries no `TJS_STATICMEMBER`, so
//! `tTJSNativeClass::CreateNew` copies it onto every instance as well
//! (`tjsNative.cpp:340-364`).  A missing member is not a quiet void: the call
//! aborts with `Member "finalize" does not exist`, which is how PARQUET's
//! `ConductorException extends Exception` -- it calls
//! `global.Exception.finalize(...)` from its own finalize -- would fail.  The
//! engine half of this rule was closed by M146
//! (`crates/krkr-engine/src/native/classes.rs`); these five classes live here.

use crate::compiler::execute_source;
use crate::runtime::value::Variant;

fn run(source: &str) -> Variant {
    execute_source("finalize.tjs", source).expect("execute")
}

#[test]
fn every_tjs2_side_class_answers_an_empty_finalize() {
    assert_eq!(
        run(r#"
            return typeof Exception.finalize + ":" + typeof Math.finalize + ":" +
                typeof RegExp.finalize + ":" + typeof Date.finalize + ":" +
                typeof Math.RandomGenerator.finalize;
            "#,),
        Variant::String("Object:Object:Object:Object:Object".into())
    );
}

/// The declared method is a no-op answering void, and as a non-static member
/// it is on every instance of the class as well.
#[test]
fn the_empty_finalize_is_a_void_no_op_on_class_and_instance() {
    assert_eq!(
        run(r#"
            var results = [];
            results.add(typeof Math.finalize());
            results.add(typeof (new Date()).finalize);
            results.add(typeof (new RegExp()).finalize);
            results.add(typeof (new Exception("probe")).finalize);
            return results.join(":");
            "#,),
        Variant::String("void:Object:Object:Object".into())
    );
}

/// A script subclass's own `finalize` is the one that runs: the native
/// declaration sits where the class's other non-static members sit, and a
/// subclass member of the same name wins over it.
#[test]
fn a_script_subclass_keeps_its_own_finalize() {
    assert_eq!(
        run(r#"
            class ConductorException extends Exception {
                function ConductorException(message) {
                    super.Exception(message);
                }
                function finalize() {
                    return "script:" + this.message;
                }
            }
            var error = new ConductorException("boom");
            return error.finalize() + ":" + typeof global.Exception.finalize;
            "#,),
        Variant::String("script:boom:Object".into())
    );
}
