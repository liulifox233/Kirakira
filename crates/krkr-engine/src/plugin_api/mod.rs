//! Plugin-facing engine facilities.
//!
//! A Rust plugin implements [`crate::KrkrPlugin`] in a crate whose production
//! dependencies are `krkr-engine` and `krkr-tjs2` only, so everything the
//! plugin needs from the engine has to be reachable through this crate. This
//! module is that surface:
//!
//! * [`storage`] registers a storage media (`psb`, `lzfs`, `proxy`, `steam`,
//!   `var`, `zip`) on the project storage — the engine-side counterpart of
//!   `TVPRegisterStorageMedia` (`krkrz/src/core/base/StorageIntf.cpp:530-538`).
//! * [`transition`] registers a transition handler provider under a name —
//!   the counterpart of `TVPAddTransHandlerProvider`
//!   (`krkrz/src/core/visual/TransIntf.cpp:307-324`): a provider owns its
//!   transition names and composes the two layer bitmaps CPU-side, instead of
//!   the engine carrying a name table for the plugin.
//! * [`layer`] reads and writes a layer's main and province bitmaps through
//!   scoped views — what the `layerEx*` family gets from
//!   `Layer.mainImageBuffer*` in the reference (`LayerIntf.cpp:9513-9553`).
//!   Per-layer plugin state slots live on [`crate::KrkrHost`]
//!   (`layer_extension*`).
//! * [`video`] opens a storage movie through the host's decoder factory and
//!   re-exports the decoder vocabulary (`VideoPort`, `VideoFrame`, ...), so a
//!   movie-drawing plugin (`layerExMovie`) can name what it decodes — the
//!   plugin-side counterpart of the reference's DirectShow graph, with the
//!   engine owning backend selection the way `VideoOverlay` does.
//!
//! The core types a plugin has to name — the storage port a wrapping media
//! resolves its inner names through, and the media trait it implements — are
//! re-exported here so no plugin crate depends on `krkr-assets` or `krkr-core`
//! directly.
//!
//! Engine-owned, never plugin-side: the registry, the scheme split and
//! lowercasing, the resolve order, the write dispatch, the weak storage handle
//! and the unregistration lifecycle; for the bitmap views, the staging buffer,
//! the copy-on-write clone/commit, texture ids, image replacement and the
//! `Not drawable layer type` error; for movies, the decoder backend behind the
//! host's factory and the storage read. The plugin owns its container format,
//! its bytes, its caching and its error strings, and — for the bitmap views —
//! the algorithms, the clip-box application and the moment it calls
//! `layer_update`.

pub mod layer;
pub mod storage;
pub mod transition;
pub mod video;

pub use krkr_core::{ProjectStoragePort, ResourceData, ResourceStream, StorageMediaProvider};
pub use transition::{
    TransitionFace, TransitionFrame, TransitionHandler, TransitionHandlerError,
    TransitionHandlerProvider, TransitionOptions, TransitionRequest,
};
pub use video::{
    VideoDecoder, VideoDecoderFactory, VideoError, VideoFrame, VideoMetadata, VideoOpenError,
    VideoPort, VideoSource, open_movie,
};
