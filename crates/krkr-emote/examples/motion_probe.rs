//! Read-only probe: what a `.mtn` motion hands the draw path for two
//! animations that differ only in expression.
//!
//! Usage:
//!
//! ```sh
//! KRKR_EMOTE_PARQUET_DIR=/mnt/hdd/games/PARQUET/PARQUET \
//!   cargo run -p krkr-emote --example motion_probe -- sdmotion/sd102.mtn SD102AA SD102AD
//! ```
//!
//! It reads the member out of the game's `data.xp3`, loads it through
//! [`krkr_emote::Motion`], and for each named animation prints the draw list
//! hash at several ticks plus the rendered-canvas hash, then a per-animation
//! diff of the draw items. The point is to see whether two animations that the
//! game renders differently differ in the *sampled scene* or only in what the
//! player pipeline would have supplied.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::sync::Arc;

use krkr_emote::{Canvas, Motion, TextureCache, Tint, render_draw_list};

const TICKS: [f32; 5] = [0.0, 10.0, 30.0, 60.0, 120.0];

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// What the file gives the runtime layer (controls, variables, timelines) and
/// how many layers are parameterised — i.e. what a live player can resolve that
/// the static scene builder cannot.
fn scan_all(motion: &Motion, member: &str) {
    let pipeline = eluna::collect_emote_runtime_pipeline(motion.psb());
    let variables = eluna::collect_emote_variables(motion.psb());
    let timelines = eluna::collect_emote_timelines(motion.psb());
    let parameterized = count_parameterized(motion.psb().root.field("object"));
    println!(
        "{member}: eye={} eyebrow={} mouth={} selector={} clamp={} loop={} transition={} physics={} parts={} variables={} timelines={} parameterized_layers={}",
        pipeline.eye_controls.len(),
        pipeline.eyebrow_controls.len(),
        pipeline.mouth_controls.len(),
        pipeline.selector_controls.len(),
        pipeline.clamp_controls.len(),
        pipeline.loop_controls.len(),
        pipeline.transition_controls.len(),
        pipeline.physics_controls.len(),
        pipeline.parts_controls.len(),
        variables.len(),
        timelines.len(),
        parameterized
    );
    if !pipeline.unsupported_fields.is_empty() {
        println!("    unsupported={:?}", pipeline.unsupported_fields);
    }
    for control in &pipeline.eye_controls {
        println!(
            "    eye label={} begin={} end={} blink={}..{} count={} enabled={}",
            control.label,
            control.begin_frame,
            control.end_frame,
            control.blink_interval_min,
            control.blink_interval_max,
            control.blink_frame_count,
            control.blink_enabled
        );
    }
    for variable in &variables {
        println!(
            "    variable {} default={} min={:?} max={:?} frames={}",
            variable.name,
            variable.default_value,
            variable.min_value,
            variable.max_value,
            variable.frames.len()
        );
    }
    for timeline in &timelines {
        println!(
            "    timeline {} difference={} duration={} variables={}",
            timeline.name,
            timeline.is_difference,
            timeline.duration_ticks,
            timeline.variables.len()
        );
    }

    // What the reference's per-frame blend/colour state carries: `bm`, `bp`
    // and the four corner colours of every sprite the static path samples.
    let mut blend_histogram: BTreeMap<u32, usize> = BTreeMap::new();
    let mut colored = 0usize;
    let mut sprites = 0usize;
    let mut bp_nonzero = 0usize;
    for animation in motion.animations() {
        for sample in [0.0f32, 30.0, 90.0, 300.0, 1500.0, 15000.0] {
            let Ok(scene) = motion.scene_at(&animation.name, sample) else {
                continue;
            };
            for sprite in &scene.sprites {
                sprites += 1;
                *blend_histogram.entry(sprite.blend_mode).or_default() += 1;
                if sprite.corner_colors
                    != [if (sprite.blend_mode & 0xF0) == 0x10 {
                        0x8080_80FF
                    } else {
                        0xFFFF_FFFF
                    }; 4]
                {
                    colored += 1;
                }
                if sprite.corner_colors
                    != [if (sprite.blend_mode & 0xF0) == 0x10 {
                        0x8080_80FF
                    } else {
                        0xFFFF_FFFF
                    }; 4]
                {
                    println!(
                        "      colored sprite {} bm={:#x} colors={:?}",
                        sprite.icon_name,
                        sprite.blend_mode,
                        sprite.corner_colors.map(|c| format!("{c:#010x}"))
                    );
                }
                if sprite.blend_parameter != 0.0 {
                    bp_nonzero += 1;
                }
            }
        }
    }
    if sprites > 0 {
        println!(
            "    sprites={sprites} blend_modes={blend_histogram:?} authored_corner_colors={colored} bp_nonzero={bp_nonzero}"
        );
    }
}

fn item_signature(item: &krkr_emote::MotionDrawItem) -> String {
    format!(
        "{}#{} uv[{:.4},{:.4},{:.4},{:.4}] c[{:.2},{:.2}] sz[{:.2},{:.2}] s[{:.3},{:.3}] r{:.2} opa{:.4} z{:.2} vis{} bm{:#x} cc{:#010x}",
        item.texture,
        item.resource_index,
        item.uv[0],
        item.uv[1],
        item.uv[2],
        item.uv[3],
        item.center[0],
        item.center[1],
        item.size[0],
        item.size[1],
        item.scale[0],
        item.scale[1],
        item.rotation_degrees,
        item.opacity,
        item.z,
        item.visible,
        item.blend_mode,
        item.corner_colors[0],
    )
}

/// Counts layers carrying a `parameterize` key anywhere under `object`.
fn count_parameterized(value: Option<&eluna::PsbValue>) -> usize {
    fn walk(value: &eluna::PsbValue, out: &mut usize) {
        match value {
            eluna::PsbValue::List(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            eluna::PsbValue::Object(fields) => {
                if fields.iter().any(|(key, _)| key == "parameterize") {
                    *out += 1;
                }
                for (_, child) in fields {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }
    let mut out = 0;
    if let Some(value) = value {
        walk(value, &mut out);
    }
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let member = args.next().unwrap_or_else(|| "sdmotion/sd102.mtn".into());
    let animations: Vec<String> = args.collect();
    let dir = std::env::var("KRKR_EMOTE_PARQUET_DIR")
        .unwrap_or_else(|_| "/mnt/hdd/games/PARQUET/PARQUET".to_owned());
    let archive =
        krkr_xp3::Xp3Archive::open_file(std::path::Path::new(&dir).join("data.xp3").as_path())
            .expect("data.xp3 opens");

    if std::env::var_os("MOTION_PROBE_SCAN").is_some() {
        let mut members: Vec<String> = archive
            .entries()
            .iter()
            .map(|entry| entry.name.clone())
            .filter(|name| name.to_ascii_lowercase().ends_with(".mtn"))
            .collect();
        members.sort();
        for member in members {
            let mut bytes = Vec::new();
            archive
                .open_by_name(&member)
                .expect("member open")
                .expect("member exists")
                .read_to_end(&mut bytes)
                .expect("member read");
            match Motion::from_bytes(&bytes) {
                Ok(motion) => scan_all(&motion, &member),
                Err(error) => println!("{member}: {error}"),
            }
        }
        return;
    }

    let mut bytes = Vec::new();
    archive
        .open_by_name(&member)
        .expect("member open")
        .expect("member exists")
        .read_to_end(&mut bytes)
        .expect("member read");
    let motion = Arc::new(Motion::from_bytes(&bytes).expect("parse"));

    // The game composites the motion's own screen box onto the layer: the
    // motion origin is the window centre (mtnprobe/M149 convention).
    let (width, height, offset_x, offset_y) = {
        let root = &motion.psb().root;
        let screen = root.field("screenSize").expect("screenSize");
        let width = screen.field_u32("width").unwrap_or(1920);
        let height = screen.field_u32("height").unwrap_or(1080);
        let origin_x = screen.field_f32("originX").unwrap_or(0.0);
        let origin_y = screen.field_f32("originY").unwrap_or(0.0);
        (
            width,
            height,
            width as f32 / 2.0 - origin_x,
            height as f32 / 2.0 - origin_y,
        )
    };
    println!("screenSize = {width}x{height}, origin offset = ({offset_x}, {offset_y})");
    let place = |items: &mut Vec<krkr_emote::MotionDrawItem>| {
        for item in items.iter_mut() {
            item.world_transform[4] += offset_x;
            item.world_transform[5] += offset_y;
        }
    };

    println!("== {member}");
    println!("base_object = {}", motion.base_object());
    println!("spec = {:?}", motion.spec());
    println!("animations = {}", motion.animations().len());
    let names: Vec<&str> = motion
        .animations()
        .iter()
        .map(|animation| animation.name.as_str())
        .collect();
    println!("names = {names:?}");
    println!("sources = {}", motion.sources().len());
    let icons: usize = motion
        .sources()
        .values()
        .map(|source| source.icons.len())
        .sum();
    println!("icons = {icons}");

    for animation in &animations {
        println!("\n-- animation {animation}");
        let mut canvas = Canvas::new(width, height);
        let mut textures = TextureCache::new(Arc::clone(&motion));
        for ticks in TICKS {
            let mut items = motion
                .draw_list(animation, ticks)
                .unwrap_or_else(|error| panic!("{animation}@{ticks}: {error}"));
            let signature: Vec<String> = items.iter().map(item_signature).collect();
            let digest = fnv1a(signature.join("\n").as_bytes());
            place(&mut items);
            canvas.pixels_mut().fill(0);
            let report = render_draw_list(&mut canvas, &items, &mut textures, Tint::IDENTITY);
            let pixel_digest = fnv1a(canvas.pixels());
            let non_zero = canvas
                .pixels()
                .chunks_exact(4)
                .filter(|pixel| pixel[3] != 0)
                .count();
            println!(
                "  ticks={ticks:>6}: items={:>4} list_hash={digest:#018x} pixels={non_zero:>7} pixel_hash={pixel_digest:#018x} drawn={}",
                items.len(),
                report.drawn
            );
            let shown = if std::env::var_os("MOTION_PROBE_ALL").is_some() {
                items.len()
            } else {
                3
            };
            for item in items.iter().take(shown) {
                println!("      {}", item_signature(item));
            }
        }
    }

    if animations.len() >= 2 {
        println!("\n-- diff {} vs {}", animations[0], animations[1]);
        let a = motion.draw_list(&animations[0], 0.0).expect("a");
        let b = motion.draw_list(&animations[1], 0.0).expect("b");
        let sa: BTreeMap<String, usize> = a.iter().fold(BTreeMap::new(), |mut acc, item| {
            *acc.entry(item_signature(item)).or_insert(0) += 1;
            acc
        });
        let sb: BTreeMap<String, usize> = b.iter().fold(BTreeMap::new(), |mut acc, item| {
            *acc.entry(item_signature(item)).or_insert(0) += 1;
            acc
        });
        let mut only_a = Vec::new();
        let mut only_b = Vec::new();
        for (key, count) in &sa {
            match sb.get(key) {
                Some(other) if other == count => {}
                _ => only_a.push(key.clone()),
            }
        }
        for (key, count) in &sb {
            match sa.get(key) {
                Some(other) if other == count => {}
                _ => only_b.push(key.clone()),
            }
        }
        println!("only in {}: {}", animations[0], only_a.len());
        for line in only_a.iter().take(6) {
            println!("   {}", line);
        }
        println!("only in {}: {}", animations[1], only_b.len());
        for line in only_b.iter().take(6) {
            println!("   {}", line);
        }

        let mut textures = TextureCache::new(Arc::clone(&motion));
        let mut left = Canvas::new(width, height);
        let mut right = Canvas::new(width, height);
        let mut items = a.clone();
        place(&mut items);
        render_draw_list(&mut left, &items, &mut textures, Tint::IDENTITY);
        let mut items = b.clone();
        place(&mut items);
        render_draw_list(&mut right, &items, &mut textures, Tint::IDENTITY);
        let mut differing = 0usize;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for (index, (l, r)) in left
            .pixels()
            .chunks_exact(4)
            .zip(right.pixels().chunks_exact(4))
            .enumerate()
        {
            if l == r {
                continue;
            }
            differing += 1;
            let x = index as u32 % width;
            let y = index as u32 / width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        println!(
            "placed pixels differ in {differing} pixels, bbox ({min_x},{min_y})..({max_x},{max_y})"
        );
        println!(
            "  {} pixel_hash={:#018x}  {} pixel_hash={:#018x}",
            animations[0],
            fnv1a(left.pixels()),
            animations[1],
            fnv1a(right.pixels())
        );
    }

    // What the runtime layer (not the static builder) knows about this file.
    let timelines = eluna::collect_emote_timelines(motion.psb());
    let mut timeline_names: Vec<&str> = timelines.iter().map(|t| t.name.as_str()).collect();
    timeline_names.sort();
    println!("\n== timelines ({})", timeline_names.len());
    println!("{timeline_names:?}");
    for timeline in &timelines {
        if animations
            .iter()
            .any(|animation| animation == &timeline.name)
        {
            let frames: usize = timeline
                .variables
                .iter()
                .map(|variable| variable.frames.len())
                .sum();
            println!(
                "   {} difference={} duration={:.1} variables={} frames={}",
                timeline.name,
                timeline.is_difference,
                timeline.duration_ticks,
                timeline.variables.len(),
                frames
            );
            for variable in &timeline.variables {
                println!("        {} frames={:?}", variable.name, variable.frames);
            }
        }
    }
    let variables = eluna::collect_emote_variables(motion.psb());
    println!("\n== variables ({})", variables.len());
    for variable in &variables {
        println!(
            "   {} default={} frames={}",
            variable.name,
            variable.default_value,
            variable.frames.len()
        );
    }

    println!("\n== animation durations");
    for animation in motion.animations() {
        println!(
            "   {} ticks={:.1} layers={}",
            animation.name,
            animation.duration_ticks,
            animation.layers.len()
        );
    }

    println!("\n== face texture hashes");
    for item in motion
        .draw_list(&animations[0], 0.0)
        .expect("aa")
        .iter()
        .chain(motion.draw_list(&animations[1], 0.0).expect("ad").iter())
    {
        if !item.texture.contains("face") {
            continue;
        }
        match motion.texture_pixels(item.resource_index) {
            Ok(texture) => println!(
                "   {} #{} {}x{} hash={:#018x}",
                item.texture,
                item.resource_index,
                texture.width,
                texture.height,
                fnv1a(&texture.rgba)
            ),
            Err(error) => println!("   {} #{} {error}", item.texture, item.resource_index),
        }
    }
}
