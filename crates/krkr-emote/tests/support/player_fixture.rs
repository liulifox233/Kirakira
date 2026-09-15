//! Synthetic fixture for the live-player tests: a motion whose face is
//! *selected by a variable*, driven by an authored eye control.
//!
//! PARQUET's own `.mtn` files carry no controls at all (every `eyeControl`/
//! `eyebrowControl`/`mouthControl` list in the game's 23 members is empty, and
//! so is every `timeline`), so the reference's player pipeline has nothing to
//! resolve there. This fixture is the smallest file that does have something:
//! one parameterised layer (`parameterize`) whose local sample time is a
//! variable's value, and one `EPEyeControl` whose blink timer writes that
//! variable every tick.

use super::psb_write::{PsbWriter, Value, float, int, list, object, text};

/// One icon of the PARQUET flavor: the resource on the icon, pixels at
/// `width` x `height`.
fn parquet_icon(pixel: &Value, width: i64, height: i64) -> Value {
    object(vec![
        ("pixel", pixel.clone()),
        ("width", int(width)),
        ("height", int(height)),
        ("originX", int(0)),
        ("originY", int(0)),
        ("resolution", int(1)),
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

fn frame(icon: &'static str, time: i64) -> Value {
    object(vec![
        (
            "content",
            object(vec![
                ("src", text("src/hero")),
                ("icon", text(icon)),
                ("coord", list(vec![int(20), int(20), int(0)])),
                ("ox", int(0)),
                ("oy", int(0)),
                ("opa", int(255)),
            ]),
        ),
        ("time", int(time)),
        ("type", int(2)),
    ])
}

/// `bm`, `bp` and a four-corner colour list, all non-default, on the same
/// layer — the sprite state the draw-list boundary must carry through.
///
/// The mask is the bitfield `FUN_1001d000` gates every frame key on
/// (`vendor/eluna/crates/eluna/src/emote.rs:4426-4432`: `0x1` ox/oy, `0x2`
/// coord, `0x200` color, `0x400` opa, `0x20000` bm), so the values are read the
/// way a game's frame reads them. `bp` is not mask-gated (eluna reads it
/// unconditionally, `emote.rs:4490`).
const LIT_MASK: i64 = 0x1 | 0x2 | 0x200 | 0x400 | 0x20000;

/// The blend mode, blend parameter and corner colours [`lit_frame`] authors.
pub const LIT_BLEND_MODE: i64 = 0x13;
pub const LIT_BLEND_PARAMETER: f64 = 0.25;
pub const LIT_CORNER_COLORS: [i64; 4] = [0xFF00_00FF, 0x00FF_00FF, 0x0000_FFFF, 0x8080_80FF];

fn lit_frame(icon: &'static str, time: i64) -> Value {
    object(vec![
        (
            "content",
            object(vec![
                ("src", text("src/hero")),
                ("icon", text(icon)),
                ("mask", int(LIT_MASK)),
                ("coord", list(vec![int(20), int(20), int(0)])),
                ("ox", int(0)),
                ("oy", int(0)),
                ("opa", int(200)),
                ("bm", int(LIT_BLEND_MODE)),
                ("bp", float(LIT_BLEND_PARAMETER as f32)),
                (
                    "color",
                    list(vec![
                        int(LIT_CORNER_COLORS[0]),
                        int(LIT_CORNER_COLORS[1]),
                        int(LIT_CORNER_COLORS[2]),
                        int(LIT_CORNER_COLORS[3]),
                    ]),
                ),
            ]),
        ),
        ("time", int(time)),
        ("type", int(2)),
    ])
}

/// A one-layer motion whose layer samples the `face` variable at local time
/// `(value - 0) * 1 / 1`: the open eye below 0.5, the closed one at and above
/// it. The eye control's interval is a deterministic two ticks and its blink
/// spans twenty, so advancing the player closes the eye inside the first dozen
/// ticks and the frame's icon changes.
pub fn blinking_motion() -> Vec<u8> {
    let mut writer = PsbWriter::default();
    let open_pixel = writer.add_resource(vec![0x40u8; 8 * 8 * 4]);
    let closed_pixel = writer.add_resource(vec![0xC0u8; 8 * 8 * 4]);
    let root = object(vec![
        ("id", text("motion")),
        ("label", text("Synthetic")),
        (
            "metadata",
            object(vec![
                (
                    "variableList",
                    list(vec![object(vec![
                        ("id", text("face")),
                        ("name", text("face")),
                        ("default", float(0.0)),
                        ("min", float(0.0)),
                        ("max", float(1.0)),
                    ])]),
                ),
                (
                    "eyeControl",
                    list(vec![object(vec![
                        ("label", text("face")),
                        ("enabled", int(1)),
                        ("beginFrame", int(0)),
                        ("endFrame", int(1)),
                        ("blinkIntervalMin", float(2.0)),
                        ("blinkIntervalMax", float(2.0)),
                        ("blinkFrameCount", float(20.0)),
                        ("blinkEnabled", int(1)),
                        ("edge", list(vec![])),
                        ("node", list(vec![])),
                    ])]),
                ),
            ]),
        ),
        (
            "source",
            object(vec![(
                "hero",
                object(vec![
                    ("type", int(1)),
                    ("metadata", Value::Null),
                    (
                        "icon",
                        object(vec![
                            ("eye_open", parquet_icon(&open_pixel, 8, 8)),
                            ("eye_closed", parquet_icon(&closed_pixel, 8, 8)),
                        ]),
                    ),
                ]),
            )]),
        ),
        (
            "object",
            object(vec![(
                "hero",
                object(vec![
                    ("metadata", Value::Null),
                    (
                        "motion",
                        object(vec![
                            (
                                "idle",
                                object(vec![
                                    ("lastTime", int(60)),
                                    (
                                        "layer",
                                        list(vec![object(vec![
                                            ("label", text("face")),
                                            ("coordinate", int(0)),
                                            ("children", list(vec![])),
                                            (
                                                "parameterize",
                                                object(vec![
                                                    ("id", text("face")),
                                                    ("rangeBegin", float(0.0)),
                                                    ("rangeEnd", float(1.0)),
                                                    ("division", float(1.0)),
                                                ]),
                                            ),
                                            (
                                                "frameList",
                                                list(vec![
                                                    frame("eye_open", 0),
                                                    frame("eye_closed", 1),
                                                ]),
                                            ),
                                        ])]),
                                    ),
                                ]),
                            ),
                            (
                                "lit",
                                object(vec![
                                    ("lastTime", int(1)),
                                    (
                                        "layer",
                                        list(vec![object(vec![
                                            ("label", text("face")),
                                            ("coordinate", int(0)),
                                            ("children", list(vec![])),
                                            ("frameList", list(vec![lit_frame("eye_open", 0)])),
                                        ])]),
                                    ),
                                ]),
                            ),
                        ]),
                    ),
                ]),
            )]),
        ),
    ]);
    writer.finish(4, &root)
}
