use std::time::Duration;

use krkr_kag::Tag;
use krkr_tjs2::{Result, runtime::Runtime};

use crate::{EngineFrame, EngineInput, host::KrkrHost};

/// Hook access to engine state during lifecycle callbacks.
///
/// This API is intentionally narrower than direct engine access: hooks are
/// meant for instrumentation and policy injection, not arbitrary runtime
/// mutation.
pub struct EngineHookContext<'a> {
    runtime: &'a mut Runtime<KrkrHost>,
}

impl<'a> EngineHookContext<'a> {
    pub(crate) fn new(runtime: &'a mut Runtime<KrkrHost>) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &Runtime<KrkrHost> {
        self.runtime
    }

    pub fn host(&self) -> &KrkrHost {
        self.runtime.host()
    }

    pub fn host_mut(&mut self) -> &mut KrkrHost {
        self.runtime.host_mut()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KagTagDecision {
    Continue,
    /// Skip native/TJS handling for this tag and continue with the next tag.
    ///
    /// When a hook returns `Skip`, later hooks do not receive
    /// `before_kag_tag` for the same tag and no hook receives `after_kag_tag`
    /// for it.
    Skip,
}

/// Engine lifecycle hook for instrumentation and host policy injection.
///
/// Hooks run in registration order. Any error aborts the current engine
/// operation and is returned to the caller.
pub trait EngineHook {
    fn on_register(&mut self, _ctx: &mut EngineHookContext<'_>) -> Result<()> {
        Ok(())
    }

    /// Called at the start of `KrkrEngine::update`, before input events are
    /// posted into the scheduler.
    fn before_update(
        &mut self,
        _ctx: &mut EngineHookContext<'_>,
        _input: &mut EngineInput,
        _delta: Duration,
    ) -> Result<()> {
        Ok(())
    }

    /// Called before a KAG tag is handled.
    ///
    /// Returning `Skip` prevents further hook processing for this tag and
    /// suppresses both TJS/native tag handling and `after_kag_tag`.
    fn before_kag_tag(
        &mut self,
        _ctx: &mut EngineHookContext<'_>,
        _tag: &Tag,
    ) -> Result<KagTagDecision> {
        Ok(KagTagDecision::Continue)
    }

    /// Called after a KAG tag has been executed through either TJS or native
    /// handling.
    ///
    /// This callback is not invoked for tags skipped by `before_kag_tag`.
    fn after_kag_tag(&mut self, _ctx: &mut EngineHookContext<'_>, _tag: &Tag) -> Result<()> {
        Ok(())
    }

    /// Called after the frame has been built, allowing hooks to adjust final
    /// output presentation for the current app.
    fn after_frame(
        &mut self,
        _ctx: &mut EngineHookContext<'_>,
        _frame: &mut EngineFrame,
    ) -> Result<()> {
        Ok(())
    }
}

impl<F> EngineHook for F
where
    F: FnMut(&mut EngineHookContext<'_>, &mut EngineFrame) -> Result<()>,
{
    fn after_frame(
        &mut self,
        ctx: &mut EngineHookContext<'_>,
        frame: &mut EngineFrame,
    ) -> Result<()> {
        self(ctx, frame)
    }
}
