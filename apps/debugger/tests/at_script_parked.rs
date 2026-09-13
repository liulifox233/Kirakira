//! End-to-end probes for `--at-frame`/`--at-script` while the VM is parked.
//!
//! `src/inject.rs` unit tests cover the queueing rule; these run the real
//! binary against a synthetic project root whose startup script parks the VM
//! on a timer (`Window.showModal()`), the deterministic form of the
//! resource-load window the injection used to slip into. The startup timer
//! fires on the virtual clock (500 ms of 16.7 ms frames = frame 30), so a
//! probe injected at frame 40 always meets a parked VM.

use std::{fs, path::PathBuf, process::Command};

/// A project root with only a startup script: the dispatcher runs it, and the
/// probe needs no game assets.
fn scratch_root(name: &str, startup: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scratch root");
    fs::write(root.join("Startup.tjs"), startup).expect("startup script");
    root
}

/// Runs the built debugger over `root` and returns its output and exit code.
fn run_probe(root: &PathBuf, extra: &[&str]) -> (String, Option<i32>) {
    let output = Command::new(env!("CARGO_BIN_EXE_krkr-debug"))
        .arg(root)
        .args(["--max-frames", "60", "--quiet"])
        .args(extra)
        .output()
        .expect("run krkr-debug");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (combined, output.status.code())
}

const PARKING_STARTUP: &str = r#"
global.__m181t = new Timer(function() {
    global.__m181modal = new Window();
    __m181modal.showModal();
}, "");
__m181t.interval = 500;
__m181t.enabled = true;
"#;

const RUNNING_STARTUP: &str = "global.__m181ready = 1;";

/// The regression: with the VM parked at frame 40 the injection used to be
/// printed as executed (`executing at frame=40`) while doing nothing, and the
/// run exited 0 with `expression=void`. The injection must wait for the VM,
/// and a request that never gets a running frame must fail the run.
#[test]
fn a_parked_vm_waits_for_the_script_and_fails_if_it_never_runs() {
    let root = scratch_root("at-script-parked", PARKING_STARTUP);
    let (out, code) = run_probe(
        &root,
        &[
            "--at-frame",
            "40",
            "--at-script",
            r#"global.__m181mark = "RAN";"#,
            "--expr",
            "global.__m181mark",
        ],
    );

    assert!(out.contains("at-frame script frame=40 deferred"), "{out}");
    assert!(
        out.contains("script for frame=40 never ran: vm-suspended"),
        "{out}"
    );
    assert!(out.contains("expression_error=vm-suspended"), "{out}");
    assert!(out.contains("injection=error"), "{out}");
    assert!(!out.contains("expression=\"RAN\""), "{out}");
    assert_eq!(code, Some(1), "{out}");
}

/// A request the run never reaches is a miss too, but its cause is the frame
/// budget, not a parked VM: `--at-frame 100` under `--max-frames 60` on a root
/// whose VM never parks must say so (the reviewed P2 -- the message used to
/// claim `vm-suspended` without checking the VM).
#[test]
fn a_request_the_run_never_reaches_names_the_frame_budget() {
    let root = scratch_root("at-script-budget", RUNNING_STARTUP);
    let (out, code) = run_probe(
        &root,
        &[
            "--at-frame",
            "100",
            "--at-script",
            r#"global.__m181mark = "RAN";"#,
            "--expr",
            "global.__m181mark",
        ],
    );

    assert!(out.contains("script for frame=100 never ran"), "{out}");
    assert!(
        out.contains("the frame budget (--max-frames 60) ended first"),
        "{out}"
    );
    assert!(
        !out.contains("vm-suspended"),
        "the budget miss must not blame the VM: {out}"
    );
    assert!(out.contains("injection=error"), "{out}");
    assert_eq!(code, Some(1), "{out}");
}

/// The ordinary path is unchanged: a VM that runs at the requested frame
/// injects there, prints the exact old line, and the run succeeds.
#[test]
fn a_running_vm_injects_at_the_requested_frame() {
    let root = scratch_root("at-script-running", RUNNING_STARTUP);
    let (out, code) = run_probe(
        &root,
        &[
            "--at-frame",
            "40",
            "--at-script",
            r#"global.__m181mark = "RAN";"#,
            "--expr",
            "global.__m181mark",
        ],
    );

    assert!(out.contains("executing at frame=40"), "{out}");
    assert!(out.contains("expression=\"RAN\""), "{out}");
    assert!(!out.contains("injection=error"), "{out}");
    assert_eq!(code, Some(0), "{out}");
}
