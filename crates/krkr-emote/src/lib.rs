//! Motion model for the `Motion.Player` side of the E-mote plugin.
//!
//! The E-mote plugin draws models that the game ships as `.mtn` PSB files
//! (PARQUET uses PSB v3 and v4). Kirakira does not reimplement the PSB reader:
//! the vendored eluna copy under `vendor/eluna` parses the container, and this
//! crate adapts its output to a shape the engine can consume.
//!
//! Two things live here:
//!
//! - [`Motion`] — a loaded model: the source table (sources, icons, resource
//!   indices), the animations (motions with their layer/frame trees) and the
//!   file-level handles needed to read texture bytes.
//! - [`Motion::draw_list`] — the sampled draw list at a tick, produced by
//!   eluna's Emote scene builder on top of the adapted model.
//! - [`MotionPlayer`] — a live session over the same model: the authored
//!   variable table, the per-tick control pass (eye/brow/mouth, blink timers),
//!   timelines and the evaluated-variable scene rebuild, i.e. what the
//!   reference's `MEmotePlayer` runs. [`Motion::draw_list`] is the *static*
//!   constructor and does none of it; [`MotionPlayer`]'s own module docs map
//!   the reference surface onto eluna's implementation and list what eluna
//!   still lacks.
//!
//! ## PARQUET's motion flavor
//!
//! eluna models the FreeMote flavor, where `source.<name>.texture` carries the
//! resource index/size and layer content names a texture directly. PARQUET's
//! `.mtn` files instead put the resource on each icon
//! (`source.<name>.icon.<icon>.pixel`) with no `texture` sub-object, and name
//! the icon as `src/<source>/<icon>`. [`NormalizeReport`]-counted adaptation
//! rewrites the parsed tree into eluna's shape before eluna's schema/scene code
//! sees it. The frame-sampler semantics PARQUET's reference authors
//! (`content.mask` key gating, the `{c,x,y}` cubic-Bezier easing curves, the
//! mesh `cc` curve) live in the vendored runtime itself; see
//! `vendor/eluna/UPSTREAM.md`'s patch ledger.
//!
//! ```no_run
//! use krkr_emote::Motion;
//!
//! fn main() -> Result<(), krkr_emote::MotionError> {
//!     let bytes = std::fs::read("motion/sd101.mtn").expect("read the .mtn file");
//!     let motion = Motion::from_bytes(&bytes)?;
//!
//!     for animation in motion.animations() {
//!         println!("{}: {} ticks", animation.name, animation.duration_ticks);
//!     }
//!
//!     for item in motion.draw_list("SD101AA", 0.0)? {
//!         let pixels = motion.texture_bytes(item.resource_index);
//!         println!("{} at {:?} ({:?} bytes)", item.texture, item.center, pixels.map(<[u8]>::len));
//!     }
//!     Ok(())
//! }
//! ```
//!
//! ## Follow-up (motionplayer plugin) seam
//!
//! The plugin calls, in order:
//!
//! 1. [`Motion::from_bytes`] with the storage bytes of the `.mtn` file,
//! 2. [`Motion::animations`] / [`Motion::sources`] to implement the
//!    `ResourceManager` metadata and motion/label listings,
//! 3. [`MotionPlayer::advance_ticks`] + [`MotionPlayer::draw_list`] per frame
//!    for a *live* session (authored variables, controls, timelines), or
//!    [`Motion::draw_list`] (or [`Motion::draw_list_with_variables`]) for a
//!    static sample the caller drives itself,
//! 4. [`TextureCache`] + [`render_draw_list`] to composite that list into the
//!    layer's RGBA bitmap (a [`Canvas`] over the layer's pixels),
//! 5. [`Motion::scene_at`] / [`Motion::psb`] / [`Motion::schema`] when it needs
//!    the raw eluna view (passes, mesh patches, stencil metadata) — the passes
//!    and mesh patches [`render_draw_list`] does not yet draw are counted in
//!    its [`RenderReport`].
//!
//! `crates/krkr-plugins/src/motion_player.rs` implements the TJS surface
//! (`Motion`/`Motion.Player`/`Motion.EmotePlayer`/`Motion.ResourceManager`)
//! on top of this seam; the end-to-end test in that module drives a synthetic
//! motion through those classes into a layer bitmap.
//!
//! `examples/motion_probe.rs` is the read-only probe over the game's own
//! assets: it dumps an animation's draw list, the corner-colour/blend state of
//! every sprite and (with `MOTION_PROBE_SCAN=1`) the runtime tables of all 23
//! `.mtn` members.

mod decode;
mod error;
mod model;
mod motion;
mod normalize;
mod player;
mod reference;
mod render;

pub use decode::{DecodeError, DecodedTexture, decode_icon, decode_rle};
pub use error::MotionError;
pub use model::{
    MotionAnimation, MotionBinding, MotionClipRect, MotionDrawItem, MotionFrame, MotionIcon,
    MotionLayer, MotionSource, MotionSourceTexture,
};
pub use motion::Motion;
pub use normalize::NormalizeReport;
pub use player::MotionPlayer;
pub use render::{
    Canvas, RenderReport, SpriteBlend, TextureCache, Tint, render_draw_list, render_draw_list_into,
};

/// The eluna API this crate builds on, re-exported so consumers do not have to
/// name the vendored path dependency for the types that appear in our
/// signatures.
pub use eluna::{
    EMOTE_TICKS_PER_SECOND, EmoteDrawFrameInfo, EmoteDrawPass, EmoteMeshPatch, EmoteModelSchema,
    EmoteSceneBounds, EmoteSchemaError, EmoteStaticScene, EmoteStaticSprite, EmoteTextureIcon,
    EmoteTextureSource, PsbError, PsbFile, PsbValue, TimelinePlayMode, emote_ticks_to_milliseconds,
    milliseconds_to_emote_ticks,
};
