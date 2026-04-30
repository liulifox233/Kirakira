use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use krkr_core::ResourceProvider;
use krkr_tjs2::{Result, TjsError, runtime::TjsHost};
use krkr_xp3::Xp3ResourceProvider;

#[derive(Clone)]
pub struct KrkrHost {
    project_root: Option<PathBuf>,
    xp3_provider: Option<Xp3ResourceProvider>,
    auto_paths: Vec<String>,
    logs: Vec<String>,
    linked_plugins: BTreeSet<String>,
    text_encoding: String,
    termination_requested: bool,
}

impl Default for KrkrHost {
    fn default() -> Self {
        Self {
            project_root: None,
            xp3_provider: None,
            auto_paths: Vec::new(),
            logs: Vec::new(),
            linked_plugins: BTreeSet::new(),
            text_encoding: "UTF-8".to_string(),
            termination_requested: false,
        }
    }
}

impl KrkrHost {
    pub fn for_project(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let xp3_provider = open_project_archives(&root)?;
        Ok(Self {
            project_root: Some(root),
            xp3_provider,
            auto_paths: Vec::new(),
            logs: Vec::new(),
            linked_plugins: BTreeSet::new(),
            text_encoding: "UTF-8".to_string(),
            termination_requested: false,
        })
    }

    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }

    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    pub fn linked_plugins(&self) -> impl Iterator<Item = &str> {
        self.linked_plugins.iter().map(String::as_str)
    }

    pub fn termination_requested(&self) -> bool {
        self.termination_requested
    }

    pub fn text_encoding(&self) -> &str {
        &self.text_encoding
    }

    pub fn set_text_encoding(&mut self, encoding: impl Into<String>) {
        self.text_encoding = encoding.into();
    }

    pub fn add_auto_path(&mut self, path: impl Into<String>) {
        let path = path.into();
        if !self.auto_paths.iter().any(|item| item == &path) {
            self.auto_paths.push(path);
        }
    }

    pub fn remove_auto_path(&mut self, path: &str) -> bool {
        let before = self.auto_paths.len();
        self.auto_paths.retain(|item| item != path);
        before != self.auto_paths.len()
    }

    pub fn clear_archive_cache(&self) -> Result<()> {
        Ok(())
    }

    pub fn storage_exists(&self, name: &str) -> bool {
        self.storage_bytes(name).is_ok()
    }

    pub fn placed_path(&self, name: &str) -> Option<PathBuf> {
        self.find_fs_path(name).ok().flatten()
    }

    pub(crate) fn read_text_storage(&self, name: &str) -> Result<String> {
        let bytes = self.storage_bytes(name)?;
        String::from_utf8(bytes)
            .map_err(|error| TjsError::runtime(format!("storage `{name}` is not UTF-8: {error}")))
    }

    fn read_binary_storage(&self, name: &str) -> Result<Vec<u8>> {
        self.storage_bytes(name)
    }

    fn write_text_storage(&self, name: &str, text: &str) -> Result<()> {
        self.write_binary_storage(name, text.as_bytes())
    }

    fn write_binary_storage(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let root = self
            .project_root
            .as_ref()
            .ok_or_else(|| TjsError::runtime("project root is not set"))?;
        let relative = clean_relative_path(name)?;
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        fs::write(&path, bytes).map_err(io_error)
    }

    fn storage_bytes(&self, name: &str) -> Result<Vec<u8>> {
        if let Some(path) = self.find_fs_path(name)? {
            return fs::read(&path).map_err(io_error);
        }

        if let Some(provider) = &self.xp3_provider
            && provider.exists(name)
        {
            let mut stream = provider.open(name).map_err(io_error)?;
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).map_err(io_error)?;
            return Ok(bytes);
        }

        Err(TjsError::runtime(format!("storage `{name}` not found")))
    }

    fn find_fs_path(&self, name: &str) -> Result<Option<PathBuf>> {
        let Some(root) = &self.project_root else {
            return Ok(None);
        };
        for candidate in self.fs_candidates(name)? {
            let path = root.join(candidate);
            if path.is_file() {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    fn fs_candidates(&self, name: &str) -> Result<Vec<PathBuf>> {
        let clean = clean_relative_path(name)?;
        let mut candidates = Vec::with_capacity(self.auto_paths.len() + 1);
        candidates.push(clean.clone());
        for auto_path in self.auto_paths.iter().rev() {
            candidates.push(clean_relative_path(auto_path)?.join(&clean));
        }
        Ok(candidates)
    }

    pub(crate) fn register_plugin(&mut self, name: &str) {
        self.linked_plugins.insert(name.to_string());
    }

    pub(crate) fn link_plugin(&mut self, name: &str) {
        self.linked_plugins.insert(name.to_string());
        self.logs
            .push(format!("plugin `{name}` linked through Rust registry"));
    }

    pub(crate) fn unlink_plugin(&mut self, name: &str) -> bool {
        self.linked_plugins.remove(name)
    }

    pub(crate) fn now_millis(&mut self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0)
    }

    pub(crate) fn log(&mut self, message: &str) {
        self.logs.push(message.to_string());
    }
}

impl TjsHost for KrkrHost {
    fn read_text(&mut self, name: &str, _mode: &str) -> Result<String> {
        self.read_text_storage(name)
    }

    fn read_binary(&mut self, name: &str, _mode: &str) -> Result<Vec<u8>> {
        self.read_binary_storage(name)
    }

    fn write_text(&mut self, name: &str, _mode: &str, text: &str) -> Result<()> {
        self.write_text_storage(name, text)
    }

    fn write_binary(&mut self, name: &str, _mode: &str, bytes: &[u8]) -> Result<()> {
        self.write_binary_storage(name, bytes)
    }

    fn now_millis(&mut self) -> i64 {
        KrkrHost::now_millis(self)
    }

    fn log(&mut self, message: &str) {
        KrkrHost::log(self, message);
    }
}

fn open_project_archives(root: &Path) -> Result<Option<Xp3ResourceProvider>> {
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(None);
    };
    let mut archives = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "xp3"))
        .collect::<Vec<_>>();
    archives.sort();
    if archives.is_empty() {
        return Ok(None);
    }
    Xp3ResourceProvider::open_archives(archives)
        .map(Some)
        .map_err(|error| TjsError::runtime(format!("failed to open XP3 archives: {error}")))
}

fn clean_relative_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(TjsError::runtime(format!(
            "storage path must be relative: {}",
            path.display()
        )));
    }

    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(TjsError::runtime(format!(
                    "storage path must stay inside project root: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(clean)
}

fn io_error(error: io::Error) -> TjsError {
    TjsError::runtime(error.to_string())
}
