//! End-to-end probes for the `textrender` console command.
//!
//! Verifying the message text format's escapes (`\n` breaks, `\k` key waits,
//! style codes) used to mean hand-writing a TJS probe into `interactive` and
//! re-deriving the game's own setup order every time (M130's papercuts);
//! `textrender` renders through the game's TextRender class and dumps the
//! character records the layout produced. The spawn shape follows
//! `at_script_parked.rs`: the real binary against a synthetic project root
//! under `CARGO_TARGET_TMPDIR`, with the console commands piped to stdin.

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

/// A project root with only a startup script: the dispatcher runs it, and the
/// probe needs no game assets.
fn scratch_root(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scratch root");
    fs::write(root.join("Startup.tjs"), "global.__m238ready = 1;").expect("startup script");
    root
}

/// Runs the built debugger interactively and returns its combined output and
/// exit code.
fn run_console(root: &PathBuf, stdin: &str) -> (String, Option<i32>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_krkr-debug"))
        .arg(root)
        .args(["--interactive", "--quiet", "--max-frames", "20"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn krkr-debug");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(stdin.as_bytes())
        .expect("write commands");
    let output = child.wait_with_output().expect("wait for krkr-debug");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (combined, output.status.code())
}

/// The `\n` case the text-format work kept re-deriving: two characters on one
/// logical line are two records, a `\n` break starts a new line, and each
/// record carries the position, size, face and style the layout produced.
#[test]
fn textrender_prints_a_record_per_character_and_the_line_break() {
    let root = scratch_root("textrender-records");
    // The console line carries the two characters backslash-n; the command
    // must pass them verbatim (a TJS literal would fold them into a newline).
    let (out, code) = run_console(&root, "textrender --size 48 A\\nB\nq\n");

    assert!(!out.contains("panicked"), "{out}");
    assert!(
        out.contains(
            "interactive textrender class=TextRenderBase size=48 width=800 text=\"A\\\\nB\" renderLines=2 renderCount=2"
        ),
        "{out}"
    );
    assert!(
        out.contains("textrender char[0] text=\"A\" x=0 y=0 line=0 size=48"),
        "{out}"
    );
    assert!(
        out.contains("textrender char[1] text=\"B\" x=0 y=54 line=1 size=48"),
        "{out}"
    );
    assert_eq!(code, Some(0), "{out}");
}

/// The key-wait surface the game's typewriter drives: `\k` must show up in
/// `getKeyWait()` with the character position it waits after, and a class the
/// project does not define must be reported by name without ending the
/// session.
#[test]
fn textrender_reports_key_waits_and_a_missing_class() {
    let root = scratch_root("textrender-keywait");
    let (out, code) = run_console(
        &root,
        "textrender --size 48 A\\kC\nmembers -f _nothing global\nq\n",
    );

    assert!(out.contains("renderCount=2"), "{out}");
    assert!(out.contains("textrender keywait[0] pos=1"), "{out}");
    assert_eq!(code, Some(0), "{out}");

    let (out, _) = run_console(&root, "textrender --class NoSuchClass hi\nq\n");
    assert!(
        out.contains("interactive textrender_error class=NoSuchClass"),
        "{out}"
    );
    assert!(out.contains("does not exist"), "{out}");
}
