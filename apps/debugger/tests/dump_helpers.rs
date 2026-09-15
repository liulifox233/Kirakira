//! End-to-end probes for the batch dump helpers, run against the real binary.
//!
//! M177's reviewer lost a completed dump run to `--dump-layer-images <dir>`
//! panicking when `<dir>` did not exist: `thread 'main' panicked ... dump
//! layer image: Os { code: 2, kind: NotFound, ... }` (exit 101), which also
//! aborted the rest of the post-run dump section. The helpers now create
//! their target directory, and a dump that still cannot be written is
//! reported and fails the run instead of panicking.
//!
//! The spawn shape follows `at_script_parked.rs`: the real binary against a
//! synthetic project root under `CARGO_TARGET_TMPDIR`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// A project root with only a startup script: the dispatcher runs it, and the
/// probe needs no game assets.
fn scratch_root(name: &str, startup: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scratch root");
    fs::write(root.join("Startup.tjs"), startup).expect("startup script");
    root
}

/// Runs the built debugger over `root` and returns its combined output and
/// exit code.
fn run_probe(root: &Path, extra: &[&str]) -> (String, Option<i32>) {
    let output = Command::new(env!("CARGO_BIN_EXE_krkr-debug"))
        .arg(root)
        .args(extra)
        .output()
        .expect("run krkr-debug");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (combined, output.status.code())
}

fn assert_png(path: &Path) {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert_eq!(&bytes[0..8], b"\x89PNG\r\n\x1a\n", "{}", path.display());
}

/// `new Layer()` plus `setImageSize`/`fillRect` puts a layer with an image in
/// the tree, which is what `--dump-layer-images` iterates.
const LAYER_STARTUP: &str = r#"
global.__m238layer = new Layer();
__m238layer.setImageSize(4, 2);
__m238layer.fillRect(0, 0, 4, 2, 0xffff0000);
"#;

/// The reviewed panic: dumping into a directory that does not exist aborted
/// the run with `dump layer image: Os { code: 2, ... }` after the frames were
/// already spent. The dump helper must create its target.
#[test]
fn dump_layer_images_creates_a_missing_target_directory() {
    let root = scratch_root("dump-layer-images-missing-dir", LAYER_STARTUP);
    let dir = root.join("dumps").join("nested");
    assert!(!dir.exists(), "the fixture must not create the dump dir");
    let (out, code) = run_probe(
        &root,
        &[
            "--max-frames",
            "5",
            "--quiet",
            "--dump-layer-images",
            dir.to_str().expect("utf-8 scratch path"),
        ],
    );

    assert!(!out.contains("panicked"), "{out}");
    assert!(out.contains("done frames=5"), "{out}");
    let mut pngs: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("target directory was not created: {error}"))
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    pngs.sort();
    assert!(!pngs.is_empty(), "no layer images were written: {out}");
    for png in &pngs {
        assert_png(png);
    }
    assert_eq!(code, Some(0), "{out}");
}

/// A file standing where the directory should be is a deterministic
/// unwritable target: the dump must report it, let the rest of the run's
/// reporting happen, and fail the run instead of panicking.
#[test]
fn dump_layer_images_reports_an_unwritable_target() {
    let root = scratch_root("dump-layer-images-blocked-dir", LAYER_STARTUP);
    let blocked = root.join("blocked");
    fs::write(&blocked, b"a file, not a directory").expect("blocking file");
    let (out, code) = run_probe(
        &root,
        &[
            "--max-frames",
            "5",
            "--quiet",
            "--dump-layer-images",
            blocked.to_str().expect("utf-8 scratch path"),
        ],
    );

    assert!(!out.contains("panicked"), "{out}");
    assert!(out.contains("layer-image error:"), "{out}");
    assert!(out.contains("dump=error"), "{out}");
    assert_eq!(code, Some(1), "{out}");
}

/// `--shot` writes one file rather than a directory, but the same panic
/// aborted the same section when its parent directory did not exist; it must
/// create the target too.
#[test]
fn shot_creates_a_missing_target_directory() {
    let root = scratch_root("shot-missing-dir", LAYER_STARTUP);
    let path = root.join("shots").join("nested").join("frame.png");
    let (out, code) = run_probe(
        &root,
        &[
            "--max-frames",
            "5",
            "--quiet",
            "--shot",
            path.to_str().expect("utf-8 scratch path"),
        ],
    );

    assert!(!out.contains("panicked"), "{out}");
    assert!(out.contains("screenshot="), "{out}");
    assert_png(&path);
    assert_eq!(code, Some(0), "{out}");
}
