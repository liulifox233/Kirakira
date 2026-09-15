//! Real-asset pixel checks for two things the adapter used to lose.
//!
//! 1. **Different expressions.** PARQUET's SD cut-ins carry one animation per
//!    expression (`SD102AA` starry-eyed, `SD102AD` plain eyes with a worried
//!    mouth and question marks); the difference is authored as different face
//!    layers, and the draw list resolves it at tick 0. The test renders both
//!    into the motion's own screen box and compares them pixel by pixel,
//!    reporting where they differ so the region can be checked against the
//!    face sprite's own rectangle.
//! 2. **The blend and corner-colour state.** Every PARQUET `.mtn` authors a
//!    `bm`; three of the 23 also author corner colours, and six sprites in
//!    three motions use a non-default blend equation (`0x11` additive on one
//!    `title_bg` sprite, `0x13` multiply on two `sd101` sprites, and white
//!    MODULATE2X corners on three `yuzusourlogo` sprites). Rendering each
//!    motion with its authored state and again with the state neutralised
//!    makes the fix's pixel impact a number per motion — the regression
//!    baseline for everything else.
//!
//! Both tests skip (and pass) when the game is not installed, like the other
//! real-asset tests.

use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;

use krkr_emote::{Canvas, Motion, TextureCache, Tint, render_draw_list};

const DEFAULT_PARQUET_DIR: &str = "/Users/ruri/Downloads/PARQUET";

fn parquet_data_xp3() -> Option<PathBuf> {
    let dir = std::env::var_os("KRKR_EMOTE_PARQUET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PARQUET_DIR));
    let path = dir.join("data.xp3");
    path.is_file().then_some(path)
}

fn parquet_archive() -> Option<krkr_xp3::Xp3Archive<std::fs::File>> {
    let path = parquet_data_xp3()?;
    Some(krkr_xp3::Xp3Archive::open_file(&path).expect("data.xp3 opens"))
}

fn load<R>(archive: &krkr_xp3::Xp3Archive<R>, name: &str) -> Arc<Motion>
where
    R: std::io::Read + std::io::Seek + Send,
{
    let mut bytes = Vec::new();
    archive
        .open_by_name(name)
        .expect("member open")
        .unwrap_or_else(|| panic!("{name} exists"))
        .read_to_end(&mut bytes)
        .expect("member read");
    Arc::new(Motion::from_bytes(&bytes).unwrap_or_else(|error| panic!("{name}: {error}")))
}

/// The motion's authored screen box: the canvas the game composites onto the
/// layer, with the model origin at its centre.
struct Screen {
    width: u32,
    height: u32,
    offset_x: f32,
    offset_y: f32,
}

fn screen_of(motion: &Motion) -> Screen {
    let root = &motion.psb().root;
    let screen = root.field("screenSize").expect("screenSize");
    let width = screen.field_u32("width").unwrap_or(1920);
    let height = screen.field_u32("height").unwrap_or(1080);
    Screen {
        width,
        height,
        offset_x: width as f32 / 2.0 - screen.field_f32("originX").unwrap_or(0.0),
        offset_y: height as f32 / 2.0 - screen.field_f32("originY").unwrap_or(0.0),
    }
}

/// The colour the game clears the draw target with before every frame
/// (`AffineSourceMotion.tjs:3230-3231` calls `_player.clear(a0, a0.neutralColor)`,
/// and the work-layer path fills the same colour), so the reference's blend
/// modes composite onto a neutral gray rather than onto transparency.
const NEUTRAL: [u8; 4] = [0x80, 0x80, 0x80, 0xFF];

/// The placed draw list of one animation: translated into the motion's own
/// screen box the way the game composites it.
fn placed(motion: &Arc<Motion>, animation: &str, ticks: f32) -> Vec<krkr_emote::MotionDrawItem> {
    let screen = screen_of(motion);
    let mut items = motion
        .draw_list(animation, ticks)
        .unwrap_or_else(|error| panic!("{animation}@{ticks}: {error}"));
    for item in items.iter_mut() {
        item.world_transform[4] += screen.offset_x;
        item.world_transform[5] += screen.offset_y;
    }
    items
}

/// Whether any item carries a blend equation or corner colour other than the
/// neutral pair — i.e. whether the fix can change this frame's pixels at all.
fn authored_state(items: &[krkr_emote::MotionDrawItem]) -> bool {
    items
        .iter()
        .any(|item| item.blend_mode != 0x10 || item.corner_colors != [0x8080_80FF; 4])
}

/// Renders one placed draw list into the motion's screen box. `neutralised`
/// forces every sprite back to the source-over/neutral state the adapter used
/// to imply, which is the before-state of this change.
fn render(
    motion: &Arc<Motion>,
    textures: &mut TextureCache,
    items: &[krkr_emote::MotionDrawItem],
    neutralised: bool,
) -> Canvas {
    let screen = screen_of(motion);
    let mut canvas = Canvas::new(screen.width, screen.height);
    for pixel in canvas.pixels_mut().chunks_exact_mut(4) {
        pixel.copy_from_slice(&NEUTRAL);
    }
    let mut items = items.to_vec();
    if neutralised {
        for item in items.iter_mut() {
            item.blend_mode = 0x10;
            item.corner_colors = [0x8080_80FF; 4];
        }
    }
    render_draw_list(&mut canvas, &items, textures, Tint::IDENTITY);
    canvas
}

/// One animation at one tick, rendered as authored and with the state
/// neutralised; identical when the asset authors no blend/colour state.
fn frame_pair(
    motion: &Arc<Motion>,
    textures: &mut TextureCache,
    animation: &str,
    ticks: f32,
) -> (Canvas, Canvas) {
    let items = placed(motion, animation, ticks);
    (
        render(motion, textures, &items, false),
        render(motion, textures, &items, true),
    )
}

/// The differing pixels between two canvases, with their bounding box.
fn diff(left: &Canvas, right: &Canvas) -> (usize, [u32; 4]) {
    assert_eq!(
        (left.width(), left.height()),
        (right.width(), right.height())
    );
    let mut count = 0;
    let mut bbox = [u32::MAX, u32::MAX, 0, 0];
    for (index, (l, r)) in left
        .pixels()
        .chunks_exact(4)
        .zip(right.pixels().chunks_exact(4))
        .enumerate()
    {
        if l == r {
            continue;
        }
        count += 1;
        let x = index as u32 % left.width();
        let y = index as u32 / left.width();
        bbox[0] = bbox[0].min(x);
        bbox[1] = bbox[1].min(y);
        bbox[2] = bbox[2].max(x);
        bbox[3] = bbox[3].max(y);
    }
    (count, bbox)
}

/// Ticks each animation is sampled at, mirroring the render digest's set.
const TICKS: [f32; 6] = [0.0, 30.0, 90.0, 300.0, 1500.0, 15000.0];

/// The two expressions of PARQUET's SD102 cut-in are different images, and the
/// difference is where the face is.
#[test]
fn the_two_sd102_expressions_render_differently() {
    let Some(archive) = parquet_archive() else {
        eprintln!("skipping: the game is not installed (set KRKR_EMOTE_PARQUET_DIR)");
        return;
    };
    let motion = load(&archive, "sdmotion/sd102.mtn");
    let mut textures = TextureCache::new(Arc::clone(&motion));

    let starry = render(
        &motion,
        &mut textures,
        &placed(&motion, "SD102AA", 0.0),
        false,
    );
    let plain = render(
        &motion,
        &mut textures,
        &placed(&motion, "SD102AD", 0.0),
        false,
    );
    let (count, bbox) = diff(&starry, &plain);
    assert!(
        count > 10_000,
        "SD102AA and SD102AD are the same image ({count} differing pixels)"
    );

    // The face sprite `SD102/face01` is 389x333 at centre (-208, -167) in
    // model space; with the screen origin at (750, 450) it covers
    // (347.5, 116.5)-(736.5, 449.5). The two motions swap that whole layer
    // (`face01` vs `face04`), and `SD102AD` also floats its question marks to
    // the right of it, so the difference must (a) include the face rectangle
    // and (b) stay inside the scene the two motions share.
    let face = [347u32, 116, 737, 450];
    let (face_count, face_bbox) = {
        let mut count = 0;
        let mut bbox = [u32::MAX, u32::MAX, 0, 0];
        for y in face[1]..face[3] {
            for x in face[0]..face[2] {
                if starry.pixel(x as i32, y as i32) != plain.pixel(x as i32, y as i32) {
                    count += 1;
                    bbox[0] = bbox[0].min(x);
                    bbox[1] = bbox[1].min(y);
                    bbox[2] = bbox[2].max(x);
                    bbox[3] = bbox[3].max(y);
                }
            }
        }
        (count, bbox)
    };
    assert!(
        face_count > 20_000,
        "the face rectangle holds the expression difference ({face_count} pixels, bbox {face_bbox:?})"
    );
    assert!(
        bbox[0] >= face[0] / 2 && bbox[2] <= 1100 && bbox[1] >= 50 && bbox[3] <= 900,
        "the difference stays inside the motion's 1500x900 screen box: bbox {bbox:?}"
    );
    println!(
        "SD102AA vs SD102AD: {count} pixels differ, bbox {bbox:?}; {face_count} of them inside the face rectangle {face:?} (bbox {face_bbox:?})"
    );
}

/// Every PARQUET motion still renders as it did except where the asset authors
/// a non-default blend equation or corner colour — those three motions are
/// listed with their diff counts.
#[test]
fn the_blend_state_changes_only_the_motions_that_author_it() {
    let Some(archive) = parquet_archive() else {
        eprintln!("skipping: the game is not installed (set KRKR_EMOTE_PARQUET_DIR)");
        return;
    };
    let mut members: Vec<String> = archive
        .entries()
        .iter()
        .map(|entry| entry.name.clone())
        .filter(|name| name.to_ascii_lowercase().ends_with(".mtn"))
        .collect();
    members.sort();

    let mut changed = Vec::new();
    for name in &members {
        let motion = load(&archive, name);
        let mut textures = TextureCache::new(Arc::clone(&motion));
        let mut differing = 0usize;
        for animation in motion.animations() {
            for ticks in TICKS {
                let items = placed(&motion, &animation.name, ticks);
                if !authored_state(&items) {
                    continue;
                }
                let (authored, neutral) =
                    frame_pair(&motion, &mut textures, &animation.name, ticks);
                differing += diff(&authored, &neutral).0;
            }
        }
        if differing > 0 {
            changed.push((name.clone(), differing));
        }
    }

    let names: Vec<&str> = changed.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "motion/m2logo.mtn",
            "motion/sd/sd101.mtn",
            "motion/title_bg.mtn",
            "motion/yuzusourlogo.mtn",
            "sdmotion/sd101.mtn"
        ],
        "the motions whose pixels the authored blend/colour state moves: {changed:?}"
    );
    for (name, differing) in &changed {
        println!("{name}: {differing} pixels differ from the neutralised render");
    }
}
