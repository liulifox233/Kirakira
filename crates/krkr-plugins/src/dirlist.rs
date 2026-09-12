//! `dirlist.dll` — the global `getDirList(dir)` directory lister.
//!
//! Reference: the krkr2 trunk `plugins/win32/dirlist/Main.cpp` (220 lines;
//! the dossier records the older krkrz snapshot at 203 lines, without the
//! `MAX_PATH` guard the trunk revision adds at `:51-53` —
//! `docs/plugins/system-storage.md`, dirlist section). The whole DLL is one
//! `tTJSDispatch` (`:11-109`) installed on the script global as `getDirList`
//! (`:145-152`) and removed again by `V2Unlink` (`:200-205`).
//!
//! Surface, verified line by line against the source:
//!
//! * `numparams < 1` → `TJS_E_BADPARAMCOUNT` (`:21`); extra arguments are
//!   ignored. The parameter is stringified with `ttstr` (`:23`).
//! * The directory name must end in `/`, else the exception
//!   `'/' must be specified at the end of given directory name.` (`:25-26`).
//! * The name goes through `TVPNormalizeStorageName` and `TVPGetLocalName`
//!   (`:29-30`).
//! * The result is a TJS `Array` (`:33-41`) of the directory's **immediate
//!   children**, names only: a subdirectory is spelled with a trailing `/`
//!   (`:71-76`), a file bare (`:77-81`), and entries are appended in the order
//!   `FindFirstFile` enumerates them (`:62-87`).
//! * A directory the OS cannot open throws `Directory not found.` (`:92`).
//!
//! # How the port lists
//!
//! Through the engine's storage listing, [`KrkrHost::storage_dirlist`] — the
//! same call `Storages.dirlist` answers with (`native/storages.rs:161-176`).
//! That listing is already the KRKR/fstat shape this plugin promises
//! (`krkr-assets/src/storage.rs:920-1027`): immediate children only, a
//! trailing `/` on directories, and the filesystem, XP3, memory, media and
//! manifest layers merged. The reference's `FindFirstFile` rules map onto it
//! as follows; the parts that cannot are deliberate divergences, recorded the
//! way the sibling `fstat` port records its own (`fstat.rs:21-34`):
//!
//! * **Filtering**: the reference's `*.*` wildcard on Win32 matches every
//!   entry — extensionless names, hidden files and all — and the port lists
//!   every child too. The one filter the reference *has* is `FindFirstFile`
//!   itself: it returns the `.` and `..` pseudo-entries for a wildcard
//!   search, and the reference appends `/` to both (they carry
//!   `FILE_ATTRIBUTE_DIRECTORY`; the same loop in `fstat/Main.cpp:480-490`
//!   shows the behaviour the two plugins share). The port lists real
//!   children only — the shape the engine's own `Storages.dirlist` promises
//!   (`krkr-core/src/media.rs:104-107`) and the divergence the `fstat` port
//!   already ships (`fstat.rs:21-23`).
//! * **Ordering**: `FindFirstFile` order is filesystem-dependent and not a
//!   contract. The engine's listing is deterministically sorted by lower-cased
//!   name, and the port keeps that (so a game that iterates sees a stable
//!   order instead of an arbitrary one).
//! * **Casing and layering**: names keep the spelling of the layer that owns
//!   the child, de-duplicated case-insensitively with the filesystem first —
//!   an engine property of the merged listing. The reference only ever saw
//!   the local filesystem, so archive, memory and media children are the
//!   second divergence, again shared with `fstat`.
//! * **Name length**: the trunk revision rejects a local name whose narrow
//!   length is `>= MAX_PATH - 3` with `Too long directory name.`
//!   (`Main.cpp:51-53`). The reference needs the check because it copies the
//!   name into `char[MAX_PATH + 1]` buffers for `FindFirstFile`; the storage
//!   listing has no fixed-size path buffer, so the port does not reproduce
//!   the platform artifact (the engine deliberately supports paths longer
//!   than `MAX_PATH`). Everything else about the call, errors included, is
//!   the reference's.

use krkr_engine::{KrkrHost, KrkrPlugin, plugin_api};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "getDirList(dir) global directory lister",
    notes: "A port of dirlist/Main.cpp: the global getDirList returns the \
            engine's storage listing as a TJS Array, with the reference's \
            argument checks (BADPARAMCOUNT, mandatory trailing '/', \
            \"Directory not found.\"). Divergences, all inherited from the \
            storage seam it shares with Storages.dirlist: no Win32 './'/'../' \
            pseudo-entries, deterministic case-insensitive order instead of \
            FindFirstFile order, archive/memory members listed too, and no \
            MAX_PATH guard (krkr2/Main.cpp:51-53, a fixed-buffer artifact).",
    install: |engine| engine.register_plugin(DirlistPlugin),
};

pub struct DirlistPlugin;

impl KrkrPlugin for DirlistPlugin {
    fn name(&self) -> &str {
        "dirlist.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // `Plugins.link` re-runs this after boot; a script that replaced the
        // global keeps its own closure.
        if matches!(runtime.global_member("getDirList"), Variant::Closure(_)) {
            return Ok(());
        }
        // `FuncCall`'s own `numparams < 1` check (`Main.cpp:21`), declared at
        // the registration site instead.
        runtime.register_global_native_with_arg_count(
            "getDirList",
            NativeArgCount::AtLeast(1),
            get_dir_list,
        );
        Ok(())
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // `V2Unlink` deletes the global member (`Main.cpp:188-207`).
        if matches!(runtime.global_member("getDirList"), Variant::Closure(_)) {
            return Ok(());
        }
        runtime.delete_object_member(runtime.global_handle(), "getDirList");
        Ok(())
    }
}

/// `tGetDirListFunction::FuncCall` (`Main.cpp:13-108`).
fn get_dir_list(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `ttstr dir(*param[0])` (`Main.cpp:23`): any variant stringifies, so a
    // `void` argument becomes the empty name and fails the `/` check below.
    let directory = match args.first() {
        Some(value) => value.to_tjs_string()?,
        None => return Err(TjsError::bad_param_count()),
    };

    // `dir.GetLastChar() != '/'` (`Main.cpp:25-26`).
    if !directory.ends_with('/') {
        return Err(TjsError::runtime(
            "'/' must be specified at the end of given directory name.",
        ));
    }

    // `TVPNormalizeStorageName` + `TVPGetLocalName` (`Main.cpp:29-30`): the
    // engine's listing resolves the name through the storage stack itself.
    // A script-thread storage probe refreshes the watched script dictionaries
    // first, exactly like `Storages.dirlist` (`native/storages.rs:161-176`).
    plugin_api::storage::refresh_storage_tables(runtime);
    let entries = runtime
        .host()
        .storage_dirlist(&directory)
        // The reference's single failure mode for the search (`Main.cpp:92`).
        .map_err(|_| TjsError::runtime("Directory not found."))?;

    // `TVPExecuteExpression("[]")` plus `PropSetByNum` (`Main.cpp:33-85`).
    let values = entries.into_iter().map(Variant::String).collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, time::SystemTime};

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths};
    use krkr_tjs2::runtime::Variant;

    use super::DirlistPlugin;

    /// A fresh project root under the system temp directory, the fixture
    /// pattern the sibling `fstat`/`save_struct` tests use.
    fn test_root(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-dirlist-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    fn test_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(DirlistPlugin).expect("plugin");
        engine
    }

    /// Evaluates an expression that joins the returned array's elements, so a
    /// test can compare the listing exactly — separators, order, casing and
    /// the trailing `/` on directories included.
    fn listing(engine: &mut KrkrEngine, directory: &str) -> String {
        match engine
            .execute_expression(
                "inline.tjs",
                &format!(
                    "(function() {{ var entries = getDirList({directory:?});\n\
                     var text = entries.count + \":\";\n\
                     for (var i = 0; i < entries.count; i++) {{\n\
                         if ((entries[i] == \".\") || (entries[i] == \"..\") ||\n\
                             (entries[i] == \"./\") || (entries[i] == \"../\")) {{\n\
                             text += \"[dot]\";\n\
                         }}\n\
                         text += entries[i] + \"|\";\n\
                     }}\n\
                     return text; }})()"
                ),
            )
            .expect("getDirList")
        {
            Variant::String(text) => text,
            other => panic!("getDirList returned {other:?}"),
        }
    }

    /// The fixture tree the listing tests share. Every name rule the reference
    /// has shows up here: an extensionless file, a dot file, mixed casing, a
    /// nested directory and a file inside it (which must not leak into the
    /// parent's listing).
    fn fixture(root: &Path) {
        let list = root.join("list");
        fs::create_dir_all(&list).expect("create list directory");
        fs::write(list.join("Beta.txt"), b"b").expect("write Beta.txt");
        fs::write(list.join("alpha.TXT"), b"a").expect("write alpha.TXT");
        fs::write(list.join("noext"), b"n").expect("write noext");
        fs::write(list.join(".hidden"), b"h").expect("write .hidden");
        fs::create_dir_all(list.join("sub")).expect("create sub directory");
        fs::write(list.join("sub/nested.txt"), b"x").expect("write nested");
    }

    /// The reference's listing rules (`Main.cpp:57-87`), asserted exactly:
    /// every immediate child (extensionless and dot names included, nested
    /// files excluded), directories with a trailing `/`, files bare, casing
    /// as stored, and no Win32 `.`/`..` entries. Order follows the engine's
    /// case-insensitive sort — the port's documented divergence from
    /// `FindFirstFile` order.
    #[test]
    fn listing_returns_immediate_children_exactly() {
        let root = test_root("children");
        fixture(&root);
        let mut engine = test_engine(&root);

        assert_eq!(
            listing(&mut engine, "list/"),
            "5:.hidden|alpha.TXT|Beta.txt|noext|sub/|"
        );
        // A trailing `/` on the subdirectory, a bare file name for the rest:
        // `sub` itself is listed, its contents are not.
        assert!(!listing(&mut engine, "list/").contains("nested.txt"));
        // The reference's `*.*` wildcard matches extensionless names too.
        assert!(listing(&mut engine, "list/").contains("noext|"));
        // `FindFirstFile` lists hidden entries; so does the engine.
        assert!(listing(&mut engine, "list/").contains(".hidden|"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `dir.GetLastChar() != '/'` (`Main.cpp:25-26`): the exception text is
    /// the reference's, and a backslash does not count.
    #[test]
    fn a_name_without_a_trailing_slash_is_rejected() {
        let root = test_root("slash");
        fixture(&root);
        let mut engine = test_engine(&root);

        for call in ["getDirList(\"list\")", "getDirList(\"list\\\\\")"] {
            let error = engine
                .execute_expression("inline.tjs", call)
                .expect_err("a name without a trailing slash must fail");
            assert_eq!(
                error.message, "'/' must be specified at the end of given directory name.",
                "{call}"
            );
        }
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `numparams < 1` (`Main.cpp:21`) is `TJS_E_BADPARAMCOUNT`, checked
    /// before anything else — exactly like the reference.
    #[test]
    fn the_argument_is_mandatory() {
        let root = test_root("noarg");
        let mut engine = test_engine(&root);

        let error = engine
            .execute_expression("inline.tjs", "getDirList()")
            .expect_err("a missing argument must fail");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        assert_eq!(error.message, "Invalid argument count");

        // A `void` argument stringifies to the empty name (`Main.cpp:23`),
        // which then fails the `/` check rather than the count check.
        let error = engine
            .execute_expression("inline.tjs", "getDirList(void)")
            .expect_err("void must fail the slash check");
        assert_eq!(
            error.message,
            "'/' must be specified at the end of given directory name."
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `FindFirstFile` failing is `Directory not found.` (`Main.cpp:92`): a
    /// missing directory, a file name with a trailing slash, and a project
    /// with no storage at all all take that path.
    #[test]
    fn a_missing_directory_reports_directory_not_found() {
        let root = test_root("missing");
        fixture(&root);
        let mut engine = test_engine(&root);

        for call in ["getDirList(\"absent/\")", "getDirList(\"list/Beta.txt/\")"] {
            let error = engine
                .execute_expression("inline.tjs", call)
                .expect_err("must fail");
            assert_eq!(error.message, "Directory not found.", "{call}");
        }

        let mut bare = KrkrEngine::new(EngineConfig::default()).expect("engine");
        bare.register_plugin(DirlistPlugin).expect("plugin");
        let error = bare
            .execute_expression("inline.tjs", "getDirList(\"list/\")")
            .expect_err("no storage to list");
        assert_eq!(error.message, "Directory not found.");
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The reference builds the result by evaluating `[]` (`Main.cpp:33-41`):
    /// a genuine TJS `Array`, mutable like any other.
    #[test]
    fn the_result_is_a_mutable_tjs_array() {
        let root = test_root("array");
        fixture(&root);
        let mut engine = test_engine(&root);

        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() { var entries = getDirList(\"list/\");\n\
                 entries.push(\"tail\");\n\
                 return entries.count + \":\" + entries[entries.count - 1]; })()",
            )
            .expect("array");
        assert_eq!(value, Variant::String("6:tail".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// `V2Unlink` removes the global (`Main.cpp:188-207`); `Plugins.unlink`
    /// is the engine's door to `unregister`.
    #[test]
    fn unlink_removes_the_global_and_relink_installs_it_again() {
        let root = test_root("unlink");
        fixture(&root);
        let mut engine = test_engine(&root);

        engine
            .execute_script("unlink.tjs", "Plugins.unlink(\"dirlist.dll\");")
            .expect("unlink");
        let error = engine
            .execute_expression("inline.tjs", "getDirList(\"list/\")")
            .expect_err("the global is gone");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::MemberNotFound);

        engine
            .execute_script("relink.tjs", "Plugins.link(\"dirlist.dll\");")
            .expect("relink");
        assert_eq!(
            listing(&mut engine, "list/"),
            "5:.hidden|alpha.TXT|Beta.txt|noext|sub/|"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
