pub mod engine;
mod globals;
pub mod host;
mod kag;
mod native;
pub mod plugin;
pub mod resource_manager;
mod scheduler;
mod script;
pub mod session;

pub use engine::{
    EngineConfig, EngineFrame, EngineInput, EngineStep, EngineTickResult, ExternalResourceRequest,
    KagLocation, KagRunBudget, KagTaskState, KagYieldReason, KrkrEngine, SystemMetrics,
};
pub use host::{
    KrkrHost, NativeTextDrawEvent, SystemPaths, TransitionPolicy, VideoOverlaySnapshot,
};
pub use plugin::KrkrPlugin;
pub use resource_manager::{DecodedImageData, ResourceManager, ResourceTaskId};
pub use session::{RuntimeFrame, RuntimeSession, RuntimeSessionError};
