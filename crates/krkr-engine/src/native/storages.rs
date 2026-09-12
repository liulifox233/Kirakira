use krkr_tjs2::{
    Result,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::host::KrkrHost;

use super::{arg_string, install_static_object, native_void};

pub(crate) fn install_storages(runtime: &mut Runtime<KrkrHost>) {
    let storages = install_static_object(runtime, "Storages");
    runtime.register_object_native(storages, "addAutoPath", storages_add_auto_path);
    runtime.register_object_native(storages, "removeAutoPath", storages_remove_auto_path);
    runtime.register_object_native(storages, "setTextEncoding", storages_set_text_encoding);
    runtime.register_object_native(storages, "getFullPath", storages_get_full_path);
    runtime.register_object_native(storages, "getPlacedPath", storages_get_placed_path);
    runtime.register_object_native(storages, "isExistentStorage", storages_exists);
    runtime.register_object_native(storages, "isExistentDirectory", storages_is_directory);
    runtime.register_object_native(storages, "dirlist", storages_dirlist);
    runtime.register_object_native(storages, "extractStorageExt", storages_extract_ext);
    runtime.register_object_native(storages, "extractStorageName", storages_extract_name);
    runtime.register_object_native(storages, "extractStoragePath", storages_extract_path);
    runtime.register_object_native(storages, "chopStorageExt", storages_chop_ext);
    runtime.register_object_native(storages, "clearArchiveCache", storages_clear_archive_cache);
    runtime.register_object_native(storages, "getLocalName", storages_get_local_name);
    runtime.register_object_native(storages, "selectFile", native_void);
    runtime.register_object_native(storages, "searchCD", native_void);
}

fn storages_add_auto_path(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let path = args
        .first()
        .ok_or_else(|| krkr_tjs2::TjsError::runtime("Storages.addAutoPath requires a path"))?
        .to_tjs_string()?;
    validate_auto_path(&path, "Storages.addAutoPath")?;
    runtime.host_mut().add_auto_path(path);
    Ok(Variant::Void)
}

fn storages_remove_auto_path(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let path = args
        .first()
        .ok_or_else(|| krkr_tjs2::TjsError::runtime("Storages.removeAutoPath requires a path"))?
        .to_tjs_string()?;
    validate_auto_path(&path, "Storages.removeAutoPath")?;
    runtime.host_mut().remove_auto_path(&path);
    Ok(Variant::Void)
}

fn storages_set_text_encoding(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(encoding) = arg_string(&args, 0)? {
        runtime.host_mut().set_text_encoding(encoding.clone());
        if let Some(scripts) = runtime.global_member("Scripts").object_handle() {
            runtime.set_object_member(scripts, "textEncoding", Variant::String(encoding));
        }
    }
    Ok(Variant::Void)
}

fn storages_get_full_path(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = arg_string(&args, 0)?.ok_or_else(|| {
        krkr_tjs2::TjsError::runtime("Storages.getFullPath requires a storage name")
    })?;
    Ok(Variant::String(
        runtime.host().normalize_storage_name(&name)?,
    ))
}

fn storages_get_placed_path(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = arg_string(&args, 0)?.ok_or_else(|| {
        krkr_tjs2::TjsError::runtime("Storages.getPlacedPath requires a storage name")
    })?;
    let path = runtime
        .host()
        .placed_storage_name(&name)
        .unwrap_or_default();
    Ok(Variant::String(path))
}

fn storages_get_local_name(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = arg_string(&args, 0)?.ok_or_else(|| {
        krkr_tjs2::TjsError::runtime("Storages.getLocalName requires a storage name")
    })?;
    Ok(Variant::String(
        runtime
            .host()
            .placed_path(&name)
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
    ))
}

fn validate_auto_path(path: &str, method: &str) -> Result<()> {
    if path.is_empty()
        || !path
            .chars()
            .last()
            .is_some_and(|ch| matches!(ch, '/' | '\\' | '>'))
    {
        return Err(krkr_tjs2::TjsError::runtime(format!(
            "{method} requires a path ending in '/', '\\' or '>'"
        )));
    }
    Ok(())
}

fn storages_exists(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let exists =
        arg_string(&args, 0)?.is_some_and(|name| runtime.host().storage_exists_exact(&name));
    Ok(Variant::Integer(i64::from(exists)))
}

fn storages_is_directory(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let exists =
        arg_string(&args, 0)?.is_some_and(|name| runtime.host().storage_is_directory(&name));
    Ok(Variant::Integer(i64::from(exists)))
}

fn storages_dirlist(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(name) = arg_string(&args, 0)? else {
        return Err(krkr_tjs2::TjsError::runtime(
            "Storages.dirlist requires a directory",
        ));
    };
    let entries = runtime.host().storage_dirlist(&name)?;
    let values = entries.into_iter().map(Variant::String).collect();
    Ok(Variant::Object(runtime.alloc_array_object(values)))
}

fn storages_extract_ext(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        arg_string(&args, 0)?
            .map(|value| extract_storage_ext(&value))
            .unwrap_or_default(),
    ))
}

fn storages_extract_name(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        arg_string(&args, 0)?
            .map(|value| extract_storage_name(&value))
            .unwrap_or_default(),
    ))
}

fn storages_extract_path(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        arg_string(&args, 0)?
            .map(|value| extract_storage_path(&value))
            .unwrap_or_default(),
    ))
}

fn storages_chop_ext(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::String(
        arg_string(&args, 0)?
            .map(|value| chop_storage_ext(&value))
            .unwrap_or_default(),
    ))
}

fn storages_clear_archive_cache(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    runtime.host().clear_archive_cache()?;
    Ok(Variant::Void)
}

fn storage_delimiter_index(name: &str) -> Option<usize> {
    name.rfind(['/', '\\', '>'])
}

fn storage_file_start(name: &str) -> usize {
    storage_delimiter_index(name)
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn storage_extension_index(name: &str) -> Option<usize> {
    let start = storage_file_start(name);
    name[start..].rfind('.').map(|index| start + index)
}

fn extract_storage_ext(name: &str) -> String {
    storage_extension_index(name)
        .map(|index| name[index..].to_string())
        .unwrap_or_default()
}

fn extract_storage_name(name: &str) -> String {
    name[storage_file_start(name)..].to_string()
}

fn extract_storage_path(name: &str) -> String {
    storage_delimiter_index(name)
        .map(|index| name[..=index].to_string())
        .unwrap_or_default()
}

fn chop_storage_ext(name: &str) -> String {
    storage_extension_index(name)
        .map(|index| name[..index].to_string())
        .unwrap_or_else(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use krkr_assets::{ProjectStorage, StorageMediaProvider};
    use krkr_core::{ProjectStoragePort, ResourceStream};
    use krkr_tjs2::runtime::Variant;

    use crate::engine::{EngineConfig, KrkrEngine};

    /// A media that records the name space of every probe, so a test can see
    /// that a normalized name actually reached the provider.
    struct ProbeMedia {
        entry: &'static str,
        asked: Mutex<Vec<String>>,
    }

    impl ProbeMedia {
        fn new(entry: &'static str) -> Self {
            Self {
                entry,
                asked: Mutex::new(Vec::new()),
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().expect("media lock").clone()
        }
    }

    impl StorageMediaProvider for ProbeMedia {
        fn media_name(&self) -> &str {
            "psb"
        }

        fn exists(&self, name: &str) -> bool {
            self.asked
                .lock()
                .expect("media lock")
                .push(name.to_string());
            name == self.entry
        }

        fn open(&self, name: &str) -> std::io::Result<Box<dyn ResourceStream>> {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no entry `{name}`"),
            ))
        }
    }

    /// Evaluates `Storages.getFullPath(<name>)` the way a script reaches it.
    fn full_path(engine: &mut KrkrEngine, name: &str) -> String {
        let source = format!("return Storages.getFullPath({name:?});");
        match engine
            .execute_script("getFullPath-probe.tjs", &source)
            .expect("script")
        {
            Variant::String(value) => value,
            other => panic!("Storages.getFullPath returned {other:?}"),
        }
    }

    #[test]
    fn get_full_path_keeps_the_reference_trailing_delimiter() {
        // krkrz's `getFullPath` is `TVPNormalizeStorageName`
        // (`StorageIntf.cpp:1391-1402`, `:549`); its compression loop
        // (`:400-453`) deletes a delimiter only when another one follows, so a
        // trailing `/` survives. GINKA's `addAutoPathRecursive` feeds the
        // result to `Storages.isExistentDirectory`/`dirlist`, whose fstat
        // override demands the trailing `/` (`fstat/Main.cpp:759-762`).
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        assert_eq!(full_path(&mut engine, "a/b/"), "a/b/");
        assert_eq!(full_path(&mut engine, "a/b"), "a/b");
        assert_eq!(full_path(&mut engine, "a/b//"), "a/b/");
        assert_eq!(full_path(&mut engine, "./setup/debug/"), "setup/debug/");
        assert_eq!(full_path(&mut engine, "setup\\debug\\"), "setup/debug/");
        assert_eq!(full_path(&mut engine, "setup/debug/../"), "setup/");
        assert_eq!(
            full_path(&mut engine, "archive.xp3>DIR/"),
            "archive.xp3>dir/"
        );
        // The delimiter in front of `>` is a duplicated one for the reference
        // and disappears (`StorageIntf.cpp:392-398`, `:405`, `:409-413`).
        assert_eq!(
            full_path(&mut engine, "archive.xp3/>DIR/"),
            "archive.xp3>dir/"
        );
        assert_eq!(full_path(&mut engine, "/"), "/");
        assert_eq!(full_path(&mut engine, ""), "");
    }

    #[test]
    fn get_full_path_keeps_the_media_prefix() {
        // `TVPNormalizeStorageName` splits `media://domain/path`, lower-cases
        // the media name and re-emits `media + "://" + domain + path`
        // (`StorageIntf.cpp:299-354`, `:366-374`, `:462`), so the two slashes
        // are structure and survive; folding them to `psb:/container.psb/...`
        // would leave a name no registered media can dispatch, because
        // `split_media_name` requires the literal `://`.
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        assert_eq!(
            full_path(&mut engine, "psb://container.psb/inner/"),
            "psb://container.psb/inner/"
        );
        assert_eq!(
            full_path(&mut engine, "psb://container.psb/inner"),
            "psb://container.psb/inner"
        );
        // A media-root name, and the media name lower-cased the way the
        // reference does (`StorageIntf.cpp:366-374`).
        assert_eq!(
            full_path(&mut engine, "psb://container.psb/"),
            "psb://container.psb/"
        );
        assert_eq!(
            full_path(&mut engine, "PSB://container.psb"),
            "psb://container.psb"
        );
        // The archive delimiter splits first, so the media prefix sits on the
        // outer half only.
        assert_eq!(
            full_path(&mut engine, "psb://container.psb/arc.xp3>DIR/"),
            "psb://container.psb/arc.xp3>dir/"
        );
    }

    /// The round trip the finding lost: the string `Storages.getFullPath`
    /// returns has to reach the registered provider when a script feeds it
    /// back into a `Storages` call.
    #[test]
    fn get_full_path_media_names_stay_dispatchable() {
        let media = Arc::new(ProbeMedia::new("container.psb/inner"));
        let storage = Arc::new(ProjectStorage::new(None, Vec::new(), None, Vec::new()));
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::clone(&storage) as Arc<dyn ProjectStoragePort>),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine
            .tjs_runtime_mut()
            .host_mut()
            .register_storage_media(Arc::clone(&media) as Arc<dyn StorageMediaProvider>)
            .expect("register media");

        let result = engine
            .execute_script(
                "getFullPath-round-trip.tjs",
                r#"
                var full = Storages.getFullPath("psb://container.psb/inner");
                return full + "|" + Storages.isExistentStorage(full);
                "#,
            )
            .expect("script");

        assert_eq!(
            result,
            Variant::String("psb://container.psb/inner|1".to_string())
        );
        assert_eq!(media.asked(), vec!["container.psb/inner".to_string()]);
    }

    /// The other consumer of the same normalization: `addAutoPath` stores the
    /// media spelling, and `removeAutoPath` has to match it after its own
    /// normalization pass.
    #[test]
    fn media_auto_paths_keep_their_prefix_through_add_and_remove() {
        let storage = Arc::new(ProjectStorage::new(None, Vec::new(), None, Vec::new()));
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::clone(&storage) as Arc<dyn ProjectStoragePort>),
            ..EngineConfig::default()
        })
        .expect("engine");

        engine
            .execute_script(
                "auto-path-media.tjs",
                r#"Storages.addAutoPath("psb://container.psb/");"#,
            )
            .expect("script");

        // `addAutoPath` trims the required trailing delimiter, keeping the
        // `://` the media registry dispatches on.
        assert_eq!(
            storage.auto_paths(),
            vec!["psb://container.psb".to_string()]
        );
        assert!(
            engine
                .tjs_runtime_mut()
                .host_mut()
                .remove_auto_path("psb://container.psb")
        );
        assert!(storage.auto_paths().is_empty());
    }
}
