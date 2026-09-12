//! eluna is a small Rust crate for Emote/PSB reverse engineering work.
//!
//! The current scope is intentionally narrow:
//! - PSB container parsing with explicit MDF/LZ4 normalization and optional key-based decryption;
//! - resource range extraction;
//! - D3D9-compatible Emote mesh vertex layout and strip generation.

pub mod api;
pub mod emote;
pub mod psb;
pub mod reverse;
pub mod runtime;
pub mod sdk;
pub mod schema;
pub mod shader;
pub mod vertex;

pub use psb::{
    adler32, bruteforce_emote_key, normalize_psb_input, PsbBruteforceOptions,
    PsbBruteforceResult, PsbDecryptionKey, PsbError, PsbFile, PsbHeader,
    PsbNormalizeOptions, PsbResourceRange, PsbValue, EMOTE_PSB_KEY0, EMOTE_PSB_KEY1,
    EMOTE_PSB_KEY2, EMOTE_PSB_KEY4, EMOTE_PSB_KEY5,
};
pub use vertex::{build_d3d_triangle_strips, expand_triangle_strips_to_list, EmoteVertex, MeshStripBatch, VertexBuildError};
pub use schema::{collect_resource_refs, collect_schema_paths, top_level_keys, PsbPathEntry, PsbResourceRefs, PsbValueKind};
pub use shader::{ScreenUvConstants, ShaderMatrixConstants};
pub use emote::{load_emote_static_scene, EmoteDrawFrameInfo, EmoteDrawPass, EmoteMeshChainNode, EmoteMeshPatch, EmoteModelSchema, EmoteMotionInfo, EmoteSceneBounds, EmoteSchemaError, EmoteStaticScene, EmoteStaticSprite, EmoteStepFrameInput, EmoteStepFrameLayerState, EmoteStepFrameMeshState, EmoteStepFrameOutput, EmoteTextureIcon, EmoteTextureSource};
pub use runtime::{collect_emote_runtime_pipeline, collect_emote_timelines, collect_emote_variables, BustPhysicsState, ClampControl, ElunaPlayer, EmoteRuntimePipeline, EmoteTimeline, EmoteTimelineFrame, EmoteTimelineVariable, EmoteVariableInfo, EmoteVariableState, EmoteVariableTarget, HairPhysicsState, LoopControl, MirrorControl, OpaqueControl, PhysicsControl, PhysicsControlDefinition, SelectorControl, SelectorOption, TransitionControl};
pub use sdk::{emote_runtime_parity_report, EmoteLoadOptions, EmoteRuntime, EmoteRuntimeError, EmoteRuntimeParityReport};

pub use api::{
    emote_ticks_to_milliseconds, milliseconds_to_emote_ticks, EmotePlayerControl, TimelinePlayMode,
    VariableWrite, EMOTE_TICKS_PER_SECOND,
};
