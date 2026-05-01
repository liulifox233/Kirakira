use std::{collections::BTreeMap, error::Error, fmt, io::Cursor, time::Duration};

use kira::{
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Tween,
    sound::{
        static_sound::{StaticSoundData, StaticSoundHandle},
        streaming::{StreamingSoundData, StreamingSoundHandle},
    },
    track::{TrackBuilder, TrackHandle},
};
use krkr_core::{AudioBus, AudioCommand, AudioInstanceId, AudioSourceKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioState {
    Stopped,
    Ready,
}

#[derive(Debug)]
pub enum AudioError {
    BackendUnavailable(String),
    DecodeFailed { storage: String, message: String },
    PlaybackFailed { storage: String, message: String },
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackendUnavailable(message) => {
                write!(formatter, "audio backend is unavailable: {message}")
            }
            Self::DecodeFailed { storage, message } => {
                write!(formatter, "failed to decode audio `{storage}`: {message}")
            }
            Self::PlaybackFailed { storage, message } => {
                write!(formatter, "failed to play audio `{storage}`: {message}")
            }
        }
    }
}

impl Error for AudioError {}

pub struct AudioSystem {
    state: AudioState,
    backend: Option<KiraBackend>,
}

struct KiraBackend {
    manager: AudioManager<DefaultBackend>,
    bgm_track: TrackHandle,
    se_track: TrackHandle,
    handles: BTreeMap<AudioInstanceId, PlayingSound>,
}

struct PlayRequest {
    id: AudioInstanceId,
    bus: AudioBus,
    kind: AudioSourceKind,
    storage: String,
    bytes: Vec<u8>,
    looping: bool,
    volume: f32,
}

enum PlayingSound {
    Static {
        bus: AudioBus,
        handle: StaticSoundHandle,
    },
    Streaming {
        bus: AudioBus,
        handle: StreamingSoundHandle<kira::sound::FromFileError>,
    },
}

impl fmt::Debug for AudioSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AudioSystem")
            .field("state", &self.state)
            .field("ready", &self.backend.is_some())
            .finish()
    }
}

impl Default for AudioSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioSystem {
    pub const fn new() -> Self {
        Self {
            state: AudioState::Stopped,
            backend: None,
        }
    }

    pub fn prepare(&mut self) -> Result<(), AudioError> {
        if self.backend.is_some() {
            self.state = AudioState::Ready;
            return Ok(());
        }

        let mut manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
            .map_err(|error| AudioError::BackendUnavailable(error.to_string()))?;
        let bgm_track = manager
            .add_sub_track(TrackBuilder::new())
            .map_err(|error| AudioError::BackendUnavailable(error.to_string()))?;
        let se_track = manager
            .add_sub_track(TrackBuilder::new())
            .map_err(|error| AudioError::BackendUnavailable(error.to_string()))?;

        self.backend = Some(KiraBackend {
            manager,
            bgm_track,
            se_track,
            handles: BTreeMap::new(),
        });
        self.state = AudioState::Ready;
        Ok(())
    }

    pub fn apply_commands(
        &mut self,
        commands: impl IntoIterator<Item = AudioCommand>,
    ) -> Result<(), AudioError> {
        for command in commands {
            self.apply_command(command)?;
        }
        Ok(())
    }

    pub fn apply_command(&mut self, command: AudioCommand) -> Result<(), AudioError> {
        if self.backend.is_none() {
            self.prepare()?;
        }
        let backend = self.backend.as_mut().ok_or_else(|| {
            AudioError::BackendUnavailable("backend did not initialize".to_string())
        })?;
        backend.apply_command(command)
    }

    pub const fn state(&self) -> AudioState {
        self.state
    }
}

impl KiraBackend {
    fn apply_command(&mut self, command: AudioCommand) -> Result<(), AudioError> {
        match command {
            AudioCommand::Play {
                id,
                bus,
                kind,
                storage,
                bytes,
                looping,
                volume,
            } => self.play(PlayRequest {
                id,
                bus,
                kind,
                storage,
                bytes,
                looping,
                volume,
            }),
            AudioCommand::Stop { id, fade_seconds } => {
                if let Some(handle) = self.handles.get_mut(&id) {
                    handle.stop(tween(fade_seconds));
                }
                self.handles.remove(&id);
                Ok(())
            }
            AudioCommand::SetVolume {
                id,
                volume,
                fade_seconds,
            } => {
                if let Some(handle) = self.handles.get_mut(&id) {
                    handle.set_volume(linear_volume_to_decibels(volume), tween(fade_seconds));
                }
                Ok(())
            }
            AudioCommand::Pause { id, fade_seconds } => {
                if let Some(handle) = self.handles.get_mut(&id) {
                    handle.pause(tween(fade_seconds));
                }
                Ok(())
            }
            AudioCommand::Resume { id, fade_seconds } => {
                if let Some(handle) = self.handles.get_mut(&id) {
                    handle.resume(tween(fade_seconds));
                }
                Ok(())
            }
            AudioCommand::StopBus { bus, fade_seconds } => {
                for handle in self.handles.values_mut() {
                    if handle.bus() == bus {
                        handle.stop(tween(fade_seconds));
                    }
                }
                self.handles.retain(|_, handle| handle.bus() != bus);
                Ok(())
            }
            AudioCommand::SetBusVolume {
                bus,
                volume,
                fade_seconds,
            } => {
                match bus {
                    AudioBus::Master => self
                        .manager
                        .main_track()
                        .set_volume(linear_volume_to_decibels(volume), tween(fade_seconds)),
                    AudioBus::Bgm => self
                        .bgm_track
                        .set_volume(linear_volume_to_decibels(volume), tween(fade_seconds)),
                    AudioBus::SoundEffect => self
                        .se_track
                        .set_volume(linear_volume_to_decibels(volume), tween(fade_seconds)),
                }
                Ok(())
            }
        }
    }

    fn play(&mut self, request: PlayRequest) -> Result<(), AudioError> {
        let PlayRequest {
            id,
            bus,
            kind,
            storage,
            bytes,
            looping,
            volume,
        } = request;
        if let Some(mut old_handle) = self.handles.remove(&id) {
            old_handle.stop(Tween::default());
        }

        let db = linear_volume_to_decibels(volume);
        let handle = match kind {
            AudioSourceKind::Static => {
                let mut data =
                    StaticSoundData::from_cursor(Cursor::new(bytes)).map_err(|error| {
                        AudioError::DecodeFailed {
                            storage: storage.clone(),
                            message: error.to_string(),
                        }
                    })?;
                data = data.volume(db);
                if looping {
                    data = data.loop_region(..);
                }
                let handle = match bus {
                    AudioBus::Master => self.manager.play(data),
                    AudioBus::Bgm => self.bgm_track.play(data),
                    AudioBus::SoundEffect => self.se_track.play(data),
                }
                .map_err(|error| AudioError::PlaybackFailed {
                    storage: storage.clone(),
                    message: error.to_string(),
                })?;
                PlayingSound::Static { bus, handle }
            }
            AudioSourceKind::Streaming => {
                let mut data =
                    StreamingSoundData::from_cursor(Cursor::new(bytes)).map_err(|error| {
                        AudioError::DecodeFailed {
                            storage: storage.clone(),
                            message: error.to_string(),
                        }
                    })?;
                data = data.volume(db);
                if looping {
                    data = data.loop_region(..);
                }
                let handle = match bus {
                    AudioBus::Master => self.manager.play(data),
                    AudioBus::Bgm => self.bgm_track.play(data),
                    AudioBus::SoundEffect => self.se_track.play(data),
                }
                .map_err(|error| AudioError::PlaybackFailed {
                    storage: storage.clone(),
                    message: error.to_string(),
                })?;
                PlayingSound::Streaming { bus, handle }
            }
        };
        self.handles.insert(id, handle);
        Ok(())
    }
}

impl PlayingSound {
    fn bus(&self) -> AudioBus {
        match self {
            Self::Static { bus, .. } | Self::Streaming { bus, .. } => *bus,
        }
    }

    fn stop(&mut self, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.stop(tween),
            Self::Streaming { handle, .. } => handle.stop(tween),
        }
    }

    fn set_volume(&mut self, volume: Decibels, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.set_volume(volume, tween),
            Self::Streaming { handle, .. } => handle.set_volume(volume, tween),
        }
    }

    fn pause(&mut self, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.pause(tween),
            Self::Streaming { handle, .. } => handle.pause(tween),
        }
    }

    fn resume(&mut self, tween: Tween) {
        match self {
            Self::Static { handle, .. } => handle.resume(tween),
            Self::Streaming { handle, .. } => handle.resume(tween),
        }
    }
}

fn tween(seconds: f32) -> Tween {
    Tween {
        duration: Duration::from_secs_f32(seconds.max(0.0)),
        ..Tween::default()
    }
}

fn linear_volume_to_decibels(volume: f32) -> Decibels {
    let volume = volume.clamp(0.0, 1.0);
    if volume <= 0.0 {
        Decibels::SILENCE
    } else {
        Decibels(20.0 * volume.log10())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_linear_volume_to_decibels() {
        assert_eq!(linear_volume_to_decibels(1.0), Decibels::IDENTITY);
        assert_eq!(linear_volume_to_decibels(0.0), Decibels::SILENCE);
        assert!(linear_volume_to_decibels(0.5).0 < 0.0);
    }
}
