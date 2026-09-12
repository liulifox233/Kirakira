//! getabout.dll compatibility implementation (`docs/plugins/getabout.md`).
//!
//! The whole DLL is one binder call: it attaches the engine export
//! `TVPGetAboutString` to the `System` class as `System.getAboutString`
//! (`Kirikiroid2/src/plugins/getabout.cpp`,
//! `NCB_ATTACH_FUNCTION(getAboutString, System, TVPGetAboutString)`) and
//! registers nothing else — the same shape as `getLangName.dll`, and the
//! reason this module has no surface of its own: the member belongs to
//! `System`.
//!
//! Two bigger plugins attach the same member — `windowEx.dll`
//! (`window_ex.rs:500`) and the `systemEx` half of `PackinOne.dll`
//! (`packinone.rs:511`) — so a title that loads either of them already has
//! `System.getAboutString`, and the standalone DLL exists for titles that load
//! nothing else. Whichever module attaches first wins: [`install_get_about`]
//! registers only when the member is absent, so it never replaces another
//! module's body and the attachments cannot drift apart — every one of them
//! answers [`ABOUT_STRING`], with extra arguments tolerated the way the other
//! two registrations tolerate them.
//!
//! # The reference format
//!
//! `TVPGetAboutString()` (`krkrz/src/core/msg/MsgIntf.cpp:163-178`) formats the
//! engine's version resource and the TJS version into the about string and
//! appends the accumulated important log — the text the Windows version dialog
//! shows (`VersionFormUnit.cpp:41`). This engine has neither a version
//! resource nor that log, so the text is this engine's own identity instead of
//! the reference shape. Producing it needs an engine-side version/log helper
//! that all three attachments would call, so it stays a recorded gap rather
//! than a fourth spelling of the same string here.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "System.getAboutString",
    notes: "The DLL's whole surface is that one attachment (NCB_ATTACH_FUNCTION), so registering installs System.getAboutString only when no other module put it there: windowEx.dll and the systemEx half of PackinOne.dll attach the same member, and this module leaves whichever of them won in place, which keeps the three from drifting apart. Every attachment answers the same identity string; the reference renders its version resource plus the important log (MsgIntf.cpp:163-178), which this engine has neither of, so the text is not the reference format — an engine-side helper would be needed for all three at once. Extra arguments are tolerated, matching the other two registrations.",
    install: |engine| engine.register_plugin(GetAboutPlugin),
};

pub struct GetAboutPlugin;

impl KrkrPlugin for GetAboutPlugin {
    fn name(&self) -> &str {
        "getabout.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_get_about(runtime);
        Ok(())
    }
}

/// The text every attachment of this member answers — this engine's identity,
/// where the reference formats its version resource and important log. Kept
/// `pub` so the other two attachments (`window_ex.rs`, `packinone.rs`) can
/// read it from here instead of repeating the literal when they are next
/// touched; today all three spell the same string.
pub const ABOUT_STRING: &str = "Kirakira (Kirikiri-compatible emulator)";

/// `TVPGetAboutString` bound to `System` as `getAboutString`
/// (`MsgIntf.cpp:163-178`), registering only when the member is absent.
///
/// The other two attachments register theirs unconditionally, so whether a
/// given engine has this member already depends on which modules were
/// installed; attaching over one of them would make the answer depend on load
/// order. A rejected registration is not a failure — the reference DLL is just
/// as much of a no-op there.
fn install_get_about(runtime: &mut Runtime<KrkrHost>) {
    let Some(system) = runtime.global_member("System").object_handle() else {
        return;
    };
    if !matches!(
        runtime.object_member(system, "getAboutString"),
        Variant::Void
    ) {
        return;
    }
    runtime.register_object_native(system, "getAboutString", get_about_string);
}

fn get_about_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(ABOUT_STRING.to_string()))
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::{ABOUT_STRING, GetAboutPlugin};
    use crate::catalog::{PluginFamily, PluginStatus, resolve};

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(GetAboutPlugin).expect("plugin");
        engine
    }

    fn about_string(engine: &mut KrkrEngine) -> String {
        match engine
            .execute_script("probe.tjs", "return System.getAboutString();")
            .expect("probe script")
        {
            Variant::String(text) => text,
            other => panic!("System.getAboutString answered {other:?}"),
        }
    }

    #[test]
    fn the_catalog_entry_is_this_plugin_and_no_longer_missing() {
        let entry = resolve("getabout.dll").expect("catalog entry");
        assert_eq!(entry.name, "getabout.dll");
        assert_eq!(entry.family, PluginFamily::System);
        assert!(!entry.parquet, "PARQUET does not ship getabout.dll");
        assert_eq!(super::META.status, PluginStatus::Shim);
        assert!(!entry.is_placeholder());
    }

    #[test]
    fn registering_links_the_name_and_installs_the_member() {
        let mut engine = engine();
        assert!(
            engine
                .host()
                .linked_plugins()
                .any(|name| name == "getabout.dll"),
            "the module did not register its name"
        );
        assert!(
            !engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("not implemented: getabout.dll")),
            "an implemented module must not report itself as missing"
        );
        assert_eq!(about_string(&mut engine), ABOUT_STRING);

        let runtime = engine.tjs_runtime();
        let system = runtime
            .global_member("System")
            .object_handle()
            .expect("System object");
        assert!(
            matches!(
                runtime.object_member(system, "getAboutString"),
                Variant::Object(_)
            ),
            "System.getAboutString is not a native member: {:?}",
            runtime.object_member(system, "getAboutString")
        );
    }

    /// `Plugins.link` installs the module through the host registry: with only
    /// this plugin registered, the member must exist after the link, exactly
    /// as it does after `V2Link`.
    #[test]
    fn linking_getabout_through_plugins_reaches_the_member() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(GetAboutPlugin).expect("plugin");
        let value = engine
            .execute_script(
                "probe.tjs",
                r#"
                Plugins.link("getabout.dll");
                return System.getAboutString();
                "#,
            )
            .expect("probe script");
        assert_eq!(value, Variant::String(ABOUT_STRING.to_string()));
    }

    /// `windowEx.dll` attaches the same member and is installed first in
    /// catalog order; the module must leave that attachment alone instead of
    /// answering a second body.
    #[test]
    fn another_modules_attachment_is_not_replaced() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(crate::window_ex::WindowExPlugin)
            .expect("windowEx");
        let system = engine
            .tjs_runtime()
            .global_member("System")
            .object_handle()
            .expect("System object");
        let attached = engine
            .tjs_runtime()
            .object_member(system, "getAboutString")
            .object_handle();
        assert!(attached.is_some(), "windowEx did not attach the member");

        engine.register_plugin(GetAboutPlugin).expect("getabout");
        assert_eq!(
            engine
                .tjs_runtime()
                .object_member(system, "getAboutString")
                .object_handle(),
            attached,
            "getabout.dll replaced windowEx.dll's attachment"
        );
        assert_eq!(about_string(&mut engine), ABOUT_STRING);
    }

    /// The three attachments of `System.getAboutString` must answer the same
    /// text, whichever one a profile installed.
    #[test]
    fn the_attachments_answer_the_same_text() {
        let mut with_getabout = engine();
        let mut with_window_ex = KrkrEngine::new(EngineConfig::default()).expect("engine");
        with_window_ex
            .register_plugin(crate::window_ex::WindowExPlugin)
            .expect("windowEx");
        let mut with_packinone = KrkrEngine::new(EngineConfig::default()).expect("engine");
        with_packinone
            .register_plugin(crate::packinone::PackinOnePlugin)
            .expect("PackinOne");

        for engine in [&mut with_getabout, &mut with_window_ex, &mut with_packinone] {
            assert_eq!(about_string(engine), ABOUT_STRING);
        }
    }

    /// The reference's binder function takes no arguments and the other two
    /// attachments register it as `NativeArgCount::Any`, so a call that passes
    /// some must still answer instead of failing on the count.
    #[test]
    fn extra_arguments_are_tolerated() {
        let mut engine = engine();
        let value = engine
            .execute_script("probe.tjs", "return System.getAboutString(1, 2);")
            .expect("probe script");
        assert_eq!(value, Variant::String(ABOUT_STRING.to_string()));
    }
}
