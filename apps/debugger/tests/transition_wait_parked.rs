//! Regression: the headless shell must leave the engine's transition clock
//! alone, because a game registers its transition wait *around* the call that
//! starts the transition and relies on the completion trigger to fire it.
//!
//! The engine's immediate `TransitionPolicy` — what `krkr-debug` selected by
//! default before this pin — finishes a transition inside the script's
//! `Layer.beginTransition` call, so `onTransitionCompleted`, and with it the
//! `conductor.trigger("trans_<layer>")` a KAGEX game runs from it, fires before
//! the game has registered the wait keys that trigger satisfies.  The keys then
//! stay pending forever: PARQUET's restored title parks at
//! `custom.ks@*title_restore:161` with
//! `wait=[click, click_arg, trans_(object …)_arg, trans_(object …)]` while
//! every screenshot still looks right and every later click is swallowed
//! (M212's finding; reproduced end-to-end in M214's `h3` run and fixed in
//! `h3fix` by leaving the engine default in place).
//!
//! The fixture is the protocol shape rather than the game: a minimal conductor
//! with KAGEX's `wait`/`trigger` keying, and a layer that registers the wait
//! after `beginTransition` returned — the order `SystemTransManager.trans` uses.
//! On the engine's frame clock the 300 ms crossfade ends on frame 18, well
//! after that registration, and the trigger fires it.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

/// The startup script of the scratch project: one layer, one 300 ms transition,
/// and a wait registered after the transition was started.  `probeLog` records
/// the order: `R` the registration, `C` the completion, `W` the trigger firing
/// the wait.
const WAIT_AFTER_BEGIN: &str = r#"
global.probeLog = "";
class ProbeConductor {
    var status = 2; // mWait
    var waitUntil = %[];
    function wait(keys) {
        (Dictionary.assign incontextof waitUntil)(keys);
        global.probeLog += "R";
    }
    function trigger(name) {
        if (status != 2) return 0;
        var handler = waitUntil[name];
        if (handler === void) return 0;
        var arg = waitUntil[name + "_arg"];
        if (arg === void) handler(); else handler(arg);
        (Dictionary.clear incontextof waitUntil)();
        status = 1; // mRun
        global.probeLog += "W";
        return 1;
    }
}
global.probeConductor = new ProbeConductor();
global.probeDest = new Layer(global.probeConductor, null);
global.probeSource = new Layer();
probeDest.visible = true;
probeDest.onTransitionCompleted = function(dest, src) {
    global.probeLog += "C";
    this.window.trigger("trans_" + (string)dest);
};
probeDest.beginTransition("crossfade", true, probeSource, %[time: 300]);
var probeKey = "trans_" + (string)probeDest;
var probeWaits = %[];
probeWaits[probeKey] = function(a) { return a; };
probeWaits[probeKey + "_arg"] = 1;
probeConductor.wait(probeWaits);
"#;

/// `1:` means the conductor left mWait; the sequence after it names what ran
/// (`C` the completion, `W` the trigger that fired the wait).
const STATUS_AND_LOG: &str = r#"probeConductor.status + ":" + probeLog"#;

fn scratch_root(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scratch root");
    fs::write(root.join("Startup.tjs"), WAIT_AFTER_BEGIN).expect("startup script");
    root
}

fn run_probe(root: &PathBuf, extra: &[&str]) -> (String, Option<i32>) {
    let output = Command::new(env!("CARGO_BIN_EXE_krkr-debug"))
        .arg(root)
        .args(["--max-frames", "40", "--quiet", "--expr", STATUS_AND_LOG])
        .args(extra)
        .stdin(Stdio::null())
        .output()
        .expect("run krkr-debug");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (combined, output.status.code())
}

/// The pin: with the shell's default policy the completion reaches a wait that
/// was registered after `beginTransition` returned.
#[test]
fn a_transition_wait_registered_after_begin_transition_still_fires() {
    let root = scratch_root("transition-wait-default");
    let (out, code) = run_probe(&root, &[]);

    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(r#"expression="1:RCW"#),
        "the conductor must leave mWait through the registered wait (`RCW`); output:\n{out}"
    );
}

/// The control that keeps the pin honest: the opt-in immediate policy still
/// finishes the transition inside `beginTransition`, so its completion
/// (`C`) lands *before* the registration (`R`) and the trigger finds no
/// handler — the conductor stays in mWait and the run ends with it parked.
/// This is the documented hazard of the flag (`requested_transition_policy`
/// carries the reference argument); if the engine ever defers immediate
/// completions to the frame clock, this expectation moves with it rather than
/// the flag becoming silently safe.
#[test]
fn the_immediate_policy_flag_reproduces_the_park() {
    let root = scratch_root("transition-wait-immediate");
    let (out, code) = run_probe(&root, &["--immediate-transitions"]);

    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(r#"expression="2:CR"#),
        "the completion must land before the registration and leave the \
         conductor in mWait (`CR`); output:\n{out}"
    );
}
