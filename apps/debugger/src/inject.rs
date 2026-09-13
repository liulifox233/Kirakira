//! `--at-frame`/`--at-script` injection and the parked-VM contract.
//!
//! A parked VM (a call stack suspended on a pending resource or on a modal
//! window) makes every fresh top-level evaluation return `void` immediately
//! instead of running it — the effect `--watch-expr` documents in `main.rs`
//! and the interactive console reports as [`VM_SUSPENDED`]. Reporting that
//! `void` as "the script executed" is a lie: the probe's injection silently
//! never happens, and the run still exits 0.
//!
//! So injection never enters a parked VM. An `--at-frame N --at-script`
//! request whose frame arrives while the VM is parked stays queued and runs
//! on the first frame where the VM runs again, in request order, printing the
//! frame it actually ran at. A request that never runs before the run ends is
//! reported by [`AtFrameScripts::unfinished`] with the cause that applies --
//! the park outlived the run, or the frame budget ended before the frame
//! arrived -- and fails the run. The interactive console handles the same
//! situation by handing its operator the diagnosis and letting them advance
//! frames and retry; this is the batch-tool form of that retry.

use krkr_engine::KrkrEngine;

/// The parked-VM diagnosis, worded once so every surface names the same cause
/// in the same words: the console prints it as
/// `interactive expression_error=...`, the batch probe as
/// `expression_error=...` or an `at-frame` deferral notice.
pub const VM_SUSPENDED: &str =
    "vm-suspended (a call stack is parked on a pending resource; advance frames and retry)";

/// True while a fresh top-level evaluation would return `void` instead of
/// running: the VM is parked and `execute_script`/`execute_expression` cannot
/// be trusted to have done anything.
pub fn parked(engine: &KrkrEngine) -> bool {
    engine.is_script_suspended()
}

/// Result of one `--at-frame`/`--at-script` request that the frame boundary
/// settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtScriptOutcome {
    /// The script executed at `frame`. `deferred` marks a request whose own
    /// frame arrived while the VM was parked and which ran later.
    Ran {
        requested_frame: usize,
        frame: usize,
        deferred: bool,
    },
    /// The VM is parked, so the script stays queued for a later frame.
    /// Reported once per request, so a long park does not repeat the notice
    /// every frame.
    Deferred { requested_frame: usize },
}

struct AtScriptRequest {
    frame: usize,
    script: String,
    done: bool,
    deferral_reported: bool,
}

/// An `--at-frame`/`--at-script` request the run ended without running, and
/// the cause the message must name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtScriptUnfinished {
    /// The requested frame arrived while the VM was parked, and the VM never
    /// gave the script a running frame again -- the park outlived the run.
    VmStayedParked { requested_frame: usize },
    /// The run ended before the requested frame was reached; no park was ever
    /// involved. The frame budget is the cause to name, not the VM.
    BudgetEnded { requested_frame: usize },
}

/// The `--at-frame`/`--at-script` queue with its parked-VM deferral.
pub struct AtFrameScripts {
    requests: Vec<AtScriptRequest>,
}

impl AtFrameScripts {
    /// Keeps the command-line order of the pairs: a deferred request runs
    /// before any request declared after it, so an injection never overtakes
    /// an earlier one.
    pub fn new(requests: Vec<(usize, String)>) -> Self {
        Self {
            requests: requests
                .into_iter()
                .map(|(frame, script)| AtScriptRequest {
                    frame,
                    script,
                    done: false,
                    deferral_reported: false,
                })
                .collect(),
        }
    }

    /// Settles every request due at `frame` — earlier deferrals first, then
    /// this frame's own — while the VM runs. Returns the outcomes worth
    /// reporting. While the VM is parked nothing runs and the queue keeps its
    /// order.
    pub fn inject(&mut self, engine: &mut KrkrEngine, frame: usize) -> Vec<AtScriptOutcome> {
        let mut outcomes = Vec::new();
        for request in &mut self.requests {
            if request.done || request.frame > frame {
                continue;
            }
            if parked(engine) {
                if !request.deferral_reported {
                    request.deferral_reported = true;
                    outcomes.push(AtScriptOutcome::Deferred {
                        requested_frame: request.frame,
                    });
                }
                // No request can run while the VM is parked; the queue stays
                // untouched, so request order survives the wait.
                continue;
            }
            engine
                .execute_script("krkr_debug_at_frame.tjs", &request.script)
                .expect("at-frame script");
            request.done = true;
            outcomes.push(AtScriptOutcome::Ran {
                requested_frame: request.frame,
                frame,
                deferred: request.frame != frame,
            });
        }
        outcomes
    }

    /// The requests still waiting when the run ends, each with the cause that
    /// actually applies. The caller reports them and fails the run.
    pub fn unfinished(&self) -> impl Iterator<Item = AtScriptUnfinished> + '_ {
        self.requests
            .iter()
            .filter(|request| !request.done)
            .map(|request| {
                if request.deferral_reported {
                    AtScriptUnfinished::VmStayedParked {
                        requested_frame: request.frame,
                    }
                } else {
                    AtScriptUnfinished::BudgetEnded {
                        requested_frame: request.frame,
                    }
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krkr_core::{FrameInput, Size};
    use krkr_engine::{EngineConfig, EngineInput, KrkrEngine};
    use krkr_tjs2::runtime::Variant;
    use std::time::Duration;

    /// Parks the VM the way a resource-load window does: a modal window call
    /// suspends the running call stack, and every fresh top-level evaluation
    /// returns `void` without running anything.
    fn parked_engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "m181-park.tjs",
                r#"
                global.__m181trace = "";
                global.__m181modal = new Window();
                __m181modal.showModal();
                "#,
            )
            .expect("park the VM");
        assert!(parked(&engine), "fixture must park the VM");
        engine
    }

    fn resume(engine: &mut KrkrEngine) {
        let modal = engine
            .tjs_runtime()
            .global_member("__m181modal")
            .object_handle()
            .expect("modal object");
        engine
            .tjs_runtime_mut()
            .call_object_method(modal, "close", Vec::new())
            .expect("close modal");
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("resume frame");
        assert!(!parked(engine), "fixture must resume the VM");
    }

    fn trace(engine: &KrkrEngine) -> Variant {
        engine.tjs_runtime().global_member("__m181trace")
    }

    /// The regression: today a parked VM swallows the injected script — the
    /// call returns `void`, the assignment never happens, and the run still
    /// reports success. The queue must instead keep the request and run it
    /// once the VM resumes.
    #[test]
    fn a_parked_vm_defers_the_at_frame_script_instead_of_dropping_it() {
        let mut engine = parked_engine();
        let mut scripts =
            AtFrameScripts::new(vec![(5, r#"global.__m181trace += "A";"#.to_string())]);

        let outcomes = scripts.inject(&mut engine, 5);
        assert_eq!(
            outcomes,
            vec![AtScriptOutcome::Deferred { requested_frame: 5 }]
        );
        assert_eq!(trace(&engine), Variant::String(String::new()));
        assert_eq!(
            scripts.unfinished().collect::<Vec<_>>(),
            vec![AtScriptUnfinished::VmStayedParked { requested_frame: 5 }]
        );

        // A second parked frame repeats nothing (one notice per request).
        assert!(scripts.inject(&mut engine, 6).is_empty());
        assert_eq!(
            scripts.unfinished().collect::<Vec<_>>(),
            vec![AtScriptUnfinished::VmStayedParked { requested_frame: 5 }]
        );

        resume(&mut engine);
        let outcomes = scripts.inject(&mut engine, 7);
        assert_eq!(
            outcomes,
            vec![AtScriptOutcome::Ran {
                requested_frame: 5,
                frame: 7,
                deferred: true,
            }]
        );
        assert_eq!(trace(&engine), Variant::String("A".to_string()));
        assert_eq!(scripts.unfinished().count(), 0);
    }

    /// The unfinished reason must be the cause that actually applies: a miss
    /// whose frame arrived while parked is the park; a miss whose frame the
    /// run never reached is the frame budget (the reviewed P2: the budget case
    /// was reported as `vm-suspended` without checking the VM at all).
    #[test]
    fn the_unfinished_reason_names_the_park_or_the_frame_budget() {
        let mut engine = parked_engine();
        let mut scripts = AtFrameScripts::new(vec![
            (5, r#"global.__m181trace += "A";"#.to_string()),
            (50, r#"global.__m181trace += "B";"#.to_string()),
        ]);

        // Frame 5 arrives while the VM is parked: that miss is the park.
        assert_eq!(
            scripts.inject(&mut engine, 5),
            vec![AtScriptOutcome::Deferred { requested_frame: 5 }]
        );
        // The run then ends at frame 20, before frame 50 ever arrives.
        assert_eq!(
            scripts.unfinished().collect::<Vec<_>>(),
            vec![
                AtScriptUnfinished::VmStayedParked { requested_frame: 5 },
                AtScriptUnfinished::BudgetEnded {
                    requested_frame: 50
                },
            ]
        );
    }

    /// A deferred request keeps its place in the queue: once the VM resumes,
    /// everything waiting runs in request order — a request for an earlier
    /// frame never runs after a later one, and nothing overtakes the wait.
    #[test]
    fn deferred_scripts_run_in_request_order_after_the_vm_resumes() {
        let mut engine = parked_engine();
        let mut scripts = AtFrameScripts::new(vec![
            (5, r#"global.__m181trace += "A";"#.to_string()),
            (6, r#"global.__m181trace += "B";"#.to_string()),
        ]);

        assert_eq!(
            scripts.inject(&mut engine, 5),
            vec![AtScriptOutcome::Deferred { requested_frame: 5 }]
        );
        assert_eq!(
            scripts.inject(&mut engine, 6),
            vec![AtScriptOutcome::Deferred { requested_frame: 6 }]
        );
        assert_eq!(trace(&engine), Variant::String(String::new()));

        resume(&mut engine);
        assert_eq!(
            scripts.inject(&mut engine, 8),
            vec![
                AtScriptOutcome::Ran {
                    requested_frame: 5,
                    frame: 8,
                    deferred: true,
                },
                AtScriptOutcome::Ran {
                    requested_frame: 6,
                    frame: 8,
                    deferred: true,
                },
            ]
        );
        assert_eq!(trace(&engine), Variant::String("AB".to_string()));
    }

    /// A script that runs while the VM is running reports its own frame, not
    /// a deferral: the ordinary path keeps its exact output.
    #[test]
    fn a_running_vm_runs_the_at_frame_script_at_its_own_frame() {
        let mut engine = parked_engine();
        resume(&mut engine);
        let mut scripts =
            AtFrameScripts::new(vec![(5, r#"global.__m181trace += "A";"#.to_string())]);

        assert_eq!(
            scripts.inject(&mut engine, 5),
            vec![AtScriptOutcome::Ran {
                requested_frame: 5,
                frame: 5,
                deferred: false,
            }]
        );
        assert_eq!(trace(&engine), Variant::String("A".to_string()));
    }
}
