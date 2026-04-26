use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioState {
    Stopped,
    Ready,
}

#[derive(Debug)]
pub enum AudioError {
    BackendUnavailable,
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackendUnavailable => write!(formatter, "audio backend is unavailable"),
        }
    }
}

impl Error for AudioError {}

#[derive(Debug)]
pub struct AudioSystem {
    state: AudioState,
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
        }
    }

    pub fn prepare(&mut self) -> Result<(), AudioError> {
        self.state = AudioState::Ready;
        Ok(())
    }

    pub const fn state(&self) -> AudioState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_system_can_prepare_without_playback() {
        let mut audio = AudioSystem::new();

        audio.prepare().expect("prepare should be a no-op shell");

        assert_eq!(audio.state(), AudioState::Ready);
    }
}
