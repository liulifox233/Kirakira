//! Errors surfaced by the E-mote motion adapter.

use std::error::Error;
use std::fmt;

/// Failure modes of loading or sampling a `.mtn` motion.
#[derive(Debug)]
pub enum MotionError {
    /// The PSB container could not be read (signature, tables, encryption).
    Psb(eluna::PsbError),
    /// The PSB parsed, but its Emote tables (object/source) are not usable.
    Schema(eluna::EmoteSchemaError),
    /// The requested animation name does not exist in the model.
    MissingAnimation(String),
}

impl fmt::Display for MotionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Psb(error) => write!(f, "PSB container error: {error}"),
            Self::Schema(error) => write!(f, "E-mote schema error: {error}"),
            Self::MissingAnimation(name) => write!(f, "animation '{name}' does not exist"),
        }
    }
}

impl Error for MotionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Psb(error) => Some(error),
            Self::Schema(error) => Some(error),
            Self::MissingAnimation(_) => None,
        }
    }
}

impl From<eluna::PsbError> for MotionError {
    fn from(value: eluna::PsbError) -> Self {
        Self::Psb(value)
    }
}

impl From<eluna::EmoteSchemaError> for MotionError {
    fn from(value: eluna::EmoteSchemaError) -> Self {
        Self::Schema(value)
    }
}
