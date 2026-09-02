//! Platform-neutral runtime orchestration.
//!
//! `RuntimeSession` is the seam used by desktop and Web shells.  It keeps
//! asset acquisition and audio output behind polling traits, while the
//! existing `KrkrEngine` remains responsible for TJS/KAG semantics.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use krkr_core::{
    AssetEvent, AssetScheduler, AudioCommand, AudioError, AudioEvent, AudioSink, Clock, SaveEvent,
    SaveStore,
};
use krkr_tjs2::TjsError;

use crate::{EngineFrame, EngineInput, EngineStep, KrkrEngine};

pub struct RuntimeFrame {
    pub engine: EngineFrame,
    pub assets: Vec<AssetEvent>,
    pub audio: Vec<AudioEvent>,
    pub saves: Vec<SaveEvent>,
}

pub struct RuntimeSession {
    engine: KrkrEngine,
    assets: Box<dyn AssetScheduler>,
    audio: Box<dyn AudioSink>,
    clock: Box<dyn Clock>,
    save: Option<Box<dyn SaveStore>>,
    pending_assets: BTreeSet<String>,
    pending_asset_requests: BTreeMap<krkr_core::AssetRequestId, String>,
}

impl RuntimeSession {
    pub fn new(
        engine: KrkrEngine,
        assets: Box<dyn AssetScheduler>,
        audio: Box<dyn AudioSink>,
        clock: Box<dyn Clock>,
    ) -> Self {
        Self {
            engine,
            assets,
            audio,
            clock,
            save: None,
            pending_assets: BTreeSet::new(),
            pending_asset_requests: BTreeMap::new(),
        }
    }

    pub fn engine(&self) -> &KrkrEngine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut KrkrEngine {
        &mut self.engine
    }

    /// Starts the project dispatcher through the same runtime owner that
    /// drives subsequent frames. Hosts may call this once after constructing
    /// their capability adapters; lazy startup resources are then resumed by
    /// [`RuntimeSession::update`].
    pub fn start_project(&mut self) -> Result<(), TjsError> {
        self.engine.start_project()
    }

    pub fn assets_mut(&mut self) -> &mut dyn AssetScheduler {
        &mut *self.assets
    }

    pub fn audio_mut(&mut self) -> &mut dyn AudioSink {
        &mut *self.audio
    }

    pub fn take_audio_commands(&mut self) -> Vec<AudioCommand> {
        self.audio.take_commands()
    }

    pub fn clock_mut(&mut self) -> &mut dyn Clock {
        &mut *self.clock
    }

    pub fn set_save_store(&mut self, save: Box<dyn SaveStore>) {
        self.save = Some(save);
    }

    pub fn request_save_load(
        &mut self,
        profile: &str,
        key: &str,
    ) -> Option<krkr_core::SaveRequestId> {
        self.save.as_mut().map(|store| store.load(profile, key))
    }

    pub fn request_save(
        &mut self,
        profile: &str,
        key: &str,
        data: std::sync::Arc<[u8]>,
    ) -> Option<krkr_core::SaveRequestId> {
        self.save
            .as_mut()
            .map(|store| store.save(profile, key, data))
    }

    pub fn pending_asset_count(&self) -> usize {
        self.pending_assets.len()
    }

    pub fn pending_asset_paths(&self) -> impl Iterator<Item = &str> {
        self.pending_assets.iter().map(String::as_str)
    }

    pub fn cancel_asset(&mut self, id: krkr_core::AssetRequestId) -> bool {
        let cancelled = self.assets.cancel(id);
        if cancelled {
            if let Some(path) = self.pending_asset_requests.remove(&id) {
                if let Some(pending) = self
                    .pending_assets
                    .iter()
                    .find(|pending| pending.eq_ignore_ascii_case(&path))
                    .cloned()
                {
                    self.pending_assets.remove(&pending);
                }
            }
        }
        cancelled
    }

    /// Runs one host frame and returns all cross-platform side effects.
    pub fn update(
        &mut self,
        input: EngineInput,
        delta: Duration,
    ) -> Result<RuntimeFrame, RuntimeSessionError> {
        // Sample the host clock at the boundary and pass it into the engine;
        // this keeps timers/video/audio waits deterministic across wall-clock,
        // virtual-clock and browser performance.now implementations.
        let now_millis = self.clock.now_millis();
        let input = input.with_now_millis(now_millis);
        let assets = self.assets.poll();
        for event in &assets {
            let event_id = match event {
                AssetEvent::Ready { id, .. } | AssetEvent::Failed { id, .. } => *id,
            };
            self.pending_asset_requests.remove(&event_id);
            let was_pending = match event {
                AssetEvent::Ready { path, .. } | AssetEvent::Failed { path, .. } => self
                    .pending_assets
                    .iter()
                    .any(|pending| pending.eq_ignore_ascii_case(path)),
            };
            match event {
                AssetEvent::Ready { path, .. } | AssetEvent::Failed { path, .. } => {
                    if let Some(pending) = self
                        .pending_assets
                        .iter()
                        .find(|pending| pending.eq_ignore_ascii_case(path))
                        .cloned()
                    {
                        self.pending_assets.remove(&pending);
                    }
                }
            }
            match event {
                AssetEvent::Ready { path, data, .. } => {
                    if let Err(error) = self.engine.provide_external_resource(path, data.to_vec()) {
                        if is_resource_pending_error(&error) {
                            self.engine.mark_resource_waiting();
                        } else {
                            return Err(error.into());
                        }
                    }
                }
                AssetEvent::Failed { path, .. }
                    if was_pending || self.engine.has_external_resource_request(path) =>
                {
                    // Wake the suspended operation so the engine reports a
                    // deterministic decode/read error instead of remaining
                    // blocked forever on a failed network request.
                    if let Err(error) = self.engine.provide_external_resource(path, Vec::new()) {
                        if is_resource_pending_error(&error) {
                            self.engine.mark_resource_waiting();
                        } else {
                            return Err(error.into());
                        }
                    }
                }
                AssetEvent::Failed { .. } => {}
            }
        }
        let step = self.engine.step(input, delta)?;
        let engine = match step {
            EngineStep::Ready { frame, requests } | EngineStep::Waiting { frame, requests } => {
                for request in requests {
                    // A suspended VM may report the same request on every
                    // frame. Keep one scheduler waiter per logical path so
                    // slow fetches cannot fan out into duplicate work.
                    if self
                        .pending_assets
                        .iter()
                        .any(|pending| pending.eq_ignore_ascii_case(&request.path))
                    {
                        continue;
                    }
                    self.pending_assets.insert(request.path.clone());
                    let id = self.assets.request(&request.path, request.kind);
                    self.pending_asset_requests.insert(id, request.path);
                }
                frame
            }
        };
        let commands = self.engine.host_mut().take_audio_commands();
        self.audio.submit(&commands)?;
        let audio = self.audio.poll_events();
        // Audio completion is part of the runtime boundary, not a platform
        // UI concern.  KAG conductors wait on this signal and must be woken
        // even when a host only consumes the returned diagnostics (the Web
        // shell does not have a native audio worker to do it for us).
        for event in &audio {
            if let AudioEvent::PlaybackStopped { id } = event {
                self.engine.notify_audio_stopped(*id)?;
            }
        }
        let saves = self
            .save
            .as_mut()
            .map(|store| store.poll())
            .unwrap_or_default();
        Ok(RuntimeFrame {
            engine,
            assets,
            audio,
            saves,
        })
    }
}

fn is_resource_pending_error(error: &TjsError) -> bool {
    error.kind == krkr_tjs2::TjsErrorKind::ResourcePending
        || error.to_string().contains("KAG resource is pending:")
}

#[derive(Debug)]
pub enum RuntimeSessionError {
    Engine(TjsError),
    Audio(AudioError),
}

impl RuntimeSessionError {
    pub fn is_debug_quit(&self) -> bool {
        matches!(self, Self::Engine(error) if error.is_debug_quit())
    }
}

impl std::fmt::Display for RuntimeSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(error) => write!(f, "engine update failed: {error}"),
            Self::Audio(error) => write!(f, "audio update failed: {error}"),
        }
    }
}

impl std::error::Error for RuntimeSessionError {}

impl From<TjsError> for RuntimeSessionError {
    fn from(error: TjsError) -> Self {
        Self::Engine(error)
    }
}

impl From<AudioError> for RuntimeSessionError {
    fn from(error: AudioError) -> Self {
        Self::Audio(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krkr_core::{FrameInput, MemoryAssetStore, Size};

    #[test]
    fn runtime_session_emits_a_frame_without_platform_services() {
        let engine = KrkrEngine::new(crate::EngineConfig::default()).expect("engine");
        let mut session = RuntimeSession::new(
            engine,
            Box::new(MemoryAssetStore::default()),
            Box::new(NullAudioSink),
            Box::new(krkr_core::VirtualClock::default()),
        );
        let frame = session
            .update(
                crate::EngineInput::new(
                    FrameInput::new(Size::new(320.0, 240.0), 1.0 / 60.0),
                    Vec::new(),
                ),
                Duration::from_millis(16),
            )
            .expect("runtime frame");
        assert!(frame.assets.is_empty());
        assert!(frame.audio.is_empty());
        assert!(frame.engine.output.image_uploads.is_empty());
    }

    struct NullAudioSink;

    impl AudioSink for NullAudioSink {
        fn prepare(&mut self) -> Result<(), AudioError> {
            Ok(())
        }

        fn submit(&mut self, _commands: &[AudioCommand]) -> Result<(), AudioError> {
            Ok(())
        }

        fn poll_events(&mut self) -> Vec<AudioEvent> {
            Vec::new()
        }
    }
}
