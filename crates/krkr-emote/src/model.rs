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
    /// `frameList[i].type`: `0` clears the layer, `2` carries content.
    pub kind: i64,
    /// Verbatim `content.src`, e.g. `src/SD101/bg` or `motion/SD101/ef_moya`.
    pub src: Option<String>,
    /// Verbatim `content.icon`, when the flavor separates it from `src`.
    pub icon: Option<String>,
    /// `src`/`icon` resolved against the source table.
    pub binding: Option<MotionBinding>,
    pub coord: Option<[f32; 3]>,
    /// Verbatim `content.opa`: the file's 0..255 opacity byte
    /// (`motionplayer_nod3d.dll` `FUN_1001d000` keeps it as `value & 0xff`,
    /// defaulting to `0xff`, i.e. fully opaque). This is the raw file value,
    /// kept for diagnostics — the scene applies it as `opa / 255`
    /// (`vendor/eluna/crates/eluna/src/emote.rs:2383`, `:2999`), and the
    /// adapter passes it through unchanged.
    pub opacity: Option<f32>,
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
    let src = content
        .and_then(|content| content.field_str("src"))
        .map(str::to_owned);
    let icon = content
        .and_then(|content| content.field_str("icon"))
        .map(str::to_owned);

    MotionFrame {
        time: value.field_f32("time").unwrap_or(0.0),
        kind: value.field_i64("type").unwrap_or(3),
        binding: resolve_binding(sources, src.as_deref().unwrap_or_default(), icon.as_deref()),
        src,
        icon,
        coord: content.and_then(content_coord),
        opacity: content.and_then(|content| content.field_f32("opa")),
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
