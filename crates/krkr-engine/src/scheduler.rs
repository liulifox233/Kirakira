use std::collections::{BTreeMap, BTreeSet, VecDeque};

use krkr_core::EngineEvent;
use krkr_tjs2::runtime::{ObjectHandle, Variant};

pub(crate) const TIMER_EVENT_NAME: &str = "onTimer";
pub(crate) const ASYNC_TRIGGER_EVENT_NAME: &str = "onFire";
pub(crate) const AUDIO_FADE_COMPLETED_EVENT_NAME: &str = "onFadeCompleted";

#[derive(Clone, Debug, Default)]
pub(crate) struct TvpScheduler {
    script_events: VecDeque<ScriptEvent>,
    input_events: VecDeque<EngineEvent>,
    window_update_events: VecDeque<ObjectHandle>,
    timers: BTreeMap<ObjectHandle, TimerState>,
    idle_async_triggers: BTreeMap<ObjectHandle, usize>,
    audio_fade_completions: BTreeMap<ObjectHandle, i64>,
    continuous_handlers: Vec<Variant>,
    next_sequence: u64,
    sequence_to_process: u64,
    event_disabled: bool,
    frame_continuous_delivered: bool,
    frame_idle_async_delivered: BTreeSet<ObjectHandle>,
    window_updates_delivering: bool,
    active_window_update: Option<ObjectHandle>,
    delivered_window_update_counts: BTreeMap<ObjectHandle, usize>,
}

impl TvpScheduler {
    pub(crate) fn begin_frame(&mut self) {
        self.frame_continuous_delivered = false;
        self.frame_idle_async_delivered.clear();
    }

    pub(crate) fn set_event_disabled(&mut self, disabled: bool) {
        self.event_disabled = disabled;
    }

    pub(crate) fn begin_script_delivery_turn(&mut self) {
        self.sequence_to_process = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
    }

    pub(crate) fn post_script_event(&mut self, request: ScriptEventRequest) -> ScriptPostResult {
        if request.discardable && self.event_disabled {
            return ScriptPostResult::Discarded;
        }

        if matches!(request.mode, ScriptEventMode::Immediate) {
            if self.event_disabled {
                return ScriptPostResult::Discarded;
            }
            return ScriptPostResult::Immediate(self.script_event_from_request(request));
        }

        if matches!(request.mode, ScriptEventMode::RemovePost) {
            self.cancel_script_events(request.source, request.target, &request.name, request.tag);
        }

        let event = self.script_event_from_request(request);
        self.script_events.push_back(event);
        ScriptPostResult::Queued
    }

    pub(crate) fn post_timer_event(&mut self, handle: ObjectHandle, tag: u32) {
        let _ = self.post_script_event(
            ScriptEventRequest::new(
                handle,
                handle,
                TIMER_EVENT_NAME,
                tag,
                ScriptEventKind::Timer,
            )
            .discardable(true),
        );
    }

    pub(crate) fn trigger_async(
        &mut self,
        handle: ObjectHandle,
        mode: AsyncTriggerMode,
        cached: bool,
    ) {
        match mode {
            AsyncTriggerMode::Normal | AsyncTriggerMode::Exclusive => {
                if cached {
                    self.cancel_source_events(handle);
                }
                let _ = self.post_script_event(
                    ScriptEventRequest::new(
                        handle,
                        handle,
                        ASYNC_TRIGGER_EVENT_NAME,
                        0,
                        ScriptEventKind::AsyncTrigger,
                    )
                    .exclusive(matches!(mode, AsyncTriggerMode::Exclusive)),
                );
            }
            AsyncTriggerMode::AtIdle => {
                let entry = self.idle_async_triggers.entry(handle).or_insert(0);
                if cached {
                    *entry = 1;
                } else {
                    *entry = entry.saturating_add(1);
                }
            }
        }
    }

    pub(crate) fn cancel_async(&mut self, handle: ObjectHandle) {
        self.cancel_source_events(handle);
        self.idle_async_triggers.remove(&handle);
    }

    pub(crate) fn pop_script_event(
        &mut self,
        selection: ScriptEventSelection,
    ) -> Option<ScriptEvent> {
        let index = self.script_events.iter().position(|event| {
            if event.sequence > self.sequence_to_process {
                return false;
            }
            match selection {
                ScriptEventSelection::Exclusive => event.exclusive,
                ScriptEventSelection::Any => true,
            }
        })?;
        self.script_events.remove(index)
    }

    pub(crate) fn has_exclusive_script_event(&self) -> bool {
        self.script_events.iter().any(|event| event.exclusive)
    }

    pub(crate) fn count_script_events(
        &self,
        source: ObjectHandle,
        target: ObjectHandle,
        name: &str,
        tag: u32,
    ) -> usize {
        self.script_events
            .iter()
            .filter(|event| event.matches(source, target, name, tag))
            .count()
    }

    pub(crate) fn cancel_script_events(
        &mut self,
        source: ObjectHandle,
        target: ObjectHandle,
        name: &str,
        tag: u32,
    ) -> usize {
        let before = self.script_events.len();
        self.script_events
            .retain(|event| !event.matches(source, target, name, tag));
        before - self.script_events.len()
    }

    pub(crate) fn cancel_source_events(&mut self, source: ObjectHandle) -> usize {
        let before = self.script_events.len();
        self.script_events.retain(|event| event.source != source);
        before - self.script_events.len()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn discard_all_discardable_events(&mut self) -> usize {
        let before = self.script_events.len();
        self.script_events.retain(|event| !event.discardable);
        before - self.script_events.len()
    }

    pub(crate) fn post_input_event(&mut self, event: EngineEvent) {
        self.input_events.push_back(event);
    }

    pub(crate) fn pop_input_event(&mut self) -> Option<EngineEvent> {
        self.input_events.pop_front()
    }

    pub(crate) fn post_window_update(&mut self, handle: ObjectHandle) -> bool {
        let queued = self
            .window_update_events
            .iter()
            .filter(|event| **event == handle)
            .count();
        if !self.window_updates_delivering {
            if queued == 0 {
                self.window_update_events.push_back(handle);
            }
            return true;
        }

        let active = usize::from(self.active_window_update == Some(handle));
        let delivered = self
            .delivered_window_update_counts
            .get(&handle)
            .copied()
            .unwrap_or(0);
        if active + delivered + queued < 2 {
            self.window_update_events.push_back(handle);
            true
        } else {
            // A queued event will still complete the pending paint. If only
            // the active event remains, the per-delivery recursion limit has
            // discarded this repost and no completion is pending.
            queued != 0
        }
    }

    pub(crate) fn begin_window_update_delivery(&mut self) -> bool {
        if self.window_updates_delivering || self.window_update_events.is_empty() {
            return false;
        }
        self.window_updates_delivering = true;
        self.delivered_window_update_counts.clear();
        true
    }

    pub(crate) fn pop_window_update_event(&mut self) -> Option<ObjectHandle> {
        let handle = self.window_update_events.pop_front()?;
        self.active_window_update = Some(handle);
        Some(handle)
    }

    pub(crate) fn finish_window_update_event(&mut self, handle: ObjectHandle) {
        self.active_window_update = None;
        *self
            .delivered_window_update_counts
            .entry(handle)
            .or_insert(0) += 1;
    }

    pub(crate) fn finish_window_update_delivery(&mut self) {
        self.window_updates_delivering = false;
        self.active_window_update = None;
        self.delivered_window_update_counts.clear();
    }

    pub(crate) fn register_timer(&mut self, handle: ObjectHandle) {
        self.timers.entry(handle).or_default();
    }

    pub(crate) fn timer_handles(&self) -> Vec<ObjectHandle> {
        self.timers.keys().copied().collect()
    }

    pub(crate) fn timer_next_fire_millis(&self, handle: ObjectHandle) -> Option<i64> {
        self.timers
            .get(&handle)
            .and_then(|timer| timer.next_fire_millis)
    }

    pub(crate) fn set_timer_next_fire_millis(
        &mut self,
        handle: ObjectHandle,
        next_fire_millis: Option<i64>,
    ) {
        self.timers.entry(handle).or_default().next_fire_millis = next_fire_millis;
    }

    pub(crate) fn next_timer_tag(&mut self, handle: ObjectHandle) -> u32 {
        let timer = self.timers.entry(handle).or_default();
        let tag = 1u32.saturating_add(timer.counter << 1);
        timer.counter = timer.counter.saturating_add(1);
        tag
    }

    pub(crate) fn schedule_audio_fade_completion(&mut self, handle: ObjectHandle, due: i64) {
        self.audio_fade_completions.insert(handle, due);
    }

    pub(crate) fn cancel_audio_fade_completion(&mut self, handle: ObjectHandle) {
        self.audio_fade_completions.remove(&handle);
    }

    pub(crate) fn post_due_audio_fade_completions(&mut self, now: i64) {
        let due = self
            .audio_fade_completions
            .iter()
            .filter_map(|(handle, due)| (*due <= now).then_some(*handle))
            .collect::<Vec<_>>();
        for handle in due {
            self.audio_fade_completions.remove(&handle);
            let _ = self.post_script_event(ScriptEventRequest::new(
                handle,
                handle,
                AUDIO_FADE_COMPLETED_EVENT_NAME,
                0,
                ScriptEventKind::AudioFadeCompleted,
            ));
        }
    }

    pub(crate) fn add_continuous_handler(&mut self, handler: Variant) {
        if !matches!(handler, Variant::Void)
            && !self.continuous_handlers.iter().any(|item| item == &handler)
        {
            self.continuous_handlers.push(handler);
        }
    }

    pub(crate) fn remove_continuous_handler(&mut self, handler: &Variant) -> bool {
        let before = self.continuous_handlers.len();
        self.continuous_handlers.retain(|item| item != handler);
        before != self.continuous_handlers.len()
    }

    pub(crate) fn pop_idle_event(&mut self) -> Option<IdleEvent> {
        if let Some(handle) = self
            .idle_async_triggers
            .keys()
            .copied()
            .find(|handle| !self.frame_idle_async_delivered.contains(handle))
        {
            self.frame_idle_async_delivered.insert(handle);
            let remove = {
                let count = self
                    .idle_async_triggers
                    .get_mut(&handle)
                    .expect("idle trigger key came from map");
                *count = count.saturating_sub(1);
                *count == 0
            };
            if remove {
                self.idle_async_triggers.remove(&handle);
            }
            return Some(IdleEvent::AsyncTrigger(handle));
        }

        if !self.frame_continuous_delivered && !self.continuous_handlers.is_empty() {
            self.frame_continuous_delivered = true;
            return Some(IdleEvent::ContinuousHandlers(
                self.continuous_handlers.clone(),
            ));
        }

        None
    }

    pub(crate) fn invalidate_object(&mut self, handle: ObjectHandle) {
        self.timers.remove(&handle);
        self.cancel_async(handle);
        self.window_update_events.retain(|event| *event != handle);
        self.audio_fade_completions.remove(&handle);
        self.script_events
            .retain(|event| event.source != handle && event.target != handle);
    }

    #[cfg(test)]
    pub(crate) fn has_window_update(&self, handle: ObjectHandle) -> bool {
        self.active_window_update == Some(handle)
            || self
                .window_update_events
                .iter()
                .any(|event| *event == handle)
    }

    fn script_event_from_request(&self, request: ScriptEventRequest) -> ScriptEvent {
        ScriptEvent {
            source: request.source,
            target: request.target,
            name: request.name,
            tag: request.tag,
            args: request.args,
            kind: request.kind,
            exclusive: request.exclusive,
            discardable: request.discardable,
            sequence: self.next_sequence,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScriptEvent {
    pub(crate) source: ObjectHandle,
    pub(crate) target: ObjectHandle,
    pub(crate) name: String,
    pub(crate) tag: u32,
    pub(crate) args: Vec<Variant>,
    pub(crate) kind: ScriptEventKind,
    pub(crate) exclusive: bool,
    pub(crate) discardable: bool,
    sequence: u64,
}

impl ScriptEvent {
    fn matches(&self, source: ObjectHandle, target: ObjectHandle, name: &str, tag: u32) -> bool {
        self.source == source
            && self.target == target
            && self.name == name
            && (tag == 0 || self.tag == tag)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum ScriptEventKind {
    Custom,
    Timer,
    AsyncTrigger,
    AudioFadeCompleted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum ScriptEventMode {
    Immediate,
    Post,
    RemovePost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScriptEventSelection {
    Exclusive,
    Any,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AsyncTriggerMode {
    Normal,
    Exclusive,
    AtIdle,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IdleEvent {
    AsyncTrigger(ObjectHandle),
    ContinuousHandlers(Vec<Variant>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ScriptPostResult {
    Queued,
    Immediate(ScriptEvent),
    Discarded,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScriptEventRequest {
    source: ObjectHandle,
    target: ObjectHandle,
    name: String,
    tag: u32,
    args: Vec<Variant>,
    kind: ScriptEventKind,
    mode: ScriptEventMode,
    exclusive: bool,
    discardable: bool,
}

impl ScriptEventRequest {
    pub(crate) fn new(
        source: ObjectHandle,
        target: ObjectHandle,
        name: impl Into<String>,
        tag: u32,
        kind: ScriptEventKind,
    ) -> Self {
        Self {
            source,
            target,
            name: name.into(),
            tag,
            args: Vec::new(),
            kind,
            mode: ScriptEventMode::Post,
            exclusive: false,
            discardable: false,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn mode(mut self, mode: ScriptEventMode) -> Self {
        self.mode = mode;
        self
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn args(mut self, args: Vec<Variant>) -> Self {
        self.args = args;
        self
    }

    pub(crate) fn exclusive(mut self, exclusive: bool) -> Self {
        self.exclusive = exclusive;
        self
    }

    pub(crate) fn discardable(mut self, discardable: bool) -> Self {
        self.discardable = discardable;
        self
    }
}

#[derive(Clone, Debug, Default)]
struct TimerState {
    next_fire_millis: Option<i64>,
    counter: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_script_event_is_returned_for_direct_delivery() {
        let mut scheduler = TvpScheduler::default();
        let source = ObjectHandle(1);
        let target = ObjectHandle(2);

        let result = scheduler.post_script_event(
            ScriptEventRequest::new(source, target, "onProbe", 7, ScriptEventKind::Custom)
                .mode(ScriptEventMode::Immediate)
                .args(vec![Variant::Integer(3)]),
        );

        let ScriptPostResult::Immediate(event) = result else {
            panic!("expected immediate event");
        };
        assert_eq!(event.source, source);
        assert_eq!(event.target, target);
        assert_eq!(event.name, "onProbe");
        assert_eq!(event.tag, 7);
        assert_eq!(event.args, vec![Variant::Integer(3)]);
    }

    #[test]
    fn remove_post_deletes_matching_source_target_name_and_tag() {
        let mut scheduler = TvpScheduler::default();
        let source = ObjectHandle(1);
        let target = ObjectHandle(2);
        let other = ObjectHandle(3);

        let _ = scheduler.post_script_event(ScriptEventRequest::new(
            source,
            target,
            "onProbe",
            10,
            ScriptEventKind::Custom,
        ));
        let _ = scheduler.post_script_event(ScriptEventRequest::new(
            source,
            target,
            "onProbe",
            11,
            ScriptEventKind::Custom,
        ));
        let _ = scheduler.post_script_event(ScriptEventRequest::new(
            other,
            target,
            "onProbe",
            10,
            ScriptEventKind::Custom,
        ));
        let _ = scheduler.post_script_event(
            ScriptEventRequest::new(source, target, "onProbe", 10, ScriptEventKind::Custom)
                .mode(ScriptEventMode::RemovePost),
        );

        assert_eq!(
            scheduler.count_script_events(source, target, "onProbe", 10),
            1
        );
        assert_eq!(
            scheduler.count_script_events(source, target, "onProbe", 11),
            1
        );
        assert_eq!(
            scheduler.count_script_events(other, target, "onProbe", 10),
            1
        );
    }

    #[test]
    fn discardable_script_events_drop_while_event_system_is_disabled() {
        let mut scheduler = TvpScheduler::default();
        let _ = scheduler.post_script_event(
            ScriptEventRequest::new(
                ObjectHandle(1),
                ObjectHandle(1),
                "onProbe",
                0,
                ScriptEventKind::Custom,
            )
            .discardable(true),
        );
        assert_eq!(scheduler.discard_all_discardable_events(), 1);

        scheduler.set_event_disabled(true);

        assert_eq!(
            scheduler.post_script_event(
                ScriptEventRequest::new(
                    ObjectHandle(1),
                    ObjectHandle(1),
                    "onProbe",
                    0,
                    ScriptEventKind::Custom,
                )
                .discardable(true),
            ),
            ScriptPostResult::Discarded
        );
        scheduler.begin_script_delivery_turn();
        assert!(
            scheduler
                .pop_script_event(ScriptEventSelection::Any)
                .is_none()
        );
    }
}
