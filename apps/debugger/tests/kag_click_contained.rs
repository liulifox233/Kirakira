//! Regression: the semantic click must dispose of a raising click handler the
//! way the engine's own event dispatch does.
//!
//! `--kag-click` / `--kag-auto-click` used to evaluate
//! `conductor.trigger("click")` as a plain TJS expression, so an exception
//! escaping the game's click handler was reported as a debugger error and the
//! click was dropped — while the engine delivers the same click through
//! `fire_kag_primary_click` -> `call_event_method`
//! (`crates/krkr-engine/src/engine.rs:2552`, `:2364-2403`), whose boundary
//! hands the exception to `System.exceptionHandler`, logs, and lets the frame
//! carry on.  On PARQUET every auto-click re-raised the game's own
//! `InvalidParam` and the run sat at `start.ks@*envplay:28` emitting one
//! debugger error per click for thousands of frames (M220's follow-up; M223).
//!
//! The fixture is the smallest KAGEX-shaped click path: `kag.onPrimaryClick`
//! (the entry the engine posts a primary click to), a conductor parked on a
//! `click` wait, and a handler that raises.  The engine's own coordinate click
//! is run as the pairing — it is the disposition the semantic click must
//! inherit.
//!
//! What a fixture cannot reach is the real game: it cannot show that
//! PARQUET's own handler aborts mid-render or that its project handler is the
//! one being consulted.  The live run covers that — the 800-frame probe's
//! `expression="213|Invalid argument"` (the game's `System.exceptionHandler`
//! saw the raise 213 times, 0 times before the fix) and the full run's 2002
//! boundary logs naming `onMouseDown` with `error: Invalid argument`.

use std::{fs, path::PathBuf, process::Command};

/// A conductor parked on a `click` wait, behind the engine's primary-click
/// entry, whose click handler raises the way the real game's does.
const RAISING_CLICK_STARTUP: &str = r#"
global.__clickRan = 0;
global.__handlerCalls = 0;

System.exceptionHandler = function(e) {
    global.__handlerCalls++;
    return true;
};

class ProbeConductor {
    var status = 2; // mWait
    var mRun = 1;
    var mStop = 0;
    var mWait = 2;
    var curLine = 7;
    var waitUntil = %[];
    function trigger(name) {
        if (status != mWait) return false;
        var handler = waitUntil[name];
        if (handler === void) return false;
        handler();
        (Dictionary.clear incontextof waitUntil)();
        status = mRun;
        return true;
    }
}

class ProbeKAG {
    var conductor;
    var currentStorage = "probe.ks";
    var currentLabel = "*click";
    var inSleep = 0;
    function ProbeKAG() {
        this.conductor = new ProbeConductor();
    }
    function onPrimaryClick() {
        global.__clickRan++;
        this.conductor.trigger("click");
    }
}

global.kag = new ProbeKAG();
kag.conductor.waitUntil["click"] = function() {
    throw new Exception("probe click handler failure");
};
"#;

/// `handlerCalls:clickRan:conductorStatus`. The handler raising leaves the
/// conductor parked (its `trigger` aborts before clearing the wait), which is
/// the game's own script abort — what containment changes is the exception's
/// disposition, not the handler's continuation.
const CLICK_STATE: &str =
    "global.__handlerCalls + ':' + global.__clickRan + ':' + global.kag.conductor.status";

fn scratch_root(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scratch root");
    fs::write(root.join("Startup.tjs"), RAISING_CLICK_STARTUP).expect("startup script");
    root
}

/// The `[s]` shape: the conductor asleep (KAGEX's `s` handler sets `inSleep`
/// and stops without a wait), and the game's own `waitClick` as the only way
/// to build the wait a wake can satisfy.
const SLEEPING_CONDUCTOR_STARTUP: &str = r#"
global.__sleepWoke = 0;
global.__primaryClickRan = 0;

class SleepConductor {
    var status = 0; // mStop
    var mRun = 1;
    var mStop = 0;
    var mWait = 2;
    var curLine = 35;
    var waitUntil = %[];
    function trigger(name) {
        if (status != mWait) return false;
        var handler = waitUntil[name];
        if (handler === void) return false;
        handler();
        (Dictionary.clear incontextof waitUntil)();
        status = mRun;
        return true;
    }
}

class SleepKAG {
    var conductor;
    var currentStorage = "title.ks";
    var currentLabel = "*wait";
    var inSleep = 1;
    function SleepKAG() {
        this.conductor = new SleepConductor();
    }
    function waitClick(elm) {
        this.conductor.waitUntil["click"] = function() {
            global.__sleepWoke = 1;
        };
        this.conductor.status = this.conductor.mWait;
    }
    function onPrimaryClick() {
        global.__primaryClickRan = 1;
    }
}

global.kag = new SleepKAG();
"#;

fn sleep_scratch_root(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scratch root");
    fs::write(root.join("Startup.tjs"), SLEEPING_CONDUCTOR_STARTUP).expect("startup script");
    root
}

fn run_probe(root: &PathBuf, extra: &[&str]) -> (String, Option<i32>) {
    let output = Command::new(env!("CARGO_BIN_EXE_krkr-debug"))
        .arg(root)
        .args(["--max-frames", "20", "--quiet", "--logs"])
        .args(extra)
        .output()
        .expect("run krkr-debug");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (combined, output.status.code())
}

/// The semantic click arms the engine's primary click, so the game's handler
/// runs under `call_event_method`'s boundary: `System.exceptionHandler` sees
/// the raise, the frame carries on, and the run does not end on it.
#[test]
fn the_semantic_click_contains_a_raising_handler_the_way_the_engine_does() {
    let root = scratch_root("kag-click-contained");
    let (out, code) = run_probe(&root, &["--kag-click", "5", "--expr", CLICK_STATE]);

    assert!(
        out.contains("kag-click frame=5 -> engine primary click"),
        "{out}"
    );
    assert!(
        !out.contains("kag-click frame=5 error:"),
        "the click must not end as a debugger error: {out}"
    );
    // The handler was consulted (1) and the click reached it (1); the
    // conductor is still parked because the fixture's own `trigger` aborted
    // at the raise, exactly as the game's does.
    assert!(out.contains("expression=\"1:1:2\""), "{out}");
    assert!(out.contains("event `onPrimaryClick`"), "{out}");
    assert_eq!(code, Some(0), "{out}");
}

/// The pairing: the engine's own coordinate click over the same handler. A
/// click with no layer under the cursor is posted to `kag.onPrimaryClick`
/// (`fire_kag_primary_click`, engine.rs:2552), which is the disposition the
/// semantic click now inherits.
#[test]
fn the_engines_own_click_dispatch_is_the_pairing_for_that_disposition() {
    let root = scratch_root("kag-click-engine-pairing");
    let (out, code) = run_probe(&root, &["--click", "5,64,64", "--expr", CLICK_STATE]);

    assert!(out.contains("expression=\"1:1:2\""), "{out}");
    assert!(out.contains("event `onPrimaryClick`"), "{out}");
    assert_eq!(code, Some(0), "{out}");
}

/// A run driven by `--kag-auto-click` keeps clicking while the handler keeps
/// raising: the engine contains each raise and the frame loop carries on,
/// which is exactly the property the PARQUET prologue run lost.
#[test]
fn an_auto_click_run_keeps_clicking_when_the_handler_keeps_raising() {
    let root = scratch_root("kag-auto-click-contained");
    let (out, code) = run_probe(
        &root,
        &["--kag-auto-click", "--expr", "global.__handlerCalls"],
    );

    let handled: i64 = out
        .lines()
        .find_map(|line| line.strip_prefix("expression="))
        .expect("expression line")
        .trim_matches('"')
        .parse()
        .expect("handler call count");
    assert_eq!(handled, 20, "one contained raise per frame: {out}");
    assert!(
        !out.contains("error: Runtime error: uncaught exception"),
        "no click may escape as a run error: {out}"
    );
    assert_eq!(code, Some(0), "{out}");
}

/// The `[s]` sleep wake is the one action the engine's input dispatch has no
/// entry for (a primary click ends at `kag.onPrimaryClick`, and KAGEX's own
/// handler only acts while the conductor waits), so it stays the game-side
/// synthesis: the game's `waitClick` builds the wait and the click fires it.
/// The wake must survive — `--kag-click` on a sleeping title is how PARQUET's
/// `title.ks@*wait:35` and 少女世界的生存之道's `title.ks:35 → 36 → 40` advance.
#[test]
fn the_sleep_wake_still_runs_the_games_own_waitclick_synthesis() {
    let root = sleep_scratch_root("kag-click-sleep-wake");
    let (out, code) = run_probe(
        &root,
        &[
            "--kag-click",
            "5",
            "--expr",
            "global.__sleepWoke + ':' + global.kag.conductor.status + ':' + global.__primaryClickRan",
        ],
    );

    assert!(
        out.contains("kag-click frame=5 -> \"wake-sleep title.ks:35\""),
        "{out}"
    );
    // Woken (1) and running (mRun); the engine primary click is not the entry
    // for this state, so `onPrimaryClick` must not have been posted.
    assert!(out.contains("expression=\"1:1:0\""), "{out}");
    assert_eq!(code, Some(0), "{out}");
}

/// The pre-fix `--kag-click` on a *click wait* becomes the engine's click, so
/// the printed line names the engine (the sleep line above is unchanged).
#[test]
fn a_click_wait_is_never_woken_by_the_harness_itself() {
    let root = scratch_root("kag-click-wait-via-engine");
    let (out, _code) = run_probe(&root, &["--kag-click", "5", "--expr", CLICK_STATE]);

    assert!(out.contains("-> engine primary click"), "{out}");
    assert!(!out.contains("wake-sleep"), "{out}");
}
