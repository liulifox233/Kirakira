use std::{
    fs::File,
    io,
    path::{Component, Path, PathBuf},
};

use krkr_core::{ResourceProvider, ResourceStream};
use rfd::{MessageButtons, MessageDialog, MessageLevel};

#[derive(Clone, Debug)]
pub struct FsResourceProvider {
    root: PathBuf,
}

impl FsResourceProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve(&self, path: &str) -> io::Result<PathBuf> {
        let path = Path::new(path);
        if path.is_absolute() {
            return Err(invalid_resource_path(path));
        }

        let mut clean = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => clean.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                    return Err(invalid_resource_path(path));
                }
            }
        }

        Ok(self.root.join(clean))
    }
}

impl ResourceProvider for FsResourceProvider {
    fn open(&self, path: &str) -> io::Result<Box<dyn ResourceStream>> {
        let file = File::open(self.resolve(path)?)?;
        Ok(Box::new(file))
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_ok_and(|path| path.is_file())
    }
}

pub fn pick_folder() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_folder()
}

pub fn show_error(title: &str, message: &str) {
    show_message(MessageLevel::Error, title, message);
}

pub fn show_warning(title: &str, message: &str) {
    show_message(MessageLevel::Warning, title, message);
}

fn show_message(level: MessageLevel, title: &str, message: &str) {
    let _result = MessageDialog::new()
        .set_level(level)
        .set_title(title)
        .set_description(message)
        .set_buttons(MessageButtons::Ok)
        .show();
}

fn invalid_resource_path(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("resource path must be relative and stay inside the root: {path:?}"),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Read,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn fs_provider_reads_files_under_root() {
        let root = temp_root();
        fs::create_dir_all(root.join("scenario")).expect("create temp dir");
        fs::write(root.join("scenario/start.ks"), b"hello").expect("write fixture");
        let provider = FsResourceProvider::new(&root);

        let mut stream = provider.open("scenario/start.ks").expect("open fixture");
        let mut contents = String::new();
        stream
            .read_to_string(&mut contents)
            .expect("read fixture as UTF-8");

        assert_eq!(contents, "hello");
        assert!(provider.exists("scenario/start.ks"));
        assert!(!provider.exists("missing.ks"));

        fs::remove_dir_all(root).expect("remove temp dir");
    }

    #[test]
    fn fs_provider_rejects_parent_traversal() {
        let provider = FsResourceProvider::new(temp_root());

        assert!(provider.resolve("../outside.ks").is_err());
        assert!(provider.resolve("scenario/../../outside.ks").is_err());
    }

    fn temp_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("krkr-ruri-platform-{}-{nanos}", std::process::id()))
    }
}
