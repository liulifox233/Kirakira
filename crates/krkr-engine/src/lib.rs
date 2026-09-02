pub mod engine;
pub mod host;
pub mod plugin;
pub mod storage;

mod globals;
mod kag;
mod native;
pub mod resource_manager;
mod scheduler;
mod script;
pub mod session;

pub use engine::{
    EngineConfig, EngineFrame, EngineInput, EngineStep, EngineTickResult, ExternalResourceRequest,
    KagLocation, KagRunBudget, KagTaskState, KagYieldReason, KrkrEngine, SystemMetrics,
};
pub use host::{KrkrHost, NativeTextDrawEvent, TransitionPolicy, VideoOverlaySnapshot};
pub use plugin::KrkrPlugin;
pub use resource_manager::{DecodedImageData, ResourceManager, ResourceTaskId};
pub use session::{RuntimeFrame, RuntimeSession, RuntimeSessionError};
pub use storage::{PackageMount, ProjectStorage};
