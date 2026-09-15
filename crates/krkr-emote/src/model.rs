//! The motion model: sources, animations, layers and frames.
//!
//! Built from the parsed PSB tree in the file's own (PARQUET) shape, so the
//! model does not depend on the eluna-side normalisation.

use std::collections::BTreeMap;

use eluna::{PsbFile, PsbValue};

use crate::reference::split_icon_reference;

/// One `source.<name>` entry: the icon table a motion's layers resolve against.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionSource {
    pub name: String,
    /// `source.<name>.type`, copied verbatim (PARQUET writes an int here).
    pub kind: Option<PsbValue>,
    /// The source's shared texture in the FreeMote flavor, where every icon is
    /// a sub-rectangle of one resource. PARQUET's flavor hangs the resource on
    /// each icon instead and leaves this `None`.
    pub texture: Option<MotionSourceTexture>,
    pub icons: BTreeMap<String, MotionIcon>,
}

/// `source.<name>.texture`: the pixel resource a FreeMote source's icons carve
/// up, at its own pixel dimensions.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionSourceTexture {
    pub resource_index: u32,
    pub width: f32,
    pub height: f32,
}

/// One drawable icon of a source.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionIcon {
    pub name: String,
    /// Index into the PSB resource table; `Motion::texture_bytes` reads it.
    pub resource_index: u32,
    /// Index of the icon's palette resource, for an 8-bit paletted icon
    /// (`pal` in the file). `None` means the resource is RGBA already.
    pub palette_resource_index: Option<u32>,
    /// Pixel size before [`MotionIcon::resolution`] scaling.
    pub width: f32,
    pub height: f32,
    pub origin_x: f32,
    pub origin_y: f32,
    pub resolution: f32,
    pub clip: Option<MotionClipRect>,
    pub compress: Option<String>,
    pub attr: Option<u32>,
}

impl MotionIcon {
    pub fn resolved_width(&self) -> f32 {
        self.width * self.resolution
    }

    pub fn resolved_height(&self) -> f32 {
        self.height * self.resolution
    }
}

/// The `clip` rectangle some icons carry in addition to their size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionClipRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl MotionClipRect {
    pub fn width(self) -> f32 {
        self.right - self.left
    }

    pub fn height(self) -> f32 {
        self.bottom - self.top
    }
}

/// One `object.<base>.motion.<name>` animation.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionAnimation {
    pub name: String,
    /// `lastTime`/`loopTime`, or the largest layer frame time when absent.
    pub duration_ticks: f32,
    /// The file's `loopTime`: the tick the animation loops back to. `None`
    /// when the file carries no `loopTime` (PARQUET writes `-1` for a motion
    /// that plays once), which is what the plugin's `Player.loopTime` reports
    /// and what decides whether playback ends or wraps.
    pub loop_time: Option<f32>,
    pub layers: Vec<MotionLayer>,
}

impl MotionAnimation {
    /// Layer labels in traversal order, for `getLayerNames()`-style listings.
    pub fn labels(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for layer in &self.layers {
            layer.collect_labels(&mut out);
        }
        out
    }

    /// Smallest and largest frame time over the whole layer tree.
    pub fn frame_range(&self) -> Option<(f32, f32)> {
        let mut range: Option<(f32, f32)> = None;
        for layer in &self.layers {
            layer.include_frame_range(&mut range);
        }
        range
    }
}

/// One layer node of an animation (`motion.layer[i]` and its `children`).
#[derive(Debug, Clone, PartialEq)]
pub struct MotionLayer {
    pub label: Option<String>,
    /// `layer.coordinate`: the static z hint, when the file carries one.
    pub coordinate: Option<i64>,
    pub frames: Vec<MotionFrame>,
    pub children: Vec<MotionLayer>,
}

impl MotionLayer {
    /// Smallest and largest frame time of this node's own `frameList`.
    pub fn frame_range(&self) -> Option<(f32, f32)> {
        frame_range_of(self.frames.iter().map(|frame| frame.time))
    }

    fn collect_labels<'a>(&'a self, out: &mut Vec<&'a str>) {
        if let Some(label) = self.label.as_deref() {
            out.push(label);
        }
        for child in &self.children {
            child.collect_labels(out);
        }
    }

    fn include_frame_range(&self, range: &mut Option<(f32, f32)>) {
        if let Some((min, max)) = self.frame_range() {
            *range = Some(match *range {
                Some((range_min, range_max)) => (range_min.min(min), range_max.max(max)),
                None => (min, max),
            });
        }
        for child in &self.children {
            child.include_frame_range(range);
        }
    }
}

/// One keyframe of a layer (`layer.frameList[i]`).
#[derive(Debug, Clone, PartialEq)]
pub struct MotionFrame {
    pub time: f32,
    /// `frameList[i].type`, as the reference's frame reader decodes it:
    /// `0` is the empty/HOLD frame (`FUN_1001cdc0` marks `frame+0x18`), `2`
    /// is a plain keyframe and `3` a keyframe that interpolates toward its
    /// successor (`frame+0x19`). Only a type-3 frame tweens.
    pub kind: i64,
    /// `content.mask`, the native key-presence bitfield (`FUN_1001cdc0` stores
    /// it at `frame+0x14`). `None` when the frame carries no mask key — the
    /// mask-less FreeMote flavor, whose keys are read by presence alone.
    pub mask: Option<i64>,
    /// Verbatim `content.src`, e.g. `src/SD101/bg` or `motion/SD101/ef_moya`.
    pub src: Option<String>,
    /// Verbatim `content.icon`, when the flavor separates it from `src`.
    pub icon: Option<String>,
    /// `src`/`icon` resolved against the source table.
    pub binding: Option<MotionBinding>,
    /// `content.coord`, only when the mask carries bit `0x2`.
    pub coord: Option<[f32; 3]>,
    /// `content.opa`, only when the mask carries bit `0x400`: the file's
    /// 0..255 opacity byte (`motionplayer_nod3d.dll` `FUN_1001d000` keeps it
    /// as `value & 0xff`, defaulting to `0xff`, i.e. fully opaque). This is
    /// the raw file value, kept for diagnostics — the scene applies it as
    /// `opa / 255` (`vendor/eluna/crates/eluna/src/emote.rs:2385`,
    /// `:3013`), and the adapter passes it through unchanged.
    pub opacity: Option<f32>,
    /// `content.act`, only when the mask carries bit `0x40000`: the action name
    /// the frame triggers.
    pub act: Option<String>,
}

impl MotionFrame {
    /// The native empty frame (`type` 0): it carries no content and leaves the
    /// layer's previous state untouched.
    pub fn is_empty(&self) -> bool {
        self.kind == 0
    }

    /// A frame allowed to interpolate toward its successor (`type` 3).
    pub fn interpolates(&self) -> bool {
        self.kind == 3
    }

    /// Whether the frame's mask carries a key bit. `true` for mask-less
    /// content, which is read permissively.
    pub fn has_key(&self, bit: i64) -> bool {
        self.mask.is_none_or(|mask| mask & bit != 0)
    }
}

/// A frame's `src` reference resolved to a concrete source icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionBinding {
    pub source: String,
    pub icon: String,
    /// Resource index of the icon's pixels.
    pub resource_index: u32,
}

/// One item of a sampled draw list, already in draw order.
///
/// This is the engine-facing shape of eluna's `EmoteStaticSprite`: the plugin
/// decodes the pixels of `resource_index` (see
/// [`crate::Motion::texture_pixels`]), cuts `uv` out of them and places the
/// quad at `center` with `size`/`scale`/`world_transform`.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionDrawItem {
    /// Adapted texture key (`"<source>/<icon>"`).
    pub texture: String,
    pub resource_index: u32,
    pub icon: String,
    /// Texture-space rectangle `[left, top, right, bottom]`, normalised.
    pub uv: [f32; 4],
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub scale: [f32; 2],
    pub rotation_degrees: f32,
    /// The sprite's opacity, `0..=1`. eluna scales the file's `opa` byte by
    /// 1/255 (`vendor/eluna/crates/eluna/src/emote.rs:2999`, with the
    /// interpolated frame state rounded the way the native DLL does at
    /// `:4061`), so 192 becomes ≈0.75 exactly as the reference renders it.
    pub opacity: f32,
    pub z: f32,
    pub visible: bool,
    /// Native decoded-frame blend mode (`bm`), eluna's `EmoteStaticSprite`
    /// `blend_mode` (`vendor/eluna/crates/eluna/src/emote.rs:338`).
    ///
    /// The low nibble selects the blend equation the reference applies; the
    /// `0x10` bit selects the MODULATE2X texture-colour stage. The mapping is
    /// `emoteplayer`'s own: see [`crate::render::SpriteBlend`] for the table
    /// and the DLL addresses it was read from. `0x10` — the neutral,
    /// source-over default — is what a frame without a `bm` field carries.
    pub blend_mode: u32,
    /// Native decoded-frame blend parameter (`bp`). The reference's standard
    /// 2-D consumer does not read it (its parity notes list `bm/bp` retention
    /// and `bp`'s absence from the draw path); it travels with the item so a
    /// caller that needs it is not blocked by the adapter.
    pub blend_parameter: f32,
    /// Native four-corner colours in serialized `0xRRGGBBAA` byte order.
    ///
    /// *Inferred, not recovered*: the four entries are mapped onto the quad in
    /// build order, `[top-left, top-right, bottom-right, bottom-left]`, because
    /// that is the order the rasteriser emits the quad's corners in. The DLL's
    /// own vertex-colour order was not recovered (the D3D build's draw path
    /// feeds four per-vertex values into its shader; neither build's export
    /// pairs them with a corner). Every authored colour in PARQUET's 23 `.mtn`
    /// members has all four corners equal, so the corpus cannot tell the orders
    /// apart — a gradient-tinted sprite would be the first to do so.
    ///
    /// The default is the neutral
    /// MODULATE2X colour `0x808080FF`, whose `0x80` bytes are exactly 1.0 under
    /// that stage's `/128`, so an untinted sprite is bit-identical to one drawn
    /// without colour handling at all.
    pub corner_colors: [u32; 4],
    /// 2x3 affine transform `[m11, m12, m21, m22, tx, ty]`.
    pub world_transform: [f32; 6],
    pub mesh: Option<eluna::EmoteMeshPatch>,
    pub label: Option<String>,
    pub motion: String,
    pub draw_index: usize,
    pub pass: eluna::EmoteDrawPass,
}

impl MotionDrawItem {
    pub fn left(&self) -> f32 {
        self.center[0] - self.size[0] * 0.5
    }

    pub fn top(&self) -> f32 {
        self.center[1] - self.size[1] * 0.5
    }

    pub fn right(&self) -> f32 {
        self.center[0] + self.size[0] * 0.5
    }

    pub fn bottom(&self) -> f32 {
        self.center[1] + self.size[1] * 0.5
    }
}

pub(crate) fn build_sources(psb: &PsbFile) -> BTreeMap<String, MotionSource> {
    let Some(entries) = root_field(psb, "source").and_then(PsbValue::as_object) else {
        return BTreeMap::new();
    };

    let mut sources = BTreeMap::new();
    for (name, value) in entries {
        sources.insert(name.clone(), build_source(name, value));
    }
    sources
}

fn build_source(name: &str, value: &PsbValue) -> MotionSource {
    // FreeMote puts the resource on the source's `texture` and the icons are
    // sub-rectangles of it; PARQUET puts it on each icon. Either way the model
    // reports the resource the icon's pixels live in.
    let texture = value.field("texture");
    let texture_pixel = texture.and_then(|texture| texture.field_u32("pixel"));
    let texture_size = texture
        .and_then(|texture| Some((texture.field_f32("width")?, texture.field_f32("height")?)));

    let mut icons = BTreeMap::new();
    if let Some(entries) = value.field("icon").and_then(PsbValue::as_object) {
        for (icon_name, icon) in entries {
            if let Some(icon) = build_icon(icon_name, icon, texture_pixel) {
                icons.insert(icon_name.clone(), icon);
            }
        }
    }

    MotionSource {
        name: name.to_owned(),
        kind: value.field("type").cloned(),
        texture: match (texture_pixel, texture_size) {
            (Some(resource_index), Some((width, height))) => Some(MotionSourceTexture {
                resource_index,
                width,
                height,
            }),
            _ => None,
        },
        icons,
    }
}

fn build_icon(name: &str, value: &PsbValue, texture_pixel: Option<u32>) -> Option<MotionIcon> {
    Some(MotionIcon {
        name: name.to_owned(),
        resource_index: value.field_u32("pixel").or(texture_pixel)?,
        palette_resource_index: value.field_u32("pal"),
        width: value.field_f32("width")?,
        height: value.field_f32("height")?,
        origin_x: value.field_f32("originX").unwrap_or(0.0),
        origin_y: value.field_f32("originY").unwrap_or(0.0),
        resolution: positive_or(value.field_f32("resolution"), 1.0),
        clip: build_clip(value.field("clip")),
        compress: value.field_str("compress").map(str::to_owned),
        attr: value.field_u32("attr"),
    })
}

fn build_clip(value: Option<&PsbValue>) -> Option<MotionClipRect> {
    let value = value?;
    Some(MotionClipRect {
        left: value.field_f32("left")?,
        top: value.field_f32("top")?,
        right: value.field_f32("right")?,
        bottom: value.field_f32("bottom")?,
    })
}

pub(crate) fn build_animations(
    psb: &PsbFile,
    base_object: &str,
    sources: &BTreeMap<String, MotionSource>,
) -> Vec<MotionAnimation> {
    let Some(motions) = root_field(psb, "object")
        .and_then(|objects| objects.field(base_object))
        .and_then(|base| base.field("motion"))
        .and_then(PsbValue::as_object)
    else {
        return Vec::new();
    };

    motions
        .iter()
        .map(|(name, motion)| {
            let layers = build_layers(motion, sources);
            let loop_time = motion
                .field_f32("loopTime")
                .filter(|loop_time| loop_time.is_finite());
            let mut duration = motion
                .field_f32("lastTime")
                .or(loop_time)
                .filter(|duration| duration.is_finite())
                .unwrap_or(0.0)
                .max(0.0);
            let mut range = None;
            for layer in &layers {
                layer.include_frame_range(&mut range);
            }
            if let Some((_, max)) = range {
                duration = duration.max(max);
            }
            MotionAnimation {
                name: name.clone(),
                duration_ticks: duration,
                loop_time,
                layers,
            }
        })
        .collect()
}

fn build_layers(motion: &PsbValue, sources: &BTreeMap<String, MotionSource>) -> Vec<MotionLayer> {
    let Some(layers) = motion.field("layer").and_then(PsbValue::as_list) else {
        return Vec::new();
    };
    layers
        .iter()
        .map(|layer| build_layer(layer, sources))
        .collect()
}

fn build_layer(value: &PsbValue, sources: &BTreeMap<String, MotionSource>) -> MotionLayer {
    let mut children = Vec::new();
    for key in ["children", "layer"] {
        if let Some(entries) = value.field(key).and_then(PsbValue::as_list) {
            children.extend(entries.iter().map(|child| build_layer(child, sources)));
        }
    }

    MotionLayer {
        label: value.field_str("label").map(str::to_owned),
        coordinate: value.field_i64("coordinate"),
        frames: value
            .field("frameList")
            .and_then(PsbValue::as_list)
            .map(|frames| {
                frames
                    .iter()
                    .map(|frame| build_frame(frame, sources))
                    .collect()
            })
            .unwrap_or_default(),
        children,
    }
}

fn build_frame(value: &PsbValue, sources: &BTreeMap<String, MotionSource>) -> MotionFrame {
    let content = value.field("content");
    // The key-presence bitfield decides which content keys the frame carries;
    // a key outside the mask is not read (`FUN_1001d000`). Mask-less content
    // stays permissive for the FreeMote flavor.
    let mask = content.and_then(|content| content.field_i64("mask"));
    let has_key = |bit: i64| mask.is_none_or(|mask| mask & bit != 0);
    let gated = |bit: i64| content.filter(|_| has_key(bit));

    let src = content
        .and_then(|content| content.field_str("src"))
        .map(str::to_owned);
    let icon = content
        .and_then(|content| content.field_str("icon"))
        .map(str::to_owned);

    MotionFrame {
        time: value.field_f32("time").unwrap_or(0.0),
        kind: value.field_i64("type").unwrap_or(3),
        mask,
        binding: resolve_binding(sources, src.as_deref().unwrap_or_default(), icon.as_deref()),
        src,
        icon,
        coord: gated(0x2).and_then(content_coord),
        opacity: gated(0x400).and_then(|content| content.field_f32("opa")),
        act: gated(0x40000)
            .and_then(|content| content.field_str("act"))
            .map(str::to_owned),
    }
}

fn content_coord(content: &PsbValue) -> Option<[f32; 3]> {
    let coord = content.field("coord")?.as_list()?;
    if coord.len() < 3 {
        return None;
    }
    Some([
        coord[0].as_f32().unwrap_or(0.0),
        coord[1].as_f32().unwrap_or(0.0),
        coord[2].as_f32().unwrap_or(0.0),
    ])
}

/// Resolves a frame's `src`/`icon` against the source table.
pub(crate) fn resolve_binding(
    sources: &BTreeMap<String, MotionSource>,
    src: &str,
    icon: Option<&str>,
) -> Option<MotionBinding> {
    let reference = split_icon_reference(src, icon)?;
    let icon_entry = sources.get(&reference.source)?.icons.get(&reference.icon)?;
    Some(MotionBinding {
        source: reference.source,
        icon: reference.icon,
        resource_index: icon_entry.resource_index,
    })
}

fn frame_range_of(times: impl Iterator<Item = f32>) -> Option<(f32, f32)> {
    let mut range: Option<(f32, f32)> = None;
    for time in times.filter(|time| time.is_finite()) {
        range = Some(match range {
            Some((min, max)) => (min.min(time), max.max(time)),
            None => (time, time),
        });
    }
    range
}

fn root_field<'a>(psb: &'a PsbFile, name: &str) -> Option<&'a PsbValue> {
    psb.root.field(name)
}

fn positive_or(value: Option<f32>, fallback: f32) -> f32 {
    value
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(fallback)
}
