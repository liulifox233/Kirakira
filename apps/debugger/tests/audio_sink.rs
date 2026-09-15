//! End-to-end probes for the audio sink the headless harness reports.
//!
//! The run used to hand `RuntimeSession` a `VirtualAudioSink` unconditionally
//! (audio never decoded, and the summary said nothing), and the round-1 review
//! then caught the converse lie: a real sink whose backend died on its own
//! thread produced a closing line byte-identical to a healthy run that simply
//! played nothing. These run the real binary against a synthetic root and pin
//! the three distinguishable verdicts.
//!
//! The backend's failure is forced with `ALSA_CONFIG_PATH`, so the probes do
//! not depend on whether the host has an output device: a bogus path makes
//! alsa-lib fail to open anything, and the documented null-PCM config
//! (`docs/DEBUGGING.md`) gives a healthy device that renders nothing.
//!
//! That lever is Linux-only — it is the ALSA backend's own environment, and
//! the cpal backends on macOS/Windows do not consult it, so on a host with a
//! device the "dead backend" probe would run as a *healthy* one and fail. The
//! two ALSA probes are therefore `cfg(target_os = "linux")`; what covers the
//! rest of the platform matrix is the verdict-text unit test
//! (`the_audio_verdict_names_the_sink_it_actually_had`, which pins all four
//! outcomes including the reported-error one) plus the ungated probe below,
//! which pins that `--virtual-audio` still selects and reports the silent
//! sink. The macOS/Windows wiring itself (status event -> `audio_last_report`
//! -> verdict) is not exercised by an automated test here, and this branch
//! claims no run on those hosts.

use std::{fs, path::PathBuf, process::Command};

/// A project root with only a startup script: the dispatcher runs it, and the
/// probe needs no game assets.
fn scratch_root(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scratch root");
    fs::write(root.join("Startup.tjs"), "global.__m238ready = 1;").expect("startup script");
    root
}

/// Runs the built debugger over `root` with `ALSA_CONFIG_PATH` set and returns
/// its combined output.
fn run_probe(root: &PathBuf, alsa_config: Option<&PathBuf>, extra: &[&str]) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_krkr-debug"));
    command
        .arg(root)
        .args(["--max-frames", "5", "--quiet"])
        .args(extra);
    if let Some(config) = alsa_config {
        command.env("ALSA_CONFIG_PATH", config);
    }
    let output = command.output().expect("run krkr-debug");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    combined
}

/// The verdict line of the run (`audio decoded ...`).
fn verdict(output: &str) -> &str {
    output
        .lines()
        .find(|line| line.starts_with("audio decoded "))
        .unwrap_or_else(|| panic!("no audio verdict in: {output}"))
}

/// A backend that dies on its own thread (no output device) is named: the
/// closing line carries the sink's own report instead of reading like a
/// healthy run that played nothing.
#[cfg(target_os = "linux")]
#[test]
fn a_dead_backend_is_named_in_the_verdict() {
    let root = scratch_root("audio-dead-backend");
    let output = run_probe(
        &root,
        Some(&PathBuf::from("/nonexistent-m238-alsa.conf")),
        &[],
    );

    assert!(output.contains("audio sink=system (decoding)"), "{output}");
    assert!(output.contains("audio status=Error"), "{output}");
    let verdict = verdict(&output);
    assert!(
        verdict.contains("(the sink reported:"),
        "a dead backend must not look like a silent game: {verdict}"
    );
}

/// A healthy device that renders nothing says exactly that — no error claim —
/// so the two silent-looking runs cannot be confused.
#[cfg(target_os = "linux")]
#[test]
fn a_healthy_silent_sink_says_only_what_it_decoded() {
    let root = scratch_root("audio-healthy-silent");
    let config = root.join("asound-null.conf");
    fs::write(
        &config,
        "pcm.!default { type null }\nctl.!default { type null }\n",
    )
    .expect("null ALSA config");
    let output = run_probe(&root, Some(&config), &[]);

    let verdict = verdict(&output);
    assert_eq!(
        verdict, "audio decoded instances=0 rendered_frames=0",
        "a healthy sink that played nothing must not claim an error: {output}"
    );
}

/// `--virtual-audio` is the operator's own choice and keeps its own line, so
/// the three ways a run can decode nothing are distinguishable.
#[test]
fn the_silent_sink_is_reported_as_the_choice_it_is() {
    let root = scratch_root("audio-virtual-sink");
    let output = run_probe(&root, None, &["--virtual-audio"]);

    assert!(
        output.contains("audio sink=virtual (silent:"),
        "the chosen sink must be named: {output}"
    );
    assert!(
        verdict(&output).contains("(virtual sink: nothing decodes)"),
        "{output}"
    );
}
