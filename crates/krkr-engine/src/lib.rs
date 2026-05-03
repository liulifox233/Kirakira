pub mod engine;
pub mod host;
pub mod plugin;
pub mod storage;

mod globals;
mod kag;
mod native;
pub mod resource_manager;
mod script;

pub use engine::{
    EngineConfig, EngineFrame, EngineInput, EngineTickResult, KagLocation, KagRunBudget,
    KagTaskState, KagYieldReason, KrkrEngine, SystemMetrics,
};
pub use host::KrkrHost;
pub use plugin::KrkrPlugin;
pub use resource_manager::{DecodedImageData, ResourceManager, ResourceTaskId};
pub use storage::ProjectStorage;
