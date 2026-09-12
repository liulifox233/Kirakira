//! krkrsteam.dll compatibility shim (`docs/plugins/krkrsteam.md`).
//!
//! The shipped DLL is an ncbind wrapper around `steam_api.dll`: one global
//! `Steam` object whose factory always throws — `new Steam()` fails with the
//! reference's "Cannot create instance" — plus a `steam` storage media that
//! maps `steam://…` names onto Steam Remote Storage (`Main.cpp:64-67`,
//! `Storages.cpp`). The surface below is the union of the DLL's member strings
//! (the dossier's table) and the trunk source the build came from
//! (`krkr2/kirikiri2/trunk/kirikiri2/src/plugins/win32/steam/Main.cpp:571-595`).
//!
//! Off Steam there is no Steamworks backend, and the dossier forbids loading
//! the SDK (a missing `steam_api.dll` must not break startup), so every member
//! is a mapped/no-op surface that never throws: `false` from the achievement,
//! cloud-file and hook calls, `0` from the properties, dictionaries that carry
//! every documented field with a defaulted value, no-op screenshot calls.
//! Where the trunk would answer `void` because its Steam interfaces are
//! absent (`Main.cpp:114-118` for an uninitialized achievement, `:169-187`
//! for a missing storage), the shim still answers the documented record with
//! defaulted fields: the dossier requires those values to stay readable
//! instead of turning a field read into a miss.
//! `getLanguage()` is the one member the dossier asks to answer plausibly —
//! many games branch on it for text — so it derives the Steam language name
//! from the host locale (`LC_ALL`/`LC_MESSAGES`/`LANG`, `english` when the
//! spelling names no language), the value `ISteamApps::GetCurrentGameLanguage`
//! would report for an English default.
//!
//! The `steam` storage media is *not* installed: the reference registers it
//! with `TVPRegisterStorageMedia`, but Kirakira's media registry
//! (`krkr-assets`) exposes no plugin-facing registration path, so a
//! `steam://…` name keeps missing through the built-in resolver. That needs an
//! engine change (the same gap the `proxyfs`/`lzfs` storage plugins hit), not
//! a different plugin body.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Steam global object (achievements, cloud, screenshot and broadcast members)",
    notes: "Compat surface off Steam (no Steamworks backend and no steam_api dependency, per the \
            dossier): every member is present and never throws. Real value: getLanguage() maps the \
            host locale (LC_ALL/LC_MESSAGES/LANG) to the Steam language name (\"ja_JP.UTF-8\" -> \
            \"japanese\"), falling back to \"english\". Mapped: requestInitialize, setAchievement, \
            clearAchievement, deleteCloudFile, copyCloudFile, hookScreenshots and hookBroadcasting \
            answer false; initialized, cloudEnabled, achievementsCount, cloudFileCount and \
            isBroadcasting answer 0; getAchievement/getCloudQuota/getCloudFileInfo answer \
            dictionaries with every documented field defaulted; triggerScreenshot and \
            writeScreenshot are callable no-ops; cloudEnabled writes are accepted and ignored \
            (there is no Steam settings store to flip, so the getter keeps reporting 0). new \
            Steam() fails with the reference's \"Cannot create instance\". The `steam` storage \
            media is NOT installed: krkr-assets has the media registry but no plugin-facing \
            registration hook, so steam:// names keep missing through the built-in resolver.",
    install: |engine| engine.register_plugin(KrkrSteamPlugin),
};

pub struct KrkrSteamPlugin;

impl KrkrPlugin for KrkrSteamPlugin {
    fn name(&self) -> &str {
        "krkrsteam.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_steam(runtime);
        Ok(())
    }
}

/// What the reference factory throws (`Main.cpp:64-67`: the message map's
/// `TVPCannotCreateInstance`), so `new Steam()` fails the way scripts written
/// against the DLL expect instead of constructing an empty object.
const CANNOT_CREATE_INSTANCE: &str = "Cannot create instance";

/// What every member's handler reads like; the reference's members are all
/// bound this way (`ncbInstanceAdaptor` methods).
type MethodHandler =
    fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>;

/// Every member with the minimum argument count its recovered signature
/// requires (`Main.cpp:571-595`, the dossier's table) and the shim's answer:
/// a call with fewer arguments fails with `TJS_E_BADPARAMCOUNT` before the
/// handler, extra arguments are tolerated.
const METHODS: &[(&str, usize, MethodHandler)] = &[
    ("getLanguage", 0, steam_language),
    ("requestInitialize", 0, native_false),
    ("getAchievement", 1, achievement_info),
    ("setAchievement", 1, native_false),
    ("clearAchievement", 1, native_false),
    ("getCloudQuota", 0, cloud_quota),
    ("getCloudFileInfo", 1, cloud_file_info),
    ("deleteCloudFile", 1, native_false),
    ("copyCloudFile", 2, native_false),
    ("triggerScreenshot", 0, native_void),
    ("hookScreenshots", 1, native_false),
    ("writeScreenshot", 2, native_void),
    ("isBroadcasting", 0, native_false),
    ("hookBroadcasting", 1, native_false),
];

/// `NCB_PROPERTY_RO` members: a script write is denied, the read reports 0
/// with no Steam (achievements never initialize, no cloud file exists).
const READ_ONLY_PROPERTIES: &[&str] = &["initialized", "achievementsCount", "cloudFileCount"];

/// The one `NCB_PROPERTY` pair. There is no Steam settings store here, so the
/// setter accepts the write and changes nothing and the getter keeps reporting
/// 0 — the reference's own answer when `SteamRemoteStorage()` is absent.
const READ_WRITE_PROPERTIES: &[&str] = &["cloudEnabled"];

fn install_steam(runtime: &mut Runtime<KrkrHost>) {
    // A `Steam` the scripts already installed wins; never shadow it.
    if runtime.global_member("Steam").object_handle().is_some() {
        return;
    }
    // The reference is a class object whose factory always throws; a native
    // constructor reproduces `new Steam()` failing while the members stay
    // reachable on the global binding, which is what scripts call.
    let class = runtime.alloc_native_constructor(
        |_runtime: &mut Runtime<KrkrHost>, _this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            Err(TjsError::runtime(CANNOT_CREATE_INSTANCE))
        },
    );
    runtime.add_object_class_info(class, "Steam");
    install_steam_members(runtime, class);
    runtime.set_global_member("Steam", Variant::Object(class));
}

fn install_steam_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    for (name, min_args, handler) in METHODS {
        runtime.register_object_native_with_arg_count(
            handle,
            *name,
            NativeArgCount::AtLeast(*min_args),
            *handler,
        );
    }
    for name in READ_ONLY_PROPERTIES {
        runtime.register_object_native_property_with_access(
            handle,
            *name,
            NativePropertyAccess::ReadOnly,
            property_zero,
            property_ignored,
        );
    }
    for name in READ_WRITE_PROPERTIES {
        runtime.register_object_native_property_with_access(
            handle,
            *name,
            NativePropertyAccess::ReadWrite,
            property_zero,
            property_ignored,
        );
    }
}

/// `bool` members with no Steam to talk to: the reference's own answer with
/// its interfaces absent (`SteamAchievements.cpp:23-36`, `Main.cpp:154-253`,
/// `:272-305`).
fn native_false(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

/// `void` members whose work is entirely on Steam's side.
fn native_void(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

/// Every property's value with no Steam: the achievements never initialize,
/// no cloud file exists, cloud access is off, the app is not broadcasting.
fn property_zero(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

/// The `cloudEnabled` setter. `SetCloudEnabledForApp` writes a Steam user
/// setting this process has no store for, so the write is accepted and
/// discarded rather than mirrored into a getter that would then claim cloud
/// access the engine cannot provide.
fn property_ignored(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _value: Variant,
) -> Result<()> {
    Ok(())
}

/// `getAchievement(n)` — one achievement record with every field the DLL's
/// strings and the manual document (`docs/plugins/krkrsteam.md`;
/// `SteamAchievements.cpp:59-69` also carries the identifier as `ach`), all
/// defaulted: nothing is achieved, nothing is unlocked.
fn achievement_info(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let record = runtime.alloc_dictionary_object();
    for (name, value) in [
        ("ach", Variant::String(String::new())),
        ("name", Variant::String(String::new())),
        ("desc", Variant::String(String::new())),
        ("achieved", Variant::Integer(0)),
        ("unlockTime", Variant::Integer(0)),
    ] {
        runtime.set_object_member(record, name, value);
    }
    Ok(Variant::Object(record))
}

/// `getCloudQuota()` — the documented `total`/`available` pair, zero bytes of
/// each (there is no Steam cloud to measure).
fn cloud_quota(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let record = runtime.alloc_dictionary_object();
    for (name, value) in [
        ("total", Variant::Integer(0)),
        ("available", Variant::Integer(0)),
    ] {
        runtime.set_object_member(record, name, value);
    }
    Ok(Variant::Object(record))
}

/// `getCloudFileInfo(n)` — the documented file record with an empty name, a
/// zero size and timestamp. `desc` follows the dossier's tentative field list
/// (the string is recovered only as part of the DLL's shared string pool).
fn cloud_file_info(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let record = runtime.alloc_dictionary_object();
    for (name, value) in [
        ("filename", Variant::String(String::new())),
        ("size", Variant::Integer(0)),
        ("time", Variant::Integer(0)),
        ("desc", Variant::String(String::new())),
    ] {
        runtime.set_object_member(record, name, value);
    }
    Ok(Variant::Object(record))
}

/// `getLanguage()` — `ISteamApps::GetCurrentGameLanguage` reports the language
/// the game runs in. With no Steam the shim derives it from the host locale,
/// the closest available source, and falls back to `english`, which is what
/// Steamworks itself reports for an unresolved language.
fn steam_language(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(steam_language_name(
        host_locale().as_deref(),
    )))
}

/// The Steam language name for one raw host locale spelling (the
/// `ISteamApps::GetCurrentGameLanguage` vocabulary, e.g. `"japanese"`,
/// `"schinese"`, `"brazilian"`); `english` for spellings that name no language
/// this shim maps.
fn steam_language_name(raw: Option<&str>) -> String {
    let Some((language, region)) = raw.and_then(split_locale) else {
        return DEFAULT_LANGUAGE.to_string();
    };
    let language = match language.as_str() {
        "en" => "english",
        "ja" => "japanese",
        "ko" => "koreana",
        "de" => "german",
        "fr" => "french",
        "it" => "italian",
        "es" => "spanish",
        "ru" => "russian",
        "th" => "thai",
        "pl" => "polish",
        "da" => "danish",
        "nl" => "dutch",
        "fi" => "finnish",
        "no" | "nb" | "nn" => "norwegian",
        "sv" => "swedish",
        "hu" => "hungarian",
        "cs" => "czech",
        "ro" => "romanian",
        "tr" => "turkish",
        "bg" => "bulgarian",
        "el" => "greek",
        "uk" => "ukrainian",
        "vi" => "vietnamese",
        "zh" => {
            if is_traditional(region.as_deref()) {
                "tchinese"
            } else {
                "schinese"
            }
        }
        "pt" => {
            if region.as_deref() == Some("BR") {
                "brazilian"
            } else {
                "portuguese"
            }
        }
        _ => DEFAULT_LANGUAGE,
    };
    language.to_string()
}

/// What a Windows machine on its defaults reports to Steamworks.
const DEFAULT_LANGUAGE: &str = "english";

/// The raw host locale spelling, if the process carries one: the first of
/// `LC_ALL`, `LC_MESSAGES`, `LANG` that is set and non-empty, the POSIX
/// precedence order (`getLangName` resolves the same way).
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

/// Splits a POSIX locale spelling (`ja_JP.UTF-8`, `de_DE@euro`) into a
/// lowercase language subtag and an uppercased region/script subtag. `None`
/// for spellings that name no language at all (`C`, `POSIX`, empty, anything
/// non-alphabetic).
fn split_locale(raw: &str) -> Option<(String, Option<String>)> {
    let base = raw.split('@').next()?.split('.').next()?.trim();
    if base.is_empty() || base.eq_ignore_ascii_case("C") || base.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let mut parts = base.split('_');
    let language = parts.next().unwrap_or("").trim();
    if !(2..=3).contains(&language.len())
        || !language
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        return None;
    }
    let region = parts
        .next()
        .map(str::trim)
        .filter(|region| !region.is_empty())
        .map(str::to_ascii_uppercase);
    Some((language.to_ascii_lowercase(), region))
}

/// `zh` spells Traditional Chinese with a region (`TW`, `HK`, `MO`) or the
/// `Hant` script subtag (both uppercased by [`split_locale`]); everything else
/// reads as Simplified, which is Steam's own default for the language.
fn is_traditional(region: Option<&str>) -> bool {
    matches!(region, Some("TW" | "HK" | "MO" | "HANT"))
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::{TjsErrorKind, runtime::Variant};

    use super::{KrkrSteamPlugin, host_locale, steam_language_name};

    /// The dossier's member table (`docs/plugins/krkrsteam.md`), spelled out
    /// here independently of the install constants so that a dropped or
    /// renamed member fails this test.
    const MEMBERS: &[&str] = &[
        "getLanguage",
        "requestInitialize",
        "initialized",
        "achievementsCount",
        "getAchievement",
        "setAchievement",
        "clearAchievement",
        "cloudEnabled",
        "getCloudQuota",
        "cloudFileCount",
        "getCloudFileInfo",
        "deleteCloudFile",
        "copyCloudFile",
        "triggerScreenshot",
        "hookScreenshots",
        "writeScreenshot",
        "isBroadcasting",
        "hookBroadcasting",
    ];

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(KrkrSteamPlugin).expect("plugin");
        engine
    }

    fn run(engine: &mut KrkrEngine, script: &str) -> Variant {
        engine
            .execute_script("probe.tjs", script)
            .expect("probe script")
    }

    #[test]
    fn the_global_object_carries_the_dossier_member_list() {
        let engine = engine();
        let runtime = engine.tjs_runtime();
        let steam = runtime
            .global_member("Steam")
            .object_handle()
            .expect("Steam global");
        for name in MEMBERS {
            assert!(
                !matches!(runtime.object_member(steam, name), Variant::Void),
                "Steam.{name} is missing"
            );
        }
    }

    #[test]
    fn the_reference_refuses_construction() {
        let mut engine = engine();
        let error = engine
            .execute_script("probe.tjs", "var steam = new Steam();")
            .expect_err("the reference factory always throws");
        assert_eq!(error.kind, TjsErrorKind::Runtime, "{}", error.message);
        assert!(
            error.message.contains("Cannot create instance"),
            "{}",
            error.message
        );
    }

    #[test]
    fn achievement_members_answer_the_no_steam_defaults() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var info = Steam.getAchievement(0);
            return (Steam.initialized == 0) + "|" + (Steam.achievementsCount == 0) + "|" +
                (Steam.requestInitialize() == 0) + "|" + (Steam.setAchievement("ACH") == 0) + "|" +
                (Steam.clearAchievement(0) == 0) + "|" + (info.ach == "") + "|" +
                (info.name == "") + "|" + (info.desc == "") + "|" + (info.achieved == 0) + "|" +
                (info.unlockTime == 0);
            "#,
        );
        assert_eq!(value, Variant::String("1|1|1|1|1|1|1|1|1|1".to_string()));
    }

    #[test]
    fn cloud_members_answer_empty_quota_and_file_info() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var quota = Steam.getCloudQuota();
            Steam.cloudEnabled = 1;
            var info = Steam.getCloudFileInfo(0);
            return typeof quota + "|" + (quota.total == 0) + "|" + (quota.available == 0) + "|" +
                (Steam.cloudEnabled == 0) + "|" + (Steam.cloudFileCount == 0) + "|" +
                (Steam.deleteCloudFile("save.dat") == 0) + "|" +
                (Steam.copyCloudFile("save.dat", "backup.dat") == 0) + "|" +
                (info.filename == "") + "|" + (info.size == 0) + "|" + (info.time == 0) + "|" +
                (info.desc == "");
            "#,
        );
        assert_eq!(
            value,
            Variant::String("Object|1|1|1|1|1|1|1|1|1|1".to_string())
        );
    }

    /// The dossier's requirement is that the record members hand back a real
    /// dictionary a script can read members off (TJS2 has no `for…in`, so the
    /// dossier's example loop is the wrong shape; the readable fields are the
    /// guarantee that matters).
    #[test]
    fn the_info_members_hand_back_dictionary_instances() {
        for call in [
            "Steam.getAchievement(0)",
            "Steam.getCloudQuota()",
            "Steam.getCloudFileInfo(0)",
        ] {
            let mut engine = engine();
            let record = run(&mut engine, &format!("return {call};"))
                .object_handle()
                .unwrap_or_else(|| panic!("{call} did not hand back an object"));
            assert!(
                engine.tjs_runtime().is_dictionary_instance(record),
                "{call} did not hand back a dictionary instance"
            );
        }
    }

    #[test]
    fn screenshot_and_broadcast_members_are_inert() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            var triggered = Steam.triggerScreenshot();
            var written = Steam.writeScreenshot(void, "");
            var hookedScreenshots = Steam.hookScreenshots(void);
            var hookedBroadcast = Steam.hookBroadcasting(function(state) {});
            return typeof triggered + "|" + typeof written + "|" + hookedScreenshots + "|" +
                hookedBroadcast + "|" + Steam.isBroadcasting();
            "#,
        );
        assert_eq!(value, Variant::String("void|void|0|0|0".to_string()));
    }

    #[test]
    fn the_read_only_properties_reject_script_writes() {
        for property in ["initialized", "achievementsCount", "cloudFileCount"] {
            let mut engine = engine();
            let error = engine
                .execute_script("probe.tjs", &format!("Steam.{property} = 1;"))
                .expect_err("a read-only property denies the write");
            assert_eq!(
                error.kind,
                TjsErrorKind::AccessDenied,
                "{property}: {}",
                error.message
            );
        }
    }

    #[test]
    fn methods_reject_calls_missing_a_signature_argument() {
        for call in [
            "Steam.getAchievement()",
            "Steam.setAchievement()",
            "Steam.clearAchievement()",
            "Steam.getCloudFileInfo()",
            "Steam.deleteCloudFile()",
            "Steam.copyCloudFile(\"a\")",
            "Steam.hookScreenshots()",
            "Steam.writeScreenshot(void)",
            "Steam.hookBroadcasting()",
        ] {
            let mut engine = engine();
            let error = engine
                .execute_script("probe.tjs", &format!("return {call};"))
                .expect_err("a missing argument must be rejected");
            assert_eq!(error.kind, TjsErrorKind::BadParamCount, "{call}");
        }
    }

    /// The recovered signatures are minimum counts: extra arguments are
    /// tolerated (the shim has nothing to send them to) and the zero-argument
    /// members accept any.
    #[test]
    fn extra_arguments_are_tolerated() {
        let mut engine = engine();
        let value = run(
            &mut engine,
            r#"
            return Steam.copyCloudFile("a", "b", 1) + "|" + Steam.setAchievement("ACH", 2) + "|" +
                (Steam.getLanguage(3) == Steam.getLanguage()) + "|" + Steam.hookScreenshots(0, 1);
            "#,
        );
        assert_eq!(value, Variant::String("0|0|1|0".to_string()));
    }

    #[test]
    fn the_language_follows_the_host_locale_and_repeats() {
        let mut engine = engine();
        let expected = steam_language_name(host_locale().as_deref());
        assert_eq!(
            run(&mut engine, "return Steam.getLanguage();"),
            Variant::String(expected.clone())
        );
        let value = run(
            &mut engine,
            r#"return Steam.getLanguage() + "|" + Steam.getLanguage();"#,
        );
        assert_eq!(value, Variant::String(format!("{expected}|{expected}")));
    }

    #[test]
    fn locale_spellings_map_to_steam_language_names() {
        for (raw, steam) in [
            (Some("ja_JP.UTF-8"), "japanese"),
            (Some("en_US"), "english"),
            (Some("ko_KR"), "koreana"),
            (Some("zh_CN"), "schinese"),
            (Some("zh_TW"), "tchinese"),
            (Some("zh_Hant"), "tchinese"),
            (Some("zh_Hans"), "schinese"),
            (Some("pt_BR"), "brazilian"),
            (Some("pt_PT"), "portuguese"),
            (Some("de_DE@euro"), "german"),
            (Some("fr_FR.ISO-8859-1"), "french"),
            (Some("es_419"), "spanish"),
            (Some("ru_RU"), "russian"),
            (Some("vi_VN"), "vietnamese"),
        ] {
            assert_eq!(steam_language_name(raw), steam.to_string(), "{raw:?}");
        }
        for raw in [
            None,
            Some(""),
            Some("C"),
            Some("POSIX"),
            Some("garbage!"),
            Some("_JP"),
            Some("x"),
        ] {
            assert_eq!(steam_language_name(raw), "english".to_string(), "{raw:?}");
        }
    }
}
