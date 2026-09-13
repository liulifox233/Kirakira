//! Synthetic minimal motions, written byte by byte and loaded end to end.
//!
//! Two files are built from scratch by the `support::psb_write` helper:
//!
//! - `parquet_flavor_motion_loads` — one source with two icons, no `texture`
//!   sub-object and `src/<source>/<icon>` content, i.e. exactly the shape
//!   PARQUET's `.mtn` files use. It checks that the adapter's normalisation
//!   produces a drawable model and that the promised shape holds.
//! - `freemote_flavor_motion_is_left_alone` — the eluna-native flavor (`texture`
//!   sub-object, content naming the texture directly). It checks that the
//!   normalisation is a no-op there.

mod support;

use krkr_emote::{Motion, MotionError};
use support::psb_write::{PsbWriter, Value, float, int, list, object, text};

// ---------------------------------------------------------------------------
// The two synthetic motions and their shared `idle` animation.
// ---------------------------------------------------------------------------

/// The PARQUET-flavor source: `pixel`/`width`/`height` on each icon, no
/// `texture` sub-object.
fn parquet_source(body: &Value, face: &Value) -> Value {
    object(vec![
        ("type", int(1)),
        ("metadata", Value::Null),
        (
            "icon",
            object(vec![
                ("body", parquet_icon(body, 128, 64, 0, 0)),
                ("face", parquet_icon(face, 64, 32, 4, 6)),
            ]),
        ),
    ])
}

fn parquet_icon(pixel: &Value, width: i64, height: i64, origin_x: i64, origin_y: i64) -> Value {
    object(vec![
        ("pixel", pixel.clone()),
        ("width", int(width)),
        ("height", int(height)),
        ("originX", int(origin_x)),
        ("originY", int(origin_y)),
        ("resolution", int(1)),
        ("compress", text("none")),
        ("attr", int(0)),
        (
            "clip",
            object(vec![
                ("left", float(0.0)),
                ("top", float(0.0)),
                ("right", float(width as f32)),
                ("bottom", float(height as f32)),
            ]),
        ),
    ])
}

/// The eluna-native source: one `texture` holding the resource, icons as
/// sub-rectangles of it.
fn freemote_source(pixel: &Value) -> Value {
    object(vec![
        ("type", text("psb")),
        (
            "texture",
            object(vec![
                ("pixel", pixel.clone()),
                ("width", int(16)),
                ("height", int(16)),
            ]),
        ),
        (
            "icon",
            object(vec![(
                "face",
                object(vec![
                    ("left", float(0.0)),
                    ("top", float(0.0)),
                    ("width", float(16.0)),
                    ("height", float(16.0)),
                    ("originX", int(0)),
                    ("originY", int(0)),
                    ("resolution", int(1)),
                ]),
            )]),
        ),
    ])
}

/// `content` of one frame: full opacity at the given coordinate.
///
/// `opa` is the file's 0..255 opacity byte (`motionplayer_nod3d.dll`'s
/// `FUN_1001d000` keeps it as `value & 0xff`, defaulting to `0xff`), so 255 is
/// fully opaque and an absent field means the same.
fn content(src: &'static str, coord: [i64; 3]) -> Value {
    object(vec![
        ("src", text(src)),
        (
            "coord",
            list(vec![int(coord[0]), int(coord[1]), int(coord[2])]),
        ),
        ("ox", int(0)),
        ("oy", int(0)),
        ("opa", int(255)),
    ])
}

/// A two-layer `idle` animation: `body` at z 0 and `face` at z 1, 60 ticks long.
fn idle_animation(
    src_body: &'static str,
    src_face: &'static str,
    icon: Option<&'static str>,
) -> Value {
    let layer = |label: &'static str, src: &'static str, coord: [i64; 3]| {
        let mut frame_content = match content(src, coord) {
            Value::Object(fields) => fields,
            _ => unreachable!(),
        };
        if let Some(icon) = icon {
            frame_content.push(("icon", text(icon)));
        }
        object(vec![
            ("label", text(label)),
            ("coordinate", int(coord[2])),
            ("children", list(vec![])),
            (
                "frameList",
                list(vec![
                    object(vec![
                        ("content", Value::Object(frame_content)),
                        ("time", int(0)),
                        ("type", int(2)),
                    ]),
                    object(vec![("time", int(60)), ("type", int(0))]),
                ]),
            ),
        ])
    };

    object(vec![
        ("lastTime", int(60)),
        (
            "layer",
            list(vec![
                layer("body", src_body, [10, 20, 0]),
                layer("face", src_face, [30, 40, 1]),
            ]),
        ),
    ])
}

fn motion_root(
    source: Value,
    src_body: &'static str,
    src_face: &'static str,
    icon: Option<&'static str>,
) -> Value {
    object(vec![
        ("id", text("motion")),
        ("label", text("Synthetic")),
        ("metadata", Value::Null),
        ("source", object(vec![("hero", source)])),
        (
            "object",
            object(vec![(
                "hero",
                object(vec![
                    ("metadata", Value::Null),
                    (
                        "motion",
                        object(vec![("idle", idle_animation(src_body, src_face, icon))]),
                    ),
                ]),
            )]),
        ),
    ])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn parquet_flavor_motion_loads() {
    let mut writer = PsbWriter::default();
    let body_pixels = writer.add_resource(vec![1u8, 2, 3, 4, 5, 6]);
    let face_pixels = writer.add_resource(vec![9u8, 8, 7]);
    let root = motion_root(
        parquet_source(&body_pixels, &face_pixels),
        "src/hero/body",
        "src/hero/face",
        None,
    );
    let bytes = writer.finish(4, &root);

    let motion = Motion::from_bytes(&bytes).expect("synthetic v4 motion loads");

    assert_eq!(motion.psb().version, 4);
    assert_eq!(motion.base_object(), "hero");

    // Model shape: one source with two icons, one 60-tick animation.
    assert_eq!(motion.sources().len(), 1);
    let hero = &motion.sources()["hero"];
    assert_eq!(hero.icons.len(), 2);
    assert_eq!(hero.icons["body"].resource_index, 0);
    assert_eq!(hero.icons["body"].resolved_width(), 128.0);
    assert_eq!(hero.icons["body"].resolved_height(), 64.0);
    assert_eq!(hero.icons["face"].resource_index, 1);
    assert_eq!(hero.icons["face"].origin_x, 4.0);
    assert_eq!(hero.icons["face"].resolution, 1.0);

    assert_eq!(motion.animations().len(), 1);
    let animation = &motion.animations()[0];
    assert_eq!(animation.name, "idle");
    assert_eq!(animation.duration_ticks, 60.0);
    assert_eq!(animation.frame_range(), Some((0.0, 60.0)));
    assert_eq!(animation.labels(), vec!["body", "face"]);
    assert_eq!(animation.layers[0].frame_range(), Some((0.0, 60.0)));

    // Frames keep the file's `src` and resolve it to the icon's resource.
    let frame = &animation.layers[0].frames[0];
    assert_eq!(frame.time, 0.0);
    assert_eq!(frame.kind, 2);
    assert_eq!(frame.src.as_deref(), Some("src/hero/body"));
    let binding = frame.binding.as_ref().expect("src resolves to an icon");
    assert_eq!(binding.source, "hero");
    assert_eq!(binding.icon, "body");
    assert_eq!(binding.resource_index, 0);

    // The adaptation: one synthetic texture per icon. The file's 0..255 `opa`
    // is *not* touched — eluna reads it as a byte and divides by 255.
    let report = motion.normalize_report();
    assert_eq!(report.sources, 1);
    assert_eq!(report.synthesized_textures, 2);
    assert_eq!(report.rewritten_contents, 2);
    assert_eq!(report.unresolved_icon_references, 0);
    assert_eq!(report.icons_without_pixel, 0);
    assert!(motion.schema().textures.contains_key("hero/body"));
    assert!(motion.schema().textures.contains_key("hero/face"));

    // The draw list: two sprites, body first, textured from the resources in
    // the container. eluna's MeshTransform.None rule subtracts the icon origin
    // from the content offset, so `face` centres at -4/-6 and the layer
    // coordinate lives in the affine transform.
    let items = motion.draw_list("idle", 0.0).expect("idle samples");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].texture, "hero/body");
    assert_eq!(items[0].resource_index, 0);
    assert_eq!(items[0].uv, [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(items[0].size, [128.0, 64.0]);
    assert_eq!(items[0].center, [0.0, 0.0]);
    assert_eq!(items[0].z, 0.0);
    assert_eq!(items[0].opacity, 1.0);
    assert!(items[0].visible);
    assert_eq!(items[0].label.as_deref(), Some("body"));
    assert_eq!(items[0].draw_index, 0);
    assert_eq!(
        motion.texture_bytes(items[0].resource_index),
        Some(&[1u8, 2, 3, 4, 5, 6][..])
    );

    assert_eq!(items[1].texture, "hero/face");
    assert_eq!(items[1].resource_index, 1);
    assert_eq!(items[1].size, [64.0, 32.0]);
    assert_eq!(items[1].center, [-4.0, -6.0]);
    assert_eq!(items[1].z, 1.0);
    assert_eq!(items[1].draw_index, 1);
    assert_eq!(
        motion.texture_bytes(items[1].resource_index),
        Some(&[9u8, 8, 7][..])
    );

    // Sampling later in the animation keeps the layer state; at the clear
    // frame's own tick (60) the layer is cleared. eluna does not wrap the
    // clock itself — the caller does (`motion_player.rs`'s `advance_player`
    // wraps on `loopTime`); the pinned tree wrapped here instead.
    assert_eq!(motion.draw_list("idle", 30.0).unwrap().len(), 2);
    assert_eq!(motion.draw_list("idle", 59.9).unwrap().len(), 2);
    assert_eq!(motion.draw_list("idle", 60.0).unwrap().len(), 0);
    assert_eq!(motion.scene_at("idle", 0.0).unwrap().base_object, "hero");

    assert!(matches!(
        motion.draw_list("nope", 0.0),
        Err(MotionError::MissingAnimation(name)) if name == "nope"
    ));
}

#[test]
fn freemote_flavor_motion_is_left_alone() {
    let mut writer = PsbWriter::default();
    let pixels = writer.add_resource(vec![7u8; 16]);
    let root = motion_root(freemote_source(&pixels), "hero", "hero", Some("face"));
    let bytes = writer.finish(3, &root);

    let motion = Motion::from_bytes(&bytes).expect("synthetic v3 motion loads");

    assert_eq!(motion.psb().version, 3);
    assert_eq!(motion.base_object(), "hero");
    assert_eq!(motion.sources()["hero"].icons.len(), 1);

    // Nothing to synthesise: the file already carries a `texture`.
    let report = motion.normalize_report();
    assert_eq!(report.synthesized_textures, 0);
    assert_eq!(report.rewritten_contents, 0);
    assert_eq!(report.unresolved_icon_references, 0);

    let items = motion.draw_list("idle", 0.0).expect("idle samples");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].texture, "hero");
    assert_eq!(items[0].resource_index, 0);
    assert_eq!(items[0].icon, "face");
    assert_eq!(items[0].size, [16.0, 16.0]);
    assert_eq!(motion.texture_bytes(0), Some(&[7u8; 16][..]));
}

#[test]
fn freemote_source_with_src_paths_is_rewritten() {
    // The same eluna-native source, but content names the icon with the
    // PARQUET-style path. The adaptation must point `src` at the existing
    // `texture` key instead of synthesising anything.
    let mut writer = PsbWriter::default();
    let pixels = writer.add_resource(vec![5u8; 4]);
    let root = motion_root(
        freemote_source(&pixels),
        "src/hero/face",
        "src/hero/face",
        None,
    );
    let bytes = writer.finish(4, &root);

    let motion = Motion::from_bytes(&bytes).expect("synthetic v4 motion loads");

    let report = motion.normalize_report();
    assert_eq!(report.sources, 1);
    assert_eq!(report.synthesized_textures, 0);
    assert_eq!(report.rewritten_contents, 2);
    assert_eq!(report.unresolved_icon_references, 0);

    let items = motion.draw_list("idle", 0.0).expect("idle samples");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].texture, "hero");
    assert_eq!(items[0].resource_index, 0);
    assert_eq!(items[0].icon, "face");
}

#[test]
fn rejects_non_psb_bytes() {
    assert!(matches!(
        Motion::from_bytes(b"not a PSB file at all"),
        Err(MotionError::Psb(_))
    ));
    assert!(matches!(Motion::from_bytes(&[]), Err(MotionError::Psb(_))));
}

/// In the FreeMote flavor an icon is a sub-rectangle of a shared texture, so
/// the *resource* decodes at the texture's dimensions and the icon rectangle
/// travels as the draw item's `uv`.
#[test]
fn freemote_sub_rect_icons_decode_their_shared_texture() {
    /// The eluna-native source with one icon that is a sub-rectangle of the
    /// texture rather than the whole of it.
    fn sub_rect_source(pixel: &Value, left: i64, top: i64, width: i64, height: i64) -> Value {
        object(vec![
            ("type", text("psb")),
            (
                "texture",
                object(vec![
                    ("pixel", pixel.clone()),
                    ("width", int(16)),
                    ("height", int(16)),
                ]),
            ),
            (
                "icon",
                object(vec![(
                    "face",
                    object(vec![
                        ("left", float(left as f32)),
                        ("top", float(top as f32)),
                        ("width", float(width as f32)),
                        ("height", float(height as f32)),
                        ("originX", int(0)),
                        ("originY", int(0)),
                        ("resolution", int(1)),
                    ]),
                )]),
            ),
        ])
    }

    let mut writer = PsbWriter::default();
    let pixels = writer.add_resource(vec![3u8; 16 * 16 * 4]);
    let root = motion_root(
        sub_rect_source(&pixels, 4, 8, 8, 4),
        "hero",
        "hero",
        Some("face"),
    );
    let bytes = writer.finish(4, &root);
    let motion = Motion::from_bytes(&bytes).expect("freemote motion loads");

    let decoded = motion
        .texture_pixels(0)
        .expect("the shared texture decodes");
    assert_eq!(
        (decoded.width, decoded.height),
        (16, 16),
        "the resource decodes at the texture's size, not the icon's"
    );
    assert_eq!(decoded.rgba.len(), 16 * 16 * 4);

    let items = motion.draw_list("idle", 0.0).expect("idle samples");
    assert!(!items.is_empty());
    assert_eq!(
        items[0].uv,
        [0.25, 0.5, 0.75, 0.75],
        "the icon travels as its sub-rectangle of the texture"
    );
    assert_eq!(items[0].size, [8.0, 4.0], "the icon's own size");
}

// ---------------------------------------------------------------------------
// Why the remaining adaptation passes are still needed (M127)
//
// Each test drives the same bytes through the raw eluna schema/scene path and
// through `Motion` (which normalises), so the vendored behaviour that forces
// the pass is pinned next to the pass itself.
// ---------------------------------------------------------------------------

/// PARQUET's per-icon `pixel` flavor is still not served by the vendored tree:
/// `collect_textures` skips a source that has no `texture` sub-object
/// (`vendor/eluna/crates/eluna/src/emote.rs:1585-1587`), and `src/<source>/
/// <icon>` is not a texture key. The adapter's synthesis is what makes those
/// files load, so the pass stays.
#[test]
fn parquet_flavor_icons_need_the_synthesized_sources() {
    let mut writer = PsbWriter::default();
    let body_pixels = writer.add_resource(vec![1u8, 2, 3, 4, 5, 6]);
    let face_pixels = writer.add_resource(vec![9u8, 8, 7]);
    let root = motion_root(
        parquet_source(&body_pixels, &face_pixels),
        "src/hero/body",
        "src/hero/face",
        None,
    );
    let bytes = writer.finish(4, &root);

    // Raw: the schema sees no texture for the source at all, and a layer
    // naming `src/hero/body` draws nothing.
    let raw = krkr_emote::PsbFile::parse(&bytes).expect("raw parse");
    let raw_schema = krkr_emote::EmoteModelSchema::from_psb(&raw).expect("raw schema");
    assert!(
        raw_schema.textures.is_empty(),
        "the vendored tree reads no `texture` entry for PARQUET's per-icon sources"
    );
    let raw_scene = raw_schema
        .build_motion_scene_at_with_resources_and_variables(
            &raw,
            &bytes,
            "idle",
            0.0,
            &Default::default(),
        )
        .expect("raw scene builds");
    assert!(
        raw_scene.sprites.is_empty(),
        "`src/<source>/<icon>` resolves to no texture in the raw tree"
    );

    // Adapted: the synthetic sources exist and the same frame draws.
    let motion = Motion::from_bytes(&bytes).expect("adapted load");
    assert!(motion.schema().textures.contains_key("hero/body"));
    assert!(motion.schema().textures.contains_key("hero/face"));
    assert_eq!(motion.draw_list("idle", 0.0).expect("sample").len(), 2);
}

/// A one-layer `fade` motion with `parameterize: null` — PARQUET's spelling
/// for a layer that is *not* parameterised — whose second keyframe (opa 128)
/// sits at tick 30.
fn fade_root(source: Value) -> Value {
    let frame = |time: i64, opa: i64| {
        object(vec![
            (
                "content",
                object(vec![
                    ("src", text("hero")),
                    ("icon", text("face")),
                    ("coord", list(vec![int(0), int(0), int(0)])),
                    ("opa", int(opa)),
                ]),
            ),
            ("time", int(time)),
            ("type", int(2)),
        ])
    };
    object(vec![
        ("id", text("motion")),
        ("label", text("Synthetic")),
        ("source", object(vec![("hero", source)])),
        (
            "object",
            object(vec![(
                "hero",
                object(vec![(
                    "motion",
                    object(vec![(
                        "fade",
                        object(vec![
                            ("lastTime", int(60)),
                            (
                                "layer",
                                list(vec![object(vec![
                                    ("label", text("body")),
                                    ("coordinate", int(0)),
                                    ("parameterize", Value::Null),
                                    ("children", list(vec![])),
                                    ("frameList", list(vec![frame(0, 255), frame(30, 128)])),
                                ])]),
                            ),
                        ]),
                    )]),
                )]),
            )]),
        ),
    ])
}

/// A present-but-null `parameterize` still freezes the layer at local time 0
/// in the vendored tree (`emote.rs:4468-4474` with `resolve_parameterize`
/// returning `None` for `Null` at `:4549-4557`), so dropping the field is what
/// lets the later keyframe activate. The same bytes are sampled raw and
/// through the adapter, and the file's `opa` byte reaches eluna's `/255`
/// reading untouched (raw 128 → 128/255).
#[test]
fn parameterize_null_freezes_without_the_strip() {
    let mut writer = PsbWriter::default();
    let pixels = writer.add_resource(vec![1u8; 16 * 16 * 4]);
    let root = fade_root(freemote_source(&pixels));
    let bytes = writer.finish(4, &root);

    // Raw: at tick 30 the frozen layer still shows its tick-0 frame (opa 255).
    let raw = krkr_emote::PsbFile::parse(&bytes).expect("raw parse");
    let raw_schema = krkr_emote::EmoteModelSchema::from_psb(&raw).expect("raw schema");
    let raw_scene = raw_schema
        .build_motion_scene_at_with_resources_and_variables(
            &raw,
            &bytes,
            "fade",
            30.0,
            &Default::default(),
        )
        .expect("raw scene builds");
    assert_eq!(raw_scene.sprites.len(), 1);
    assert_eq!(
        raw_scene.sprites[0].opacity, 1.0,
        "null `parameterize` freezes the layer at local time 0"
    );

    // Adapted: the field is gone, the tick-30 frame applies, and the `opa`
    // byte is passed through unchanged (no rescale).
    let motion = Motion::from_bytes(&bytes).expect("adapted load");
    assert_eq!(motion.normalize_report().dropped_null_parameterize, 1);
    assert_eq!(
        motion.animations()[0].layers[0].frames[1].opacity,
        Some(128.0),
        "the model keeps the file's verbatim `opa` byte"
    );
    let items = motion.draw_list("fade", 30.0).expect("sample");
    assert_eq!(items.len(), 1);
    assert!(
        (items[0].opacity - 128.0 / 255.0).abs() < 1e-6,
        "opa 128 renders as 128/255, got {}",
        items[0].opacity
    );
    assert_eq!(
        motion.draw_list("fade", 0.0).expect("sample")[0].opacity,
        1.0,
        "the tick-0 frame is fully opaque"
    );
}
