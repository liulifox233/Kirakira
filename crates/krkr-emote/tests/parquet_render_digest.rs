//! Real-asset regression: the exact pixels every PARQUET `.mtn` renders.
//!
//! The synthetic bit-exactness tests in `render.rs` hold the optimised
//! rasteriser to the pre-optimisation implementation on constructed meshes;
//! this one pins the *real* workload. Every `.mtn` inside the game's
//! `data.xp3`, every animation at six ticks (start, half a second, 1.5 s, 5 s,
//! 25 s and the ~15000 frame the M149 survey profiled), each rendered into a
//! 1280x720 canvas, is hashed into one digest over every canvas byte.
//!
//! The pin was produced by the renderer at the M154 base commit and must not
//! move for a throughput-only change: any difference here is a pixel change,
//! not a measurement artefact. To regenerate it deliberately, run the test
//! with `--nocapture` (the failure message prints the digest it saw) on the
//! commit whose output becomes the new contract, and update both constants.
//!
//! **M237 moved it once, deliberately.** The renderer had been compositing
//! every sprite source-over and ignoring the two per-sprite fields the
//! reference always carried — the frame's `bm` blend mode and its four corner
//! colours (`crates/krkr-emote/src/render.rs`, `SpriteBlend`). Honouring them
//! changes exactly the five members whose assets author non-default state, and
//! nothing else: `m2logo` (red vertex colours on a `bm 0` logo), both `sd101`
//! copies (a `bm 0x13` haze sprite), `title_bg` (one `bm 0x11` additive sprite)
//! and `yuzusourlogo` (white MODULATE2X corners on its gray circles). The
//! frame and item counts are unchanged, and
//! `tests/parquet_expressions.rs::the_blend_state_changes_only_the_motions_that_author_it`
//! pins that set. Under the neutral state — every other motion — the new code
//! path is bit-identical to the old one, which the crate's own unit tests
//! assert directly.
//!
//! The archive path comes from `KRKR_EMOTE_PARQUET_DIR` and defaults to
//! `/Users/ruri/Downloads/PARQUET`; when the game is not installed the test
//! prints a skip note and passes, like the other real-asset tests.

use std::io::Read as _;
use std::path::PathBuf;

use krkr_emote::{Canvas, Motion, TextureCache, Tint, render_draw_list};

const DEFAULT_PARQUET_DIR: &str = "/Users/ruri/Downloads/PARQUET";

/// Ticks sampled per animation, in emote ticks (1/60 s).
const TICKS: [f32; 6] = [0.0, 30.0, 90.0, 300.0, 1500.0, 15000.0];

/// The layer bitmap the plugin draws into on the game's own screen size.
const CANVAS: (u32, u32) = (1280, 720);

/// FNV-1a over the little-endian u64 words of every rendered canvas.
///
/// Produced by the pre-M154 renderer at the mission's base commit (`5d605f9`):
/// 954 frames, 2909 items, digest `9951313929727531003`.
///
/// Re-pinned once, by M237, when the renderer started honouring the sprites'
/// blend modes and corner colours: 954 frames, 2909 items (unchanged),
/// digest `6713103675942426750`. See the module docs for the five motions that
/// move and the neutral case that does not.
const GOLDEN_DIGEST: u64 = 6_713_103_675_942_426_750;

/// Frames whose draw list was non-empty (the sampled animations that draw).
const GOLDEN_FRAMES: usize = 954;

/// Draw items composited across those frames.
const GOLDEN_ITEMS: usize = 2909;

fn parquet_data_xp3() -> Option<PathBuf> {
    let dir = std::env::var_os("KRKR_EMOTE_PARQUET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PARQUET_DIR));
    let path = dir.join("data.xp3");
    path.is_file().then_some(path)
}

#[test]
fn the_games_frames_render_to_the_pinned_digest() {
    let Some(path) = parquet_data_xp3() else {
        eprintln!(
            "skipping: {} not found (override with KRKR_EMOTE_PARQUET_DIR)",
            DEFAULT_PARQUET_DIR
        );
        return;
    };
    let archive = krkr_xp3::Xp3Archive::open_file(&path).expect("data.xp3 opens");
    let mut members: Vec<String> = archive
        .entries()
        .iter()
        .map(|entry| entry.name.clone())
        .filter(|name| name.to_ascii_lowercase().ends_with(".mtn"))
        .collect();
    members.sort();
    assert_eq!(members.len(), 23, "the game ships 23 .mtn members");

    let mut digest = Digest::new();
    let mut canvas = Canvas::new(CANVAS.0, CANVAS.1);
    let mut frames = 0usize;
    let mut items_drawn = 0usize;
    for name in &members {
        let mut bytes = Vec::new();
        archive
            .open_by_name(name)
            .expect("member open")
            .unwrap_or_else(|| panic!("{name} exists"))
            .read_to_end(&mut bytes)
            .expect("member read");
        let motion = Motion::from_bytes(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        let mut textures = TextureCache::new(std::sync::Arc::new(motion.clone()));
        for animation in motion.animations() {
            for ticks in TICKS {
                let Ok(items) = motion.draw_list(&animation.name, ticks) else {
                    continue;
                };
                if items.is_empty() {
                    continue;
                }
                canvas.pixels_mut().fill(0);
                let report = render_draw_list(&mut canvas, &items, &mut textures, Tint::IDENTITY);
                assert_eq!(
                    report.skipped_missing_texture, 0,
                    "{name}/{} @ {ticks}: every texture decodes",
                    animation.name
                );
                frames += 1;
                items_drawn += report.drawn;
                digest.absorb(canvas.pixels());
            }
        }
    }

    println!(
        "PARQUET render digest: {} over {frames} frames, {items_drawn} items",
        digest.finish()
    );
    assert!(
        frames >= 800 && items_drawn >= 2_500,
        "the run rendered {frames} frames / {items_drawn} items — fewer than the game's sample"
    );
    assert_eq!(frames, GOLDEN_FRAMES, "sampled frame count moved");
    assert_eq!(items_drawn, GOLDEN_ITEMS, "composited item count moved");
    assert_eq!(
        digest.finish(),
        GOLDEN_DIGEST,
        "the rendered canvases no longer match the pinned pixels"
    );
}

/// FNV-1a over the little-endian u64 words of a canvas (and its tail bytes).
struct Digest(u64);

impl Digest {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn absorb(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        let (words, tail) = bytes.as_chunks::<8>();
        for word in words {
            hash = (hash ^ u64::from_le_bytes(*word)).wrapping_mul(Self::PRIME);
        }
        for &byte in tail {
            hash = (hash ^ u64::from(byte)).wrapping_mul(Self::PRIME);
        }
        self.0 = hash;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}
