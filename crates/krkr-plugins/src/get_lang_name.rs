//! getLangName.dll compatibility implementation (`docs/plugins/getLangName.md`).
//!
//! The whole DLL is one binder function: it hangs two stateless functions off
//! the `System` object (M24's guess `System.getLangName` was wrong — the real
//! names are these two, and nothing else is registered):
//!
//! - `System.getCurrentUILangName()` — the UI language in the ISO-639 form the
//!   Windows API reports for it (`GetUserDefaultUILanguage` plus
//!   `LOCALE_SISO639LANGNAME`): `"ja"`, `"en"`.
//! - `System.getCurrentLocaleName()` — the locale name
//!   (`GetLocaleInfo(LOCALE_SNAME)`): `"ja-JP"`, `"en-US"`.
//!
//! The values are resolved from the host locale environment — `LC_ALL`, then
//! `LC_MESSAGES`, then `LANG`, the POSIX precedence order — because a plugin
//! module has no OS locale API at hand (the dossier's macOS `NSLocale` lookup
//! needs platform bindings this crate does not carry). Resolution is
//! deterministic: one process environment always yields the same two names,
//! and a process with no locale environment falls back to `en-US` / `en`, what
//! a Windows machine with default settings reports. Which `LOCALE_*` constant
//! the reference passed is an open question in the dossier; the shim follows
//! its guidance and answers with the ISO-639 code for the UI language and the
//! full locale name for the locale.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "System.getCurrentUILangName / System.getCurrentLocaleName",
    notes: "Real host-locale lookup in the reference formats: the ISO-639 UI language code (\"ja\") and the BCP-47-style locale name (\"ja-JP\"), resolved from LC_ALL/LC_MESSAGES/LANG in POSIX precedence order, with en-US/en as the deterministic fallback when the process carries no locale environment (this crate has no OS locale API; the dossier's NSLocale probe is not available). Which LOCALE_* constant the reference passed is unrecovered.",
    install: |engine| engine.register_plugin(GetLangNamePlugin),
};

pub struct GetLangNamePlugin;

impl KrkrPlugin for GetLangNamePlugin {
    fn name(&self) -> &str {
        "getLangName.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_get_lang_name(runtime);
        Ok(())
    }
}

/// What the reference reports when no locale is configured (`C`/`POSIX`, an
/// unset environment), matching a Windows machine on its `en-US` defaults.
const FALLBACK_LOCALE_NAME: &str = "en-US";
const FALLBACK_UI_LANG_NAME: &str = "en";

fn install_get_lang_name(runtime: &mut Runtime<KrkrHost>) {
    let Some(system) = runtime.global_member("System").object_handle() else {
        return;
    };
    register_unless_script(
        runtime,
        system,
        "getCurrentUILangName",
        current_ui_lang_name,
    );
    register_unless_script(runtime, system, "getCurrentLocaleName", current_locale_name);
}

fn register_unless_script(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &'static str,
    function: impl krkr_tjs2::runtime::NativeFunction<KrkrHost> + 'static,
) {
    if matches!(runtime.object_member(object, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native(object, name, function);
}

/// The raw host locale spelling, if the process carries one: the first of
/// `LC_ALL`, `LC_MESSAGES`, `LANG` that is set and non-empty.
fn host_locale() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

/// `(locale name, UI language name)` for one raw host locale spelling, in the
/// reference's formats; `None` (or an unusable spelling) yields the
/// deterministic fallback pair.
fn reference_names(raw: Option<&str>) -> (String, String) {
    match raw.and_then(parse_posix_locale) {
        Some((language, subtag)) => {
            let locale = match subtag {
                Some(subtag) => format!("{language}-{subtag}"),
                None => language.clone(),
            };
            (locale, language)
        }
        None => (
            FALLBACK_LOCALE_NAME.to_string(),
            FALLBACK_UI_LANG_NAME.to_string(),
        ),
    }
}

/// Splits a POSIX locale spelling (`ja_JP.UTF-8`, `de_DE@euro`, `zh_Hans`) into
/// a lowercase language subtag and an optional region/script subtag, cased the
/// way BCP-47 spells it (`JP`, `419`, `Hans`). `None` for spellings that name
/// no language at all (`C`, `POSIX`, empty, anything non-alphabetic).
fn parse_posix_locale(raw: &str) -> Option<(String, Option<String>)> {
    let base = raw.split('@').next()?.split('.').next()?.trim();
    if base.is_empty() || base.eq_ignore_ascii_case("C") || base.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let mut parts = base.split('_');
    let language = parts.next()?.trim();
    if !(2..=3).contains(&language.len())
        || !language
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        return None;
    }
    let language = language.to_ascii_lowercase();
    let subtag = parts.next().map(str::trim).and_then(|subtag| {
        let alphabetic = subtag
            .chars()
            .all(|character| character.is_ascii_alphabetic());
        let digits = subtag.chars().all(|character| character.is_ascii_digit());
        match subtag.len() {
            2 if alphabetic => Some(subtag.to_ascii_uppercase()),
            3 if alphabetic => Some(subtag.to_ascii_lowercase()),
            3 if digits => Some(subtag.to_string()),
            4 if alphabetic => Some(format!(
                "{}{}",
                subtag[..1].to_ascii_uppercase(),
                subtag[1..].to_ascii_lowercase()
            )),
            _ => None,
        }
    });
    Some((language, subtag))
}

/// `System.getCurrentUILangName()` — the ISO-639 language code of the host's
/// UI language.
fn current_ui_lang_name(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(reference_names(host_locale().as_deref()).1))
}

/// `System.getCurrentLocaleName()` — the host locale name, language plus
/// region/script subtag when the host names one.
fn current_locale_name(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(reference_names(host_locale().as_deref()).0))
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::{GetLangNamePlugin, host_locale, reference_names};

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(GetLangNamePlugin).expect("plugin");
        engine
    }

    #[test]
    fn posix_locale_spellings_map_to_reference_names() {
        for (raw, locale, ui) in [
            ("ja_JP.UTF-8", "ja-JP", "ja"),
            ("en_US", "en-US", "en"),
            ("de_DE@euro", "de-DE", "de"),
            ("pt_BR.ISO-8859-1", "pt-BR", "pt"),
            ("zh_Hans", "zh-Hans", "zh"),
            ("es_419", "es-419", "es"),
            ("fil_PH", "fil-PH", "fil"),
            ("ja", "ja", "ja"),
        ] {
            assert_eq!(
                reference_names(Some(raw)),
                (locale.to_string(), ui.to_string()),
                "{raw} does not map to {locale}/{ui}"
            );
        }
        for raw in [
            None,
            Some(""),
            Some("C"),
            Some("POSIX"),
            Some("garbage!"),
            Some("_JP"),
        ] {
            assert_eq!(
                reference_names(raw),
                ("en-US".to_string(), "en".to_string()),
                "{raw:?} does not fall back to the en-US/en pair"
            );
        }
    }

    #[test]
    fn only_the_two_recovered_members_are_registered_on_system() {
        let engine = engine();
        let runtime = engine.tjs_runtime();
        let system = runtime
            .global_member("System")
            .object_handle()
            .expect("System");

        for name in ["getCurrentUILangName", "getCurrentLocaleName"] {
            assert!(
                !matches!(runtime.object_member(system, name), Variant::Void),
                "System.{name} is missing"
            );
        }
        // M24's guess was wrong: the reference has no `System.getLangName`.
        assert!(matches!(
            runtime.object_member(system, "getLangName"),
            Variant::Void
        ));
    }

    #[test]
    fn the_values_follow_the_host_locale_and_repeat_across_calls() {
        let (locale, ui) = reference_names(host_locale().as_deref());
        let mut engine = engine();
        let value = engine
            .execute_script(
                "probe.tjs",
                r#"
                var locale = System.getCurrentLocaleName();
                var ui = System.getCurrentUILangName();
                return locale + "|" + ui + "|" + System.getCurrentLocaleName() + "|" +
                    System.getCurrentUILangName();
                "#,
            )
            .expect("probe locale names");
        assert_eq!(
            value,
            Variant::String(format!("{locale}|{ui}|{locale}|{ui}"))
        );
    }

    /// The reference's callbacks are parameterless SimpleBinder functions and
    /// nothing was recovered about arity rejection, so the shim tolerates
    /// extra arguments instead of raising where the reference may not.
    #[test]
    fn extra_arguments_are_tolerated() {
        let mut engine = engine();
        let value = engine
            .execute_script(
                "probe.tjs",
                r#"
                return (System.getCurrentLocaleName(1) == System.getCurrentLocaleName()) + "|" +
                    (System.getCurrentUILangName("x") == System.getCurrentUILangName());
                "#,
            )
            .expect("probe with extra arguments");
        assert_eq!(value, Variant::String("1|1".to_string()));
    }
}
