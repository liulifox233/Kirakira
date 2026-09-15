//! `xp3filter.dll` — the XP3 extraction/content filter hook.
//!
//! # What the reference does
//!
//! The plugin exists as two different things in the reference trees:
//!
//! * krkr2/krkrz ship the **sample pair** `xp3filter/xp3dec/` + `xp3filter/xp3enc/`
//!   and nothing else. `xp3dec` is a `.tpm` plugin with no TJS surface at all:
//!   `V2Link` calls `TVPSetXP3ArchiveExtractionFilter(TVPXP3ArchiveExtractionFilter)`
//!   (`xp3dec/main.cpp:98-110`) and `V2Unlink` clears it with `NULL`
//!   (`:112-124`); the callback receives one `tTVPXP3ExtractionFilterInfo`
//!   (`Offset`, `Buffer`, `BufferSize`, `FileHash`, `:42-85`; the struct is
//!   `krkr2/src/core/base/XP3Archive.h:25-38`) and XORs every byte with the
//!   file hash. The `.tpm` extension matters: the engine loads `.tpm` plugins
//!   *before* `startup.tjs`, i.e. before the first XP3 file is opened
//!   (`xp3dec/main.cpp:2-19`), which is what makes a global filter usable.
//! * Kirikiroid2 is the only tree with the **real mechanism**:
//!   `xp3filter.cpp` 460 + `xp3filter.h` 104. It installs no surface in the
//!   main engine either. On post-registration (`:438-458`) it looks for
//!   `TVPGetAppPath() + "xp3filter.tjs"`, checks it with
//!   `TVPIsExistentStorageNoSearch` (no auto-path search), reads it, and only
//!   then sets *both* core filters to its wrappers; **when the file is absent
//!   it leaves both filters alone** — the whole decision path is that one
//!   existence check. The loaded script is executed in a private per-thread
//!   TJS engine (`XP3FilterDecoder`, `:212-220, 295-354`) in which the plugin
//!   adds `Storages.setXP3ArchiveExtractionFilter(fn)` and
//!   `Storages.setXP3ArchiveContentFilter(fn)` (`:306-316`), so
//!   `xp3filter.tjs` chooses the decryption callbacks. Its public API is
//!   `TVPSetXP3FilterScript(content)` (`:421-436`): empty content unsets both
//!   filters; different content rebuilds the private engines.
//!   * Extraction wrapper (`:385-419`): called per decompressed chunk with
//!     `(FileHash, Offset, CBinaryAccessor wrapping Buffer, BufferSize,
//!     FileName, ctx)`; the script mutates the buffer in place through the
//!     accessor (`count`, `ptr`, `[i]`, `+=`/`-=`, `xor`, `xp3filter.h:4-104`).
//!   * Content wrapper (`:362-383`): called with `(filepath, archivename,
//!     filesize)` before an entry is read; the script returns `[int, ctx]`,
//!     and `1` makes the core fetch the whole file into memory at open
//!     (`XP3Archive.cpp:585-595`).
//!
//! The engine side of the hook is the same in both trees:
//! `TVPXP3ArchiveExtractionFilter` is one global function pointer
//! (`XP3Archive.cpp:30-33`) invoked from the archive read path for every
//! chunk — `XP3Archive.cpp:1005-1010` in krkr2, `:1047-1052` in K2.
//!
//! # The seam this plugin installs into
//!
//! The engine now has one. `krkr-core` carries the vocabulary
//! (`Xp3ExtractionFilter`, `Xp3ContentFilter`/`Xp3ContentFilterAction`,
//! `Xp3ExtractionFilterInfo`, `Xp3FilterContext` and the `Xp3FilterRegistry`
//! holding both slots), `krkr-xp3`'s archives hold that registry and read its
//! slots *late* — the content filter when an entry stream is created
//! (`crates/krkr-xp3/src/stream.rs:78-113`, K2 `XP3Archive.cpp:585-595`) and
//! the extraction filter on every read chunk (`:284-293`, K2 `:1047-1053`) —
//! and `krkr-engine` exposes the install to plugins as
//! [`crate::plugin_api::xp3`] (`set_extraction_filter`/`set_content_filter`),
//! handed out by `ProjectStoragePort::xp3_filter_registry`. Because the archive
//! never captures a filter, an install from a plugin's `register` — which
//! always runs after the project storage was built — reaches every later read,
//! which is the reference's own ordering (`xp3filter.cpp:438-458` installs
//! post-registration too). The engine end-to-end pin lives in
//! `crates/krkr-engine/src/plugin_api/xp3.rs`
//! (`a_plugin_installed_filter_changes_what_the_engine_reads`).
//!
//! # What is still missing here: the script bridge
//!
//! What this module cannot do yet is turn `xp3filter.tjs` into callbacks. The
//! reference executes the configuration in a **private per-thread TJS engine**
//! (`XP3FilterDecoder`, `K2 xp3filter.cpp:212-220`, `:295-354`) whose
//! `Storages.setXP3ArchiveExtractionFilter`/`setXP3ArchiveContentFilter`
//! members (`:306-316`) hand it the script's functions; the callbacks then run
//! on the archive read path, which may be a resource worker, so the reference
//! keeps one engine per thread. This port has no way to build such a private
//! runtime from a plugin, and an extraction filter is `Send + Sync` precisely
//! because it can be called off the script thread — the bridge has to solve
//! that, not route a `Runtime` handle across it. So the decision path below
//! reads the configuration and stops there, and no filter is installed.
//!
//! `Storages.setXP3ArchiveExtractionFilter`/`setXP3ArchiveContentFilter` stay
//! **not** installed in the main engine: the reference installs them only
//! inside the plugin's private script engine, and a main-engine copy would be
//! a member the reference's main engine does not have. Now that
//! `plugin_api::xp3` exists, a plugin's *Rust* side can install filters
//! directly; the script-facing spelling remains private to the bridge.
//!
//! # What this port implements
//!
//! The **decision path** the reference runs at registration:
//!
//! * `register` probes `xp3filter.tjs` in the application path with the
//!   no-search existence check (`TVPGetAppPath() + "xp3filter.tjs"` +
//!   `TVPIsExistentStorageNoSearch`, `K2 xp3filter.cpp:440-441`). The project
//!   root is this engine's application path, so the name is probed as stored
//!   and with the reference's `./` spelling.
//! * When the file is **absent** the reference does nothing, and so does this
//!   port: no filter, no hook warning, no surface. (The module does report
//!   itself once as a `Missing` catalog entry — the engine's own placeholder
//!   contract, `crate::placeholder` — with the reason attached.)
//! * When it is **present** the reference reads it and activates both filters.
//!   This port reads it (with the host's configured text encoding, the way
//!   `TVPCreateTextStreamForRead(path, "")` does) and reports once through the
//!   engine log that the script bridge is what is missing — the file was read,
//!   but nothing executed its `Storages.setXP3Archive*` calls. Registration
//!   still succeeds: the seam itself is live and reachable from a plugin's
//!   Rust side (`crate::plugin_api::xp3`).
//!
//! `Storages.setXP3ArchiveExtractionFilter`/`setXP3ArchiveContentFilter` are
//! deliberately **not** installed in the main engine: the reference installs
//! them only inside the plugin's private script engines, and a main-engine copy
//! would be a member the reference's main engine does not have.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "XP3 extraction/content filters (`xp3filter.tjs` decision path only)",
    notes: "The reference's registration-time decision path is implemented: `xp3filter.tjs` in \
            the application path is probed with the no-search existence check, a missing file \
            leaves everything alone exactly like the reference, and a present file is read and \
            reported once through the engine log. The engine's filter seam exists and a plugin's \
            Rust side can install into it (`plugin_api::xp3`, backed by krkr-xp3's live-read \
            `Xp3FilterRegistry`), but this module still installs nothing: the reference runs the \
            configuration in a private per-thread TJS engine whose \
            `Storages.setXP3Archive*` members hand it the script's callbacks, and this port has \
            no way to build that bridge — so the module keeps reporting itself as missing, with \
            the reason attached.",
    install: |engine| engine.register_plugin(Xp3FilterPlugin::new()),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what a game's
/// `Plugins.link("xp3filter.dll")` resolves through [`crate::catalog`].
pub(crate) const NAME: &str = "xp3filter.dll";

/// The configuration file the post-registration callback looks for
/// (`K2 xp3filter.cpp:440`).
pub(crate) const CONFIG_NAME: &str = "xp3filter.tjs";

pub struct Xp3FilterPlugin {
    /// Whether the not-implemented report has been written. The catalog's
    /// status contract (`crate::catalog`'s
    /// `every_entry_installs_the_plugin_it_names_and_only_a_placeholder_reports_itself`)
    /// requires a [`PluginStatus::Missing`] entry to report itself exactly once
    /// per installation; the script bridge is still missing, so this module
    /// keeps reporting that, with the *reason* attached.
    reported_missing: std::sync::atomic::AtomicBool,
    /// Whether the missing-bridge diagnostic has been written. The reference
    /// probes (and loads) the configuration on every module registration; the
    /// probe still runs every time, only the report is once per plugin
    /// instance.
    reported_bridge: std::sync::atomic::AtomicBool,
}

impl Xp3FilterPlugin {
    pub fn new() -> Self {
        Self {
            reported_missing: std::sync::atomic::AtomicBool::new(false),
            reported_bridge: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl Default for Xp3FilterPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KrkrPlugin for Xp3FilterPlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The status is `Missing` because the script bridge a game needs is not
        // built; a missing module stays visible in the log instead of silently
        // absent (the `crate::placeholder` contract).
        if !self
            .reported_missing
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            runtime.host_mut().log(&format!(
                "WARN plugin not implemented: {NAME} — the reference runs `xp3filter.tjs` in a \
                 private per-thread TJS engine and installs its callbacks into the archive read \
                 path; this port has the engine seam (`plugin_api::xp3`) while the script bridge \
                 itself is unbuilt, so only the registration-time decision path runs and no \
                 filter is installed"
            ));
        }

        let Ok(storage) = runtime.host().project_storage() else {
            // A script-only host has no application path to probe; the
            // reference's check would find nothing there either.
            return Ok(());
        };
        let Some(path) = find_config(storage) else {
            // The reference's absent-configuration branch: nothing happens.
            return Ok(());
        };
        if self
            .reported_bridge
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }

        // The reference reads the file and executes it in a private TJS engine
        // that carries the two `Storages.setXP3Archive*` members
        // (`K2 xp3filter.cpp:317, 440-455`), which hand it the script's
        // callbacks. This port has no way to build that private runtime, and a
        // filter is `Send + Sync` because it can be called off the script
        // thread — so the bridge is what is missing, and the read is the last
        // meaningful step of the decision path. The engine seam itself is live
        // (`plugin_api::xp3`), so a Rust-side install would already work.
        let encoding = runtime.host().text_encoding().to_owned();
        let detail = match storage.read_text_storage(&path, &encoding) {
            Ok(text) => format!("{} bytes", text.len()),
            Err(error) => format!("unreadable: {error}"),
        };
        runtime.host_mut().log(&format!(
            "WARN xp3filter: `{path}` is configured ({detail}), but this port has no script \
             bridge: the engine's XP3 filter seam exists (`plugin_api::xp3` over krkr-xp3's \
             `Xp3FilterRegistry`, read per entry-stream creation and per read chunk), while the \
             configuration's `Storages.setXP3Archive*` callbacks need the reference's private \
             per-thread TJS engine (`xp3filter.cpp:212-220`, `:295-354`), which this port does \
             not build. The reference's extraction/content filters stay uninstalled; a Rust-side \
             install through `plugin_api::xp3` needs no bridge."
        ));
        Ok(())
    }

    fn unregister(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // Nothing was installed: the reference's unregistration (the sample's
        // `V2Unlink`, `xp3dec/main.cpp:112-124`) clears a filter this port
        // never set.
        Ok(())
    }
}

/// The reference's existence probe: `TVPGetAppPath() + "xp3filter.tjs"` with
/// `TVPIsExistentStorageNoSearch` (`K2 xp3filter.cpp:440-441`). The project
/// root is the application path here, and the storage resolves the reference's
/// `./` spelling as well, so both spellings are probed in that order.
fn find_config(storage: &dyn krkr_engine::plugin_api::ProjectStoragePort) -> Option<String> {
    [CONFIG_NAME, "./xp3filter.tjs"]
        .into_iter()
        .find(|name| storage.storage_exists_exact(name))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::SystemTime,
    };

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths};

    use super::*;

    fn temp_root(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-xp3filter-{prefix}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    fn project_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("project storage");
        KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine")
    }

    fn logs(engine: &KrkrEngine) -> String {
        engine.host().logs().join("\n")
    }

    /// The unconfigured case: no `xp3filter.tjs` means the reference does
    /// nothing, and so does this port — no warning, no surface.
    #[test]
    fn an_absent_configuration_does_nothing() {
        let root = temp_root("absent");
        let mut engine = project_engine(&root);
        engine
            .register_plugin(Xp3FilterPlugin::new())
            .expect("register");
        // The missing module reports itself (the catalog's placeholder rule),
        // but an absent configuration must not produce the hook warning.
        let logs = logs(&engine);
        assert_eq!(
            logs.matches("not implemented: xp3filter.dll").count(),
            1,
            "the missing module must report itself once, got: {logs}"
        );
        assert!(
            !logs.contains("no script bridge"),
            "an absent configuration must not warn about the bridge, got: {logs}"
        );
        // The reference's `Storages.setXP3Archive*` members exist only in its
        // private script engines; the main engine must not grow them.
        for member in [
            "Storages.setXP3ArchiveExtractionFilter",
            "Storages.setXP3ArchiveContentFilter",
        ] {
            let value = engine
                .execute_expression("inline.tjs", &format!("typeof {member}"))
                .expect("probe");
            assert_eq!(
                value,
                krkr_tjs2::runtime::Variant::String("undefined".to_string()),
                "{member} must not exist in the main engine"
            );
        }
    }

    /// The configured case: the decision path detects the file, reads it and
    /// reports the missing script bridge instead of pretending to install a
    /// filter.
    #[test]
    fn a_present_configuration_reports_the_missing_bridge() {
        let root = temp_root("present");
        fs::write(
            root.join(CONFIG_NAME),
            "// xp3filter.tjs\nStorages.setXP3ArchiveExtractionFilter(function(){});\n",
        )
        .expect("write config");
        let mut engine = project_engine(&root);
        engine
            .register_plugin(Xp3FilterPlugin::new())
            .expect("register");

        let logs = logs(&engine);
        assert!(
            logs.contains("xp3filter: `xp3filter.tjs` is configured"),
            "the detection must be logged, got: {logs}"
        );
        assert!(
            logs.contains("no script bridge"),
            "the missing capability must be named, got: {logs}"
        );
        assert!(
            logs.contains("bytes"),
            "the read configuration must be reported, got: {logs}"
        );
    }

    /// The detection itself: both the bare name and the reference's `./`
    /// spelling are probed, and a missing file answers `None`.
    #[test]
    fn the_config_probe_follows_the_reference_path() {
        let root = temp_root("probe");
        let storage = ProjectStorage::for_root(&root).expect("storage");
        assert_eq!(find_config(&storage), None);
        fs::write(root.join(CONFIG_NAME), "// config\n").expect("write");
        assert_eq!(find_config(&storage), Some(CONFIG_NAME.to_string()));
    }

    /// `Plugins.link`/`Plugins.unlink` register and unregister the module
    /// without installing or removing anything (the reference has no
    /// unregistration callback for the filter).
    #[test]
    fn linking_and_unlinking_are_no_ops_beyond_the_diagnostic() {
        let root = temp_root("link");
        fs::write(root.join(CONFIG_NAME), "// config\n").expect("write");
        let mut engine = project_engine(&root);
        engine
            .register_plugin(Xp3FilterPlugin::new())
            .expect("register through the host registry");

        engine
            .execute_expression("inline.tjs", "Plugins.link(\"xp3filter.dll\")")
            .expect("link");
        engine
            .execute_expression("inline.tjs", "Plugins.unlink(\"xp3filter.dll\")")
            .expect("unlink");
        engine
            .execute_expression("inline.tjs", "Plugins.link(\"xp3filter.dll\")")
            .expect("re-link");
        // One diagnostic per plugin instance, not one per link.
        assert_eq!(logs(&engine).matches("no script bridge").count(), 1);
    }

    /// A host without project storage has no application path to probe; the
    /// module still reports itself as missing and installs cleanly.
    #[test]
    fn a_host_without_storage_stays_installable() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(Xp3FilterPlugin::new())
            .expect("install into a script-only host");
        let logs = logs(&engine);
        assert!(logs.contains("not implemented: xp3filter.dll"));
        assert!(!logs.contains("no script bridge"));
    }
}
