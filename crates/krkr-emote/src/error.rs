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
    /// The requested timeline name does not exist in the model.
    MissingTimeline(String),
    /// The requested resource index is not in the container.
    MissingResource(u32),
    /// A resource is there but its pixels cannot be decoded.
    Decode(crate::decode::DecodeError),
    /// The resource index does not belong to an icon of the source table, so
    /// its pixel dimensions and palette are unknown.
    UnknownTextureResource(u32),
}

impl fmt::Display for MotionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Psb(error) => write!(f, "PSB container error: {error}"),
            Self::Schema(error) => write!(f, "E-mote schema error: {error}"),
            Self::MissingAnimation(name) => write!(f, "animation '{name}' does not exist"),
            Self::MissingTimeline(name) => write!(f, "timeline '{name}' does not exist"),
            Self::MissingResource(index) => {
                write!(f, "the container has no resource {index}")
            }
            Self::Decode(error) => write!(f, "texture decode error: {error}"),
            Self::UnknownTextureResource(index) => write!(
                f,
                "resource {index} is not an icon pixel resource of this motion"
            ),
        }
    }
}

impl Error for MotionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Psb(error) => Some(error),
            Self::Schema(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::MissingAnimation(_)
            | Self::MissingTimeline(_)
            | Self::MissingResource(_)
            | Self::UnknownTextureResource(_) => None,
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

impl From<crate::decode::DecodeError> for MotionError {
    fn from(value: crate::decode::DecodeError) -> Self {
        Self::Decode(value)
    }
}
