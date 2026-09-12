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
//! # What this engine has, and what it does not
//!
//! The equivalent primitive exists: `krkr-xp3` defines
//! `Xp3ExtractionFilter::apply(uncompressed_offset, buffer, file_hash)`
//! (`crates/krkr-xp3/src/util.rs:45-56`) and applies it per decompressed chunk
//! with the same `(position, buffer, file_hash)` shape the reference's info
//! struct carries (`crates/krkr-xp3/src/stream.rs:224-226`); it is configured
//! through `Xp3OpenOptions::with_extraction_filter`
//! (`crates/krkr-xp3/src/options.rs:47-53`).
//!
//! **No plugin can reach it.** The archives are opened once while the project
//! storage is built — `ProjectStorage::for_root` →
//! `open_project_archives` → `Xp3ResourceProvider::open_archives`, i.e.
//! *default* options with no filter (`crates/krkr-assets/src/storage.rs:260-264,
//! 2624-2630`) — which happens before the engine exists and therefore before
//! any `KrkrPlugin::register` runs; the filter is then frozen per entry stream
//! (`crates/krkr-xp3/src/archive.rs:100-120`). The plugin-facing engine surface
//! (`crates/krkr-engine/src/plugin_api/mod.rs:42-46`) is storage media,
//! transitions, layer bitmaps and video, and there is no later
//! `TVPSetXP3ArchiveExtractionFilter`-style setter anywhere in `crates/`
//! (`with_extraction_filter` is constructed nowhere outside its own unit
//! tests). The K2 **content filter does not exist at all**: no hook, no per-file
//! context, no "read whole file" decision.
//!
//! So the reference's hook — a callback installed into the engine's XP3 read
//! path — has no seam to be installed into, and adding one would be an engine
//! change (a plugin-facing XP3 API, plus an ordering decision: the filter must
//! be settable *before* the project archives open, or the archives must be
//! re-openable). Inventing that API inside this plugin would be a surface the
//! reference does not have and the engine cannot honour; the gap is recorded as
//! a finding instead (`.tower/comms/findings/` — "no plugin-facing XP3 filter
//! seam", filed with M98).
//!
//! # What this port implements
//!
//! Exactly the part of the plugin that does not need the seam — the
//! **decision path** the reference runs at registration:
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
//!   engine log that the engine has no XP3 filter seam, naming the file and the
//!   missing capability. It does not install anything and it does not pretend:
//!   registration still succeeds, because the archive read path was already
//!   fixed when the project storage opened.
//!
//! `Storages.setXP3ArchiveExtractionFilter`/`setXP3ArchiveContentFilter` are
//! deliberately **not** installed in the main engine: the reference installs
//! them only inside the plugin's private script engines, and a main-engine copy
//! would be a member the reference's main engine does not have (and, here, one
//! with nothing to control).

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Missing,
    feature: "XP3 extraction/content filters (`xp3filter.tjs` decision path only)",
    notes: "The reference's registration-time decision path is implemented: `xp3filter.tjs` in \
            the application path is probed with the no-search existence check, a missing file \
            leaves everything alone exactly like the reference, and a present file is read and \
            reported once through the engine log. The hook itself cannot be installed: archives \
            are opened with the engine's default options while the project storage is built, \
            before any plugin registers, and the plugin-facing API has no XP3 filter setter — \
            there is also no content-filter primitive at all. The gap is filed as a finding \
            rather than papered over with an invented `Storages.setXP3Archive*` member (the \
            reference exposes those only inside the plugin's own private script engines).",
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
    /// per installation; the hook is still missing, so this module keeps
    /// reporting that, with the *reason* attached.
    reported_missing: std::sync::atomic::AtomicBool,
    /// Whether the missing-seam diagnostic has been written. The reference
    /// probes (and loads) the configuration on every module registration; the
    /// probe still runs every time, only the report is once per plugin
    /// instance.
    reported_seam: std::sync::atomic::AtomicBool,
}

impl Xp3FilterPlugin {
    pub fn new() -> Self {
        Self {
            reported_missing: std::sync::atomic::AtomicBool::new(false),
            reported_seam: std::sync::atomic::AtomicBool::new(false),
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
        // The status is `Missing` because the hook a game needs is not
        // installable; a missing module stays visible in the log instead of
        // silently absent (the `crate::placeholder` contract).
        if !self
            .reported_missing
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            runtime.host_mut().log(&format!(
                "WARN plugin not implemented: {NAME} — the XP3 extraction/content filter hook has \
                 no plugin-facing engine seam (project archives open with default options before \
                 plugins register); only the reference's registration-time `xp3filter.tjs` \
                 decision path runs and no TJS surface is installed"
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
            .reported_seam
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }

        // The reference reads the file and executes it in a private TJS engine
        // that carries the two `Storages.setXP3Archive*` members
        // (`K2 xp3filter.cpp:317, 440-455`). This engine cannot host that
        // private runtime (the plugin-facing API has no way to build one) and
        // has no filter to hand the script's callbacks to, so the read is the
        // last meaningful step of the decision path and the gap is reported.
        let encoding = runtime.host().text_encoding().to_owned();
        let detail = match storage.read_text_storage(&path, &encoding) {
            Ok(text) => format!("{} bytes", text.len()),
            Err(error) => format!("unreadable: {error}"),
        };
        runtime.host_mut().log(&format!(
            "WARN xp3filter: `{path}` is configured ({detail}), but this engine has no XP3 \
             filter seam: project archives are opened with default options before plugins \
             register (`krkr-assets/src/storage.rs:2624-2630`), `Xp3OpenOptions::\
             with_extraction_filter` is never constructed in production code, and no \
             content-filter primitive exists. The reference's extraction/content filters stay \
             uninstalled; see the M98 finding `no plugin-facing XP3 filter seam`."
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
            !logs.contains("no XP3 filter seam"),
            "an absent configuration must not warn about the hook, got: {logs}"
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
    /// reports the missing engine seam instead of pretending to install a
    /// filter.
    #[test]
    fn a_present_configuration_reports_the_missing_seam() {
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
            logs.contains("no XP3 filter seam"),
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
        assert_eq!(logs(&engine).matches("no XP3 filter seam").count(), 1);
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
        assert!(!logs.contains("no XP3 filter seam"));
    }
}
