//! Focused rasterizer benchmark over PARQUET's own `.mtn` draw lists.
//!
//! The full game is the honest workload, but it is also slow to reach and
//! noisy under a loaded box, so this drives the same rasteriser directly:
//! every `.mtn` in the game's `data.xp3`, every animation sampled at a handful
//! of ticks (including the ~15000 frame the M149 survey profiled), rendered
//! into a canvas the size of the game's screen.
//!
//! Reported per phase, because only one of them is the rasteriser:
//!
//! - `sample` — `Motion::draw_list` (eluna scene sampling) plus the first-touch
//!   texture decode through [`TextureCache`];
//! - `render` — `render_draw_list` alone, textures warm and canvas clearing
//!   excluded from the clock.
//!
//! Also prints a FNV-1a digest of every rendered byte, so the same run doubles
//! as the bit-exactness probe: the digest must not move when the rasteriser is
//! made faster.
//!
//! ```text
//! KRKR_EMOTE_PARQUET_DIR=/path/to/PARQUET \
//!   cargo run --release -p krkr-emote --example render_bench -- 5
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use krkr_emote::{Canvas, Motion, MotionDrawItem, TextureCache, Tint, render_draw_list};

/// Ticks sampled per animation; 15000 is the frame the M149 survey profiled.
const TICKS: [f32; 6] = [0.0, 30.0, 90.0, 300.0, 1500.0, 15000.0];

/// The game's screen size — the layer the plugin renders into.
const CANVAS: (u32, u32) = (1280, 720);

fn main() {
    let dir = std::env::var_os("KRKR_EMOTE_PARQUET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/ruri/games/PARQUET/PARQUET"));
    let iterations: u32 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(3);
    let archive = krkr_xp3::Xp3Archive::open_file(dir.join("data.xp3")).expect("data.xp3 opens");
    let mut members: Vec<String> = archive
        .entries()
        .iter()
        .map(|entry| entry.name.clone())
        .filter(|name| name.to_ascii_lowercase().ends_with(".mtn"))
        .collect();
    members.sort();
    assert!(!members.is_empty(), "no .mtn members in data.xp3");

    let motions: Vec<Motion> = members
        .iter()
        .map(|name| {
            let mut bytes = Vec::new();
            archive
                .open_by_name(name)
                .expect("member open")
                .unwrap_or_else(|| panic!("{name} exists"))
                .read_to_end(&mut bytes)
                .expect("member read");
            Motion::from_bytes(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"))
        })
        .collect();

    // Phase 1: sampling plus first-touch texture decode, one cache per motion.
    let mut caches: Vec<TextureCache> = motions
        .iter()
        .map(|motion| TextureCache::new(Arc::new(motion.clone())))
        .collect();
    let mut canvas = Canvas::new(CANVAS.0, CANVAS.1);
    let mut frames: Vec<(usize, Vec<MotionDrawItem>)> = Vec::new();
    let mut sample_time = Duration::ZERO;
    for (index, motion) in motions.iter().enumerate() {
        for animation in motion.animations() {
            for ticks in TICKS {
                let started = Instant::now();
                let Ok(items) = motion.draw_list(&animation.name, ticks) else {
                    continue;
                };
                canvas.pixels_mut().fill(0);
                let report =
                    render_draw_list(&mut canvas, &items, &mut caches[index], Tint::IDENTITY);
                sample_time += started.elapsed();
                if report.drawn > 0 {
                    frames.push((index, items));
                }
            }
        }
    }

    // Phase 2: one full pass rendered and digested — the bit-exactness probe,
    // reported separately so the digest's cost stays out of the render number.
    let mut digest = Fnv1a::new();
    let mut covered_pixels = 0u64;
    let digest_started = Instant::now();
    for (index, items) in &frames {
        canvas.pixels_mut().fill(0);
        render_draw_list(&mut canvas, items, &mut caches[*index], Tint::IDENTITY);
        digest.write(canvas.pixels());
        covered_pixels += canvas
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[3] > 0)
            .count() as u64;
    }
    let digest_time = digest_started.elapsed();

    // Phase 3: render only, textures warm, clearing outside the clock.
    let mut render_time = Duration::ZERO;
    for _ in 0..iterations {
        for (index, items) in &frames {
            canvas.pixels_mut().fill(0);
            let started = Instant::now();
            render_draw_list(&mut canvas, items, &mut caches[*index], Tint::IDENTITY);
            render_time += started.elapsed();
        }
    }

    let rendered = frames.len() as f64 * f64::from(iterations);
    let (msw, msh) = CANVAS;
    println!(
        "motions={} frames/iteration={} canvas={msw}x{msh}",
        motions.len(),
        frames.len()
    );
    println!(
        "sample phase: {:?} over {} frames ({:.2} ms/frame)",
        sample_time,
        frames.len(),
        sample_time.as_secs_f64() * 1000.0 / frames.len() as f64
    );
    println!(
        "digest pass: {:?} over {} frames",
        digest_time,
        frames.len()
    );
    println!(
        "render phase: {:?} over {rendered} frames ({:.3} ms/frame)",
        render_time,
        render_time.as_secs_f64() * 1000.0 / rendered
    );
    println!(
        "covered pixels: {covered_pixels} over {} frames ({:.0} px/frame, {:.1} Mpx/s)",
        frames.len(),
        covered_pixels as f64 / frames.len() as f64,
        covered_pixels as f64 / render_time.as_secs_f64() / 1e6
    );
    println!("canvas digest: {:016x}", digest.finish());
}

/// FNV-1a over every rendered byte.
struct Fnv1a(u64);

impl Fnv1a {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}
