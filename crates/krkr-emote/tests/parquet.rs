//! Real-asset test: every `.mtn` motion inside PARQUET's `data.xp3`.
//!
//! The game ships 23 `.mtn` files (20 of them PSB v4) under `motion/` and
//! `sdmotion/`. The test reads them out of the archive read-only, loads each
//! one through [`krkr_emote::Motion`] and checks the model shape plus the
//! draw list sampled at several ticks for every animation.
//!
//! The archive path is `/Users/ruri/Downloads/PARQUET/data.xp3` by default and
//! can be overridden with `KRKR_EMOTE_PARQUET_DIR`. When the game is not
//! installed the test prints a skip note and passes, so a clean checkout still
//! runs green.

use std::io::Read as _;
use std::path::PathBuf;

use krkr_emote::{Canvas, Motion, TextureCache, Tint, render_draw_list};

const DEFAULT_PARQUET_DIR: &str = "/Users/ruri/Downloads/PARQUET";

/// Ticks the draw list is sampled at: start, 1 s, 10 s and 100 s (1/60 s ticks).
const SAMPLE_TICKS: [f32; 4] = [0.0, 60.0, 600.0, 6000.0];

fn parquet_data_xp3() -> Option<PathBuf> {
    let dir = std::env::var_os("KRKR_EMOTE_PARQUET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PARQUET_DIR));
    let path = dir.join("data.xp3");
    path.is_file().then_some(path)
}

/// Reads one `.mtn` member out of the game's `data.xp3`.
fn parquet_motion<R>(archive: &krkr_xp3::Xp3Archive<R>, name: &str) -> Motion
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
    Motion::from_bytes(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"))
}

#[test]
fn parses_every_parquet_mtn() {
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
    assert!(
        members.len() >= 20,
        "expected the game's 23 .mtn members, found {}",
        members.len()
    );

    let mut loaded = 0usize;
    let mut v4 = 0usize;
    let mut draw_items = 0usize;
    let mut with_draw_items = 0usize;
    let mut unresolved = Vec::new();
    let mut failures = Vec::new();

    for name in &members {
        let mut bytes = Vec::new();
        archive
            .open_by_name(name)
            .expect("member open")
            .unwrap_or_else(|| panic!("{name} exists"))
            .read_to_end(&mut bytes)
            .expect("member read");

        let motion = match Motion::from_bytes(&bytes) {
            Ok(motion) => motion,
            Err(error) => {
                failures.push(format!("{name}: {error}"));
                continue;
            }
        };

        loaded += 1;
        assert!(
            matches!(motion.psb().version, 3 | 4),
            "{name}: unexpected PSB version"
        );
        if motion.psb().version == 4 {
            v4 += 1;
        }

        let icons: usize = motion
            .sources()
            .values()
            .map(|source| source.icons.len())
            .sum();
        assert!(
            !motion.sources().is_empty(),
            "{name}: source table is empty"
        );
        assert!(icons > 0, "{name}: no icons in any source");
        assert!(
            !motion.animations().is_empty(),
            "{name}: no animations under {}",
            motion.base_object()
        );
        assert_eq!(
            motion.normalize_report().icons_without_pixel,
            0,
            "{name}: icons without a pixel resource"
        );
        if motion.normalize_report().unresolved_icon_references > 0 {
            unresolved.push(name.clone());
        }

        let mut samples = 0usize;
        for animation in motion.animations() {
            assert!(
                animation.duration_ticks > 0.0,
                "{name}/{}: duration is not positive",
                animation.name
            );
            let (min, max) = animation
                .frame_range()
                .unwrap_or_else(|| panic!("{name}/{}: no frames", animation.name));
            assert!(
                min <= max && max <= animation.duration_ticks,
                "{name}/{}: frame range {min}..{max} outside duration {}",
                animation.name,
                animation.duration_ticks
            );

            for ticks in SAMPLE_TICKS {
                let items = motion
                    .draw_list(&animation.name, ticks)
                    .unwrap_or_else(|error| panic!("{name}/{} @ {ticks}: {error}", animation.name));
                for (index, item) in items.iter().enumerate() {
                    assert_eq!(
                        item.draw_index, index,
                        "{name}/{}: draw list is not in draw order",
                        animation.name
                    );
                    // A layer may pull in another motion (`motion/<object>/<motion>`),
                    // so the sprite names the motion it came from, and that motion
                    // can live in a non-base object. What must hold is that every
                    // sprite resolves to a texture source the adaptation produced.
                    assert!(
                        motion.schema().textures.contains_key(&item.texture),
                        "{name}: sprite texture {} is not an adapted texture source",
                        item.texture
                    );
                    assert!(
                        motion.texture_bytes(item.resource_index).is_some(),
                        "{name}: resource {} of {} is out of range",
                        item.resource_index,
                        item.texture
                    );
                    assert!(
                        item.uv.iter().all(|value| (-0.001..=1.001).contains(value)),
                        "{name}/{}: uv {:?} outside the texture",
                        animation.name,
                        item.uv
                    );
                    assert!(
                        item.size
                            .iter()
                            .all(|value| value.is_finite() && *value > 0.0),
                        "{name}/{}: size {:?}",
                        animation.name,
                        item.size
                    );
                    assert!(
                        (0.0..=1.0).contains(&item.opacity),
                        "{name}/{}: opacity {}",
                        animation.name,
                        item.opacity
                    );
                    assert!(
                        item.world_transform.iter().all(|value| value.is_finite()),
                        "{name}/{}: non-finite transform",
                        animation.name
                    );
                }
                samples += items.len();
            }
        }

        if samples > 0 {
            with_draw_items += 1;
        }
        draw_items += samples;
        println!(
            "{name}: v{} base={} sources={} icons={} animations={} draw_items={samples}",
            motion.psb().version,
            motion.base_object(),
            motion.sources().len(),
            icons,
            motion.animations().len()
        );
    }

    println!(
        "PARQUET data.xp3: {} .mtn members, {loaded} loaded ({v4} PSB v4), \
         {with_draw_items} with draw items ({draw_items} sampled), \
         {} failed to load, {} with unresolved icon references",
        members.len(),
        failures.len(),
        unresolved.len()
    );
    for failure in &failures {
        eprintln!("failed: {failure}");
    }
    for name in &unresolved {
        eprintln!("unresolved icon reference: {name}");
    }

    assert!(
        failures.is_empty(),
        "{} motions failed to load",
        failures.len()
    );
    assert_eq!(loaded, members.len());
    assert!(v4 >= 20, "expected 20 PSB v4 motions, found {v4}");
    assert!(
        with_draw_items >= 20,
        "expected draw items for the game's animations, got {with_draw_items} motions"
    );
    assert!(
        unresolved.is_empty(),
        "{unresolved:?} reference icons the source table does not have"
    );
}

/// Every icon of every `.mtn` decodes to exactly its own dimensions.
///
/// The resources are M2 `RL` streams (8-bit paletted for the icons that carry
/// a `pal`, RGBA otherwise); the decode is what the plugin's `draw` feeds into
/// the layer bitmap, so a wrong alignment or codec rule shows up here as a
/// size mismatch on the game's own art.
#[test]
fn decodes_every_parquet_icon() {
    let Some(path) = parquet_data_xp3() else {
        eprintln!(
            "skipping: {} not found (override with KRKR_EMOTE_PARQUET_DIR)",
            DEFAULT_PARQUET_DIR
        );
        return;
    };
    let archive = krkr_xp3::Xp3Archive::open_file(&path).expect("data.xp3 opens");
    let names = [
        "motion/sd/sd101.mtn",
        "motion/splash.mtn",
        "motion/m2logo.mtn",
    ];

    let mut decoded = 0usize;
    let mut opaque_icons = 0usize;
    for name in names {
        let motion = parquet_motion(&archive, name);
        for (source_name, source) in motion.sources() {
            for icon in source.icons.values() {
                let texture = motion
                    .texture_pixels(icon.resource_index)
                    .unwrap_or_else(|error| panic!("{name}: {source_name}/{}: {error}", icon.name));
                assert_eq!(
                    (texture.width, texture.height),
                    (icon.width as u32, icon.height as u32),
                    "{name}: {source_name}/{} dimensions",
                    icon.name
                );
                assert_eq!(
                    texture.rgba.len(),
                    texture.width as usize * texture.height as usize * 4,
                    "{name}: {source_name}/{} buffer size",
                    icon.name
                );
                if texture
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] > 0)
                {
                    opaque_icons += 1;
                }
                decoded += 1;
            }
        }
    }
    println!("decoded {decoded} icons, {opaque_icons} with opaque pixels");
    assert!(
        decoded >= 50,
        "expected the game's icon tables, got {decoded}"
    );
    assert!(
        opaque_icons * 10 >= decoded * 9,
        "most icons carry visible pixels ({opaque_icons}/{decoded})"
    );
}

/// The `opa` scale, measured on the game's own fade: `sd101.mtn`'s
/// `ef_moya/bgef1` carries `opa: 192` on its frame at tick 90, so the layer
/// fades to 192/255 ≈ 0.7529 — the reference's `value & 0xff` reading, which
/// eluna itself now implements (`vendor/eluna/crates/eluna/src/emote.rs:2385`
/// reads the byte, `:3013` divides by 255). The adapter does not touch the
/// field; before M127 it rescaled 0..255 into the `/10` scale of the older
/// eluna, and against the new tree that double-scaling showed up here as
/// 192 → 7.53 → rounded 8 → 8/255 = 0.0314.
///
/// This also pins the second half of the same chain: PARQUET writes
/// `parameterize: null` on non-parameterised layers, and before the adapter
/// dropped it the layer was frozen at local time 0
/// (`emote.rs:4625-4631` + `:4707-4714`) and this frame never activated at any
/// tick.
#[test]
fn opa_is_an_eight_bit_alpha_on_the_games_fade() {
    let Some(path) = parquet_data_xp3() else {
        eprintln!(
            "skipping: {} not found (override with KRKR_EMOTE_PARQUET_DIR)",
            DEFAULT_PARQUET_DIR
        );
        return;
    };
    let archive = krkr_xp3::Xp3Archive::open_file(&path).expect("data.xp3 opens");
    let motion = parquet_motion(&archive, "motion/sd/sd101.mtn");

    assert!(
        motion.normalize_report().dropped_null_parameterize > 0,
        "PARQUET's `parameterize: null` fields are dropped"
    );

    let opacity_at = |ticks: f32| -> f32 {
        motion
            .draw_list("ef_moya", ticks)
            .unwrap_or_else(|error| panic!("ef_moya @ {ticks}: {error}"))
            .into_iter()
            .find(|item| item.label.as_deref() == Some("bgef1"))
            .unwrap_or_else(|| panic!("ef_moya @ {ticks} draws bgef1"))
            .opacity
    };
    let expected = 192.0 / 255.0;
    assert!(
        (opacity_at(90.0) - expected).abs() < 1e-5,
        "opa 192 renders as 192/255, got {}",
        opacity_at(90.0)
    );
    // The keyframes at t=90 (`opa: 192`) and t=180 (absent `opa` → 255) are
    // linearly interpolated — the native `sub_1032FB00` behaviour eluna
    // implements at `emote.rs:4172` (`lerp` then round to the nearest byte).
    // Tick 150 sits halfway: 192 + 63 * 60/90 = 234, i.e. 234/255 ≈ 0.9176.
    // The pinned eluna held the previous keyframe's value here (0.7529); the
    // fork head tweens, which is the reference's behaviour.
    let expected_at_150 = 234.0 / 255.0;
    assert!(
        (opacity_at(150.0) - expected_at_150).abs() < 1e-5,
        "the fade tweens to 234/255 at tick 150, got {}",
        opacity_at(150.0)
    );
    assert_eq!(opacity_at(0.0), 1.0, "before tick 90 the layer is opaque");
}

/// The `zcc` on `m2logo.mtn`'s `back_black/main` is a cubic-Bezier easing
/// curve, not a corner colour. The layer's type-3 frame at tick 0 carries
/// `zx = zy = 1.5` and `zcc = {c:[1,1], x:[0, 0.21333334, 0.5833333, 1],
/// y:[0, 0.62, 0.9, 1]}`; the next type-3 frame at tick 43 carries no zoom, so
/// the tween 1.5 -> 1.0 runs through that curve (`FUN_100087d0`: solve the
/// segment's x polynomial, evaluate the y polynomial).
///
/// At tick 21 the raw factor is 21/43 = 0.488372, which eases to 0.751373, so
/// the layer scale is `1.5 + (1.0 - 1.5) * 0.751373 = 1.124314`. The naive
/// linear tween — what the sampler did while the `{c,x,y}` curve shape was not
/// evaluated — gives 1.255814.
#[test]
fn zcc_eases_the_games_zoom_tween() {
    let Some(path) = parquet_data_xp3() else {
        eprintln!(
            "skipping: {} not found (override with KRKR_EMOTE_PARQUET_DIR)",
            DEFAULT_PARQUET_DIR
        );
        return;
    };
    let archive = krkr_xp3::Xp3Archive::open_file(&path).expect("data.xp3 opens");
    let motion = parquet_motion(&archive, "motion/m2logo.mtn");

    let scale_at = |ticks: f32| -> f32 {
        let items = motion
            .draw_list("back_black", ticks)
            .unwrap_or_else(|error| panic!("back_black @ {ticks}: {error}"));
        let item = items
            .iter()
            .find(|item| item.label.as_deref() == Some("レイヤ1"))
            .unwrap_or_else(|| panic!("back_black @ {ticks} draws レイヤ1"));
        // `レイヤ1`'s own frames carry no zoom, so the world matrix's 2x2
        // magnitude is the eased parent (`main`) scale exactly.
        item.world_transform[0].hypot(item.world_transform[2])
    };

    // The layer only starts drawing at tick 12; at ticks 21 and 30 the raw
    // factors 21/43 = 0.488372 and 30/43 = 0.697674 ease to 0.751373 and
    // 0.888598, i.e. scales 1.124314 and 1.055701 (linear: 1.255814 and
    // 1.151163).
    let eased = scale_at(21.0);
    assert!(
        (eased - 1.124314).abs() < 1e-4,
        "the zcc curve eases the zoom, got {eased}"
    );
    assert!(
        (eased - 1.255814).abs() > 0.1,
        "the value is not the linear tween (1.255814), got {eased}"
    );
    let later = scale_at(30.0);
    assert!(
        (later - 1.055701).abs() < 1e-4,
        "the zcc curve eases the zoom at tick 30, got {later}"
    );
}

/// `sd101.mtn` is the one PARQUET asset with a Bezier mesh:
/// `ef_moya/bgef1`'s frames at tick 0 and 180 carry `bp: null` (the native
/// neutral patch, a 4x4 grid of `col/3, row/3`, which eluna's identity patch
/// reproduces) and the frame at tick 90 carries the authored 32-value `bp`
/// (16 (x, y) control points) plus the mesh `cc` curve. Interpolation applies
/// that curve to the factor (`FUN_100098f0`).
///
/// `cc = {x:[0, 0.5706339, 0.42936608, 1], y:[0, 0.1854102, 0.8145898, 1]}`:
/// at tick 30 the raw factor 1/3 eases to 0.228263, so control point 5 is
/// `(1/3, 1/3) + ((0.20048445, 0.17175041) - (1/3, 1/3)) * 0.228263 =
/// (0.303009, 0.296450)`. The linear tween — the behaviour before the curve
/// was evaluated — gave (0.289050, 0.279472).
#[test]
fn mesh_keyframes_take_the_cc_curve() {
    let Some(path) = parquet_data_xp3() else {
        eprintln!(
            "skipping: {} not found (override with KRKR_EMOTE_PARQUET_DIR)",
            DEFAULT_PARQUET_DIR
        );
        return;
    };
    let archive = krkr_xp3::Xp3Archive::open_file(&path).expect("data.xp3 opens");
    let motion = parquet_motion(&archive, "motion/sd/sd101.mtn");

    let mesh_point = |ticks: f32| -> [f32; 2] {
        let items = motion
            .draw_list("ef_moya", ticks)
            .unwrap_or_else(|error| panic!("ef_moya @ {ticks}: {error}"));
        let item = items
            .iter()
            .find(|item| item.label.as_deref() == Some("bgef1"))
            .unwrap_or_else(|| panic!("ef_moya @ {ticks} draws bgef1"));
        item.mesh
            .as_ref()
            .unwrap_or_else(|| panic!("ef_moya @ {ticks} carries a mesh"))
            .control_points[5]
    };

    // The authored keyframe itself.
    let authored = mesh_point(90.0);
    assert!(
        (authored[0] - 0.20048445).abs() < 1e-5 && (authored[1] - 0.17175041).abs() < 1e-5,
        "the authored bp is read verbatim, got {authored:?}"
    );

    let at_30 = mesh_point(30.0);
    assert!(
        (at_30[0] - 0.303009).abs() < 1e-4 && (at_30[1] - 0.296450).abs() < 1e-4,
        "the cc curve eases the mesh interpolation, got {at_30:?}"
    );
    assert!(
        (at_30[0] - 0.289050).abs() > 0.005,
        "the value is not the linear tween (0.289050), got {at_30:?}"
    );

    // The curve is symmetric about its midpoint, so raw and eased agree at
    // tick 45 (`(1/3, 1/3)` to the authored point at half the span).
    let at_45 = mesh_point(45.0);
    assert!(
        (at_45[0] - 0.266909).abs() < 1e-4 && (at_45[1] - 0.252542).abs() < 1e-4,
        "the curve midpoint passes the raw factor through, got {at_45:?}"
    );
}

/// `yuzusourlogo.mtn`'s `awa/graycircle` uses a three-segment chained `zcc`
/// (`x.len() = 3N + 1`): the layer's type-3 frame at tick 0 carries `zx = 0.1`
/// and the next type-3 frame at 30 carries no zoom, so 0.1 -> 1.0 is eased by
/// the chained curve. At tick 7 the raw factor 7/30 = 0.233333 lands in the
/// first segment and evaluates to 0.470825, i.e. a scale of
/// `0.1 + 0.9 * 0.470825 = 0.523742` (the linear value was 0.31).
#[test]
fn chained_curve_segments_ease_the_games_zoom_tween() {
    let Some(path) = parquet_data_xp3() else {
        eprintln!(
            "skipping: {} not found (override with KRKR_EMOTE_PARQUET_DIR)",
            DEFAULT_PARQUET_DIR
        );
        return;
    };
    let archive = krkr_xp3::Xp3Archive::open_file(&path).expect("data.xp3 opens");
    let motion = parquet_motion(&archive, "motion/yuzusourlogo.mtn");

    let scale_at = |ticks: f32| -> f32 {
        let items = motion
            .draw_list("awa", ticks)
            .unwrap_or_else(|error| panic!("awa @ {ticks}: {error}"));
        let item = items
            .iter()
            .find(|item| item.label.as_deref() == Some("graycircle"))
            .unwrap_or_else(|| panic!("awa @ {ticks} draws graycircle"));
        item.world_transform[0]
    };

    assert!((scale_at(0.0) - 0.1).abs() < 1e-4);
    let eased = scale_at(7.0);
    assert!(
        (eased - 0.523742).abs() < 1e-4,
        "the chained segments ease the zoom, got {eased}"
    );
    assert!(
        (eased - 0.31).abs() > 0.1,
        "the value is not the linear tween (0.31), got {eased}"
    );
}

/// Rendering a real motion writes pixels: `sd101.mtn`'s `SD101AA` covers a
/// good part of a 1500x900 canvas at tick 0, and nothing is skipped for a
/// missing texture.
#[test]
fn renders_a_real_motion_into_a_canvas() {
    let Some(path) = parquet_data_xp3() else {
        eprintln!(
            "skipping: {} not found (override with KRKR_EMOTE_PARQUET_DIR)",
            DEFAULT_PARQUET_DIR
        );
        return;
    };
    let archive = krkr_xp3::Xp3Archive::open_file(&path).expect("data.xp3 opens");
    let motion = parquet_motion(&archive, "motion/sd/sd101.mtn");
    let items = motion.draw_list("SD101AA", 0.0).expect("sample");
    assert!(!items.is_empty(), "the animation draws something");

    let mut canvas = Canvas::new(1500, 900);
    let mut textures = TextureCache::new(std::sync::Arc::new(motion.clone()));
    let report = render_draw_list(&mut canvas, &items, &mut textures, Tint::IDENTITY);
    println!("{report:?} over {} items", items.len());
    assert_eq!(report.skipped_missing_texture, 0, "every texture decodes");
    assert!(report.drawn > 0, "items are composited");
    let covered = canvas
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] > 0)
        .count();
    assert!(
        covered > 1000,
        "the character covers the canvas, got {covered} pixels"
    );

    // A plane of the wrong size is left alone rather than partially drawn.
    let mut wrong = vec![0u8; 16];
    let mut textures = TextureCache::new(std::sync::Arc::new(motion));
    let report =
        krkr_emote::render_draw_list_into(&mut wrong, 8, 8, &items, &mut textures, Tint::IDENTITY);
    assert_eq!(
        report,
        Default::default(),
        "a wrongly sized plane draws nothing"
    );
    assert!(wrong.iter().all(|byte| *byte == 0));
}
