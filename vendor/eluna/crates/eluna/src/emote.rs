//! Emote-specific PSB schema extraction and static draw-list generation.
//!
//! This module owns Emote layer/frame traversal and the recovered pieces of the
//! StepFrameMeshChain draw-list path. Semantics that are not confirmed from the
//! original driver are carried as explicit draw/runtime metadata instead of
//! being silently guessed.

use crate::{PsbFile, PsbValue};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
#[cfg(debug_assertions)]
use std::sync::{Mutex, OnceLock};

#[cfg(debug_assertions)]
static MISSING_PARAMETER_VARIABLES: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteModelSchema {
    pub base_object: String,
    pub spec: Option<String>,
    pub textures: BTreeMap<String, EmoteTextureSource>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteMotionInfo {
    pub name: String,
    pub duration_ticks: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteTextureSource {
    pub name: String,
    pub resource_index: u32,
    pub width: u32,
    pub height: u32,
    pub format: Option<String>,
    pub compress: Option<String>,
    pub bit_count: Option<u32>,
    pub icons: BTreeMap<String, EmoteTextureIcon>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteTextureIcon {
    pub texture_name: String,
    pub name: String,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub origin_x: f32,
    pub origin_y: f32,
    pub resolution: f32,
    pub attr: Option<u32>,
}

impl EmoteTextureIcon {
    pub fn resolved_width(&self) -> f32 {
        self.width * self.resolution
    }

    pub fn resolved_height(&self) -> f32 {
        self.height * self.resolution
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmoteMeshPatch {
    pub division_x: u32,
    pub division_y: u32,
    /// Sixteen cubic Bezier patch control points, row-major, each point in
    /// normalized local sprite space. The identity patch is
    /// `(col / 3, row / 3)`.
    pub control_points: [[f32; 2]; 16],
}

impl EmoteMeshPatch {
    pub fn identity(division_x: u32, division_y: u32) -> Self {
        let mut control_points = [[0.0; 2]; 16];
        for row in 0..4 {
            for col in 0..4 {
                let index = row * 4 + col;
                control_points[index] = [col as f32 / 3.0, row as f32 / 3.0];
            }
        }
        Self {
            division_x: division_x.max(1),
            division_y: division_y.max(1),
            control_points,
        }
    }

    pub fn sample(&self, u: f32, v: f32) -> [f32; 2] {
        let bu = cubic_basis(u.clamp(0.0, 1.0));
        let bv = cubic_basis(v.clamp(0.0, 1.0));
        let mut out = [0.0f32; 2];
        for row in 0..4 {
            for col in 0..4 {
                let w = bv[row] * bu[col];
                let p = self.control_points[row * 4 + col];
                out[0] += p[0] * w;
                out[1] += p[1] * w;
            }
        }
        out
    }

    pub fn combined_with(&self, next: &EmoteMeshPatch) -> EmoteMeshPatch {
        let identity = EmoteMeshPatch::identity(
            self.division_x.max(next.division_x),
            self.division_y.max(next.division_y),
        );
        let mut out = identity.clone();
        for i in 0..16 {
            out.control_points[i][0] = identity.control_points[i][0]
                + (self.control_points[i][0] - identity.control_points[i][0])
                + (next.control_points[i][0] - identity.control_points[i][0]);
            out.control_points[i][1] = identity.control_points[i][1]
                + (self.control_points[i][1] - identity.control_points[i][1])
                + (next.control_points[i][1] - identity.control_points[i][1]);
        }
        out
    }

    pub fn interpolate(a: &EmoteMeshPatch, b: &EmoteMeshPatch, t: f32) -> EmoteMeshPatch {
        let t = t.clamp(0.0, 1.0);
        let mut out = EmoteMeshPatch::identity(a.division_x.max(b.division_x), a.division_y.max(b.division_y));
        for i in 0..16 {
            out.control_points[i][0] = a.control_points[i][0] + (b.control_points[i][0] - a.control_points[i][0]) * t;
            out.control_points[i][1] = a.control_points[i][1] + (b.control_points[i][1] - a.control_points[i][1]) * t;
        }
        out
    }

    pub fn control_bounds(&self) -> (f32, f32, f32, f32) {
        let mut min_x = self.control_points[0][0];
        let mut min_y = self.control_points[0][1];
        let mut max_x = self.control_points[0][0];
        let mut max_y = self.control_points[0][1];
        for p in &self.control_points[1..] {
            min_x = min_x.min(p[0]);
            min_y = min_y.min(p[1]);
            max_x = max_x.max(p[0]);
            max_y = max_y.max(p[1]);
        }
        (min_x, min_y, max_x, max_y)
    }
}

fn cubic_basis(t: f32) -> [f32; 4] {
    let s = 1.0 - t;
    [s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t]
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteStaticScene {
    pub base_object: String,
    pub sprites: Vec<EmoteStaticSprite>,
    pub bounds: Option<EmoteSceneBounds>,
    pub draw_frame_info: Vec<EmoteDrawFrameInfo>,
}

impl EmoteStaticScene {
    /// Keeps only sprites that belong to one motion name and recomputes bounds.
    ///
    /// This is a preview helper. The original runtime motion selection still
    /// needs the `FindMotion` / timeline code path from the DLL.
    pub fn filter_motion(mut self, motion_name: &str) -> Self {
        self.sprites.retain(|sprite| sprite.motion_name == motion_name);
        self.bounds = compute_bounds(&self.sprites);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmoteSceneBounds {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl EmoteSceneBounds {
    pub fn width(self) -> f32 {
        self.max_x - self.min_x
    }

    pub fn height(self) -> f32 {
        self.max_y - self.min_y
    }

    pub fn center(self) -> [f32; 2] {
        [(self.min_x + self.max_x) * 0.5, (self.min_y + self.max_y) * 0.5]
    }

    fn include_rect(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        self.min_x = self.min_x.min(left);
        self.min_y = self.min_y.min(top);
        self.max_x = self.max_x.max(right);
        self.max_y = self.max_y.max(bottom);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteStaticSprite {
    pub label: Option<String>,
    pub motion_name: String,
    pub texture_name: String,
    pub texture_resource_index: u32,
    pub texture_width: u32,
    pub texture_height: u32,
    pub texture_format: Option<String>,
    pub icon_name: String,
    pub z: f32,
    pub opacity: f32,
    pub visible: bool,
    pub center_x: f32,
    pub center_y: f32,
    pub width: f32,
    pub height: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation_degrees: f32,
    pub world_transform: [f32; 6],
    pub uv_left: f32,
    pub uv_top: f32,
    pub uv_right: f32,
    pub uv_bottom: f32,
    pub mesh: Option<EmoteMeshPatch>,
    pub draw_frame_info: EmoteDrawFrameInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteDrawFrameInfo {
    pub layer_label: Option<String>,
    pub draw_index: usize,
    pub path: String,
    pub mesh_transform: i64,
    pub mesh_combine: bool,
    pub mesh_sync_child_mask: i64,
    pub mesh_sync_child_coord: bool,
    pub mesh_sync_child_angle: bool,
    pub mesh_sync_child_zoom: bool,
    pub mesh_sync_child_shape: bool,
    pub join_target: Option<i64>,
    pub inherit_mask: Option<i64>,
    pub inherit_parent: bool,
    pub inherit_opacity: bool,
    pub inherit_shape: bool,
    pub inherit_angle: bool,
    pub transform_order: Vec<i64>,
    pub coordinate: Option<i64>,
    pub ground_correction: Option<i64>,
    pub stencil_type: i64,
    pub stencil_composite_mask_layer_list: Vec<String>,
    pub control_parameter: Option<String>,
    pub control_value: Option<f32>,
    pub local_time_ticks: Option<f32>,
    pub frame_index: Option<usize>,
    pub next_frame_index: Option<usize>,
    pub interpolation_t: f32,
    pub pass: EmoteDrawPass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmoteDrawPass {
    Normal,
    MaskGeneration,
    StencilCompositeMask,
    Filtered,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteStepFrameInput {
    pub motion_name: String,
    pub time_ticks: f32,
    pub variables: BTreeMap<String, f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteStepFrameLayerState {
    pub path: String,
    pub transform: [f32; 6],
    pub opacity: f32,
    pub visible: bool,
    pub draw_frame_info: EmoteDrawFrameInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteStepFrameMeshState {
    pub path: String,
    pub texture_resource_index: u32,
    pub texture_name: String,
    pub uv_rect: [f32; 4],
    pub mesh: Option<EmoteMeshPatch>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteMeshChainNode {
    pub layer: EmoteStepFrameLayerState,
    pub mesh: Option<EmoteStepFrameMeshState>,
    pub children: Vec<EmoteMeshChainNode>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmoteStepFrameOutput {
    pub layers: Vec<EmoteStepFrameLayerState>,
    pub meshes: Vec<EmoteStepFrameMeshState>,
    pub draw_frame_info: Vec<EmoteDrawFrameInfo>,
}

impl EmoteStaticSprite {
    pub fn left(&self) -> f32 {
        self.center_x - self.width * 0.5
    }

    pub fn top(&self) -> f32 {
        self.center_y - self.height * 0.5
    }

    pub fn right(&self) -> f32 {
        self.center_x + self.width * 0.5
    }

    pub fn bottom(&self) -> f32 {
        self.center_y + self.height * 0.5
    }

    pub fn bounds_rect(&self) -> (f32, f32, f32, f32) {
        let (left, top, right, bottom) = if let Some(mesh) = &self.mesh {
            let left = self.left();
            let top = self.top();
            let (min_u, min_v, max_u, max_v) = mesh.control_bounds();
            (
                left + min_u * self.width,
                top + min_v * self.height,
                left + max_u * self.width,
                top + max_v * self.height,
            )
        } else {
            (self.left(), self.top(), self.right(), self.bottom())
        };
        bounds_after_sprite_transform(self, left, top, right, bottom)
    }
}

fn bounds_after_sprite_transform(sprite: &EmoteStaticSprite, left: f32, top: f32, right: f32, bottom: f32) -> (f32, f32, f32, f32) {
    let points = [
        transform_emote_sprite_point(sprite, [left, top]),
        transform_emote_sprite_point(sprite, [left, bottom]),
        transform_emote_sprite_point(sprite, [right, top]),
        transform_emote_sprite_point(sprite, [right, bottom]),
    ];
    let mut min_x = points[0][0];
    let mut min_y = points[0][1];
    let mut max_x = points[0][0];
    let mut max_y = points[0][1];
    for p in &points[1..] {
        min_x = min_x.min(p[0]);
        min_y = min_y.min(p[1]);
        max_x = max_x.max(p[0]);
        max_y = max_y.max(p[1]);
    }
    (min_x, min_y, max_x, max_y)
}

fn transform_emote_sprite_point(sprite: &EmoteStaticSprite, point: [f32; 2]) -> [f32; 2] {
    let sx = finite_or(sprite.scale_x, 1.0);
    let sy = finite_or(sprite.scale_y, 1.0);
    let angle = finite_or(sprite.rotation_degrees, 0.0).to_radians();
    let cos = angle.cos();
    let sin = angle.sin();
    let dx = (point[0] - sprite.center_x) * sx;
    let dy = (point[1] - sprite.center_y) * sy;
    let local = [
        sprite.center_x + dx * cos - dy * sin,
        sprite.center_y + dx * sin + dy * cos,
    ];
    transform_from_array(sprite.world_transform).apply(local)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmoteSchemaError {
    RootIsNotObject,
    MissingObjectTable,
    MissingSourceTable,
    MissingBaseObject,
    InvalidSourceTexture { source: String },
    InvalidTextureResource { source: String },
    InvalidIcon { source: String, icon: String },
}

impl fmt::Display for EmoteSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmoteSchemaError::RootIsNotObject => write!(f, "PSB root is not an object"),
            EmoteSchemaError::MissingObjectTable => write!(f, "Emote PSB has no object table"),
            EmoteSchemaError::MissingSourceTable => write!(f, "Emote PSB has no source table"),
            EmoteSchemaError::MissingBaseObject => write!(f, "cannot determine base Emote object"),
            EmoteSchemaError::InvalidSourceTexture { source } => {
                write!(f, "invalid source texture entry for {source}")
            }
            EmoteSchemaError::InvalidTextureResource { source } => {
                write!(f, "invalid or missing texture resource for {source}")
            }
            EmoteSchemaError::InvalidIcon { source, icon } => {
                write!(f, "invalid icon {source}/{icon}")
            }
        }
    }
}

impl Error for EmoteSchemaError {}

impl EmoteModelSchema {
    pub fn from_psb(psb: &PsbFile) -> Result<Self, EmoteSchemaError> {
        let root = psb.root.as_object().ok_or(EmoteSchemaError::RootIsNotObject)?;
        let root_value = PsbValue::Object(root.to_vec());
        let object = root_value.field("object").ok_or(EmoteSchemaError::MissingObjectTable)?;
        let source = root_value.field("source").ok_or(EmoteSchemaError::MissingSourceTable)?;

        let base_object = find_base_object(&root_value, object)?;
        let spec = root_value.field_str("spec").map(str::to_owned);
        let textures = collect_textures(source)?;

        Ok(Self { base_object, spec, textures })
    }

    pub fn motion_infos(&self, psb: &PsbFile) -> Result<Vec<EmoteMotionInfo>, EmoteSchemaError> {
        let root = psb.root.as_object().ok_or(EmoteSchemaError::RootIsNotObject)?;
        let root_value = PsbValue::Object(root.to_vec());
        let object_table = root_value.field("object").ok_or(EmoteSchemaError::MissingObjectTable)?;
        let base = object_table
            .field(&self.base_object)
            .ok_or(EmoteSchemaError::MissingBaseObject)?;
        let Some(motions) = base.field("motion").and_then(PsbValue::as_object) else {
            return Ok(Vec::new());
        };

        let mut out = Vec::with_capacity(motions.len());
        for (name, motion) in motions {
            out.push(EmoteMotionInfo {
                name: name.clone(),
                duration_ticks: motion_duration_ticks(motion).unwrap_or(0.0),
            });
        }
        Ok(out)
    }

    pub fn default_motion_name(&self, psb: &PsbFile) -> Result<Option<String>, EmoteSchemaError> {
        Ok(self.motion_infos(psb)?.into_iter().next().map(|info| info.name))
    }

    pub fn build_motion_scene_at(
        &self,
        psb: &PsbFile,
        motion_name: &str,
        time_ticks: f32,
    ) -> Result<EmoteStaticScene, EmoteSchemaError> {
        self.build_motion_scene_internal(psb, None, motion_name, time_ticks, &BTreeMap::new())
    }

    pub fn build_motion_scene_at_with_variables(
        &self,
        psb: &PsbFile,
        motion_name: &str,
        time_ticks: f32,
        variables: &BTreeMap<String, f32>,
    ) -> Result<EmoteStaticScene, EmoteSchemaError> {
        self.build_motion_scene_internal(psb, None, motion_name, time_ticks, variables)
    }

    pub fn build_motion_scene_at_with_resources_and_variables(
        &self,
        psb: &PsbFile,
        psb_data: &[u8],
        motion_name: &str,
        time_ticks: f32,
        variables: &BTreeMap<String, f32>,
    ) -> Result<EmoteStaticScene, EmoteSchemaError> {
        self.build_motion_scene_internal(psb, Some(psb_data), motion_name, time_ticks, variables)
    }

    fn build_motion_scene_internal(
        &self,
        psb: &PsbFile,
        psb_data: Option<&[u8]>,
        motion_name: &str,
        time_ticks: f32,
        variables: &BTreeMap<String, f32>,
    ) -> Result<EmoteStaticScene, EmoteSchemaError> {
        let root = psb.root.as_object().ok_or(EmoteSchemaError::RootIsNotObject)?;
        let root_value = PsbValue::Object(root.to_vec());
        let object_table = root_value.field("object").ok_or(EmoteSchemaError::MissingObjectTable)?;
        let root_parameter_table = root_value.field("parameter").and_then(PsbValue::as_list);
        let base = object_table
            .field(&self.base_object)
            .ok_or(EmoteSchemaError::MissingBaseObject)?;
        let motion = base
            .field("motion")
            .and_then(|motions| motions.field(motion_name))
            .ok_or(EmoteSchemaError::MissingBaseObject)?;
        let layers = motion
            .field("layer")
            .and_then(PsbValue::as_list)
            .ok_or(EmoteSchemaError::MissingBaseObject)?;

        let duration = motion_duration_ticks(motion).unwrap_or(0.0);
        let effective_time = wrap_motion_time(time_ticks, duration);
        let mut sprites = Vec::new();
        for (index, layer) in layers.iter().enumerate() {
            travel_layer_at(
                layer,
                object_table,
                motion.field("parameter").and_then(PsbValue::as_list).or(root_parameter_table),
                psb,
                psb_data,
                variables,
                &self.textures,
                motion_name,
                effective_time,
                TravelContext { draw_index: index, ..TravelContext::default() },
                &mut sprites,
            )?;
        }
        Ok(finalize_scene(self.base_object.clone(), sprites))
    }

    pub fn build_static_scene(&self, psb: &PsbFile) -> Result<EmoteStaticScene, EmoteSchemaError> {
        let root = psb.root.as_object().ok_or(EmoteSchemaError::RootIsNotObject)?;
        let root_value = PsbValue::Object(root.to_vec());
        let object_table = root_value.field("object").ok_or(EmoteSchemaError::MissingObjectTable)?;
        let base = object_table
            .field(&self.base_object)
            .ok_or(EmoteSchemaError::MissingBaseObject)?;
        let motion_table = base.field("motion").ok_or(EmoteSchemaError::MissingBaseObject)?;

        let mut sprites = Vec::new();
        if let Some(motions) = motion_table.as_object() {
            for (motion_name, motion) in motions {
                if let Some(layers) = motion.field("layer").and_then(PsbValue::as_list) {
                    let ctx = TravelContext::default();
                    for (index, layer) in layers.iter().enumerate() {
                        travel_layer(
                            layer,
                            object_table,
                            &self.textures,
                            motion_name,
                            TravelContext { draw_index: index, ..ctx.clone() },
                            &mut sprites,
                        )?;
                    }
                }
            }
        }

        Ok(finalize_scene(self.base_object.clone(), sprites))
    }
}

fn finalize_scene(base_object: String, mut sprites: Vec<EmoteStaticSprite>) -> EmoteStaticScene {
    sprites.sort_by(|a, b| a.z.total_cmp(&b.z).then(a.draw_frame_info.draw_index.cmp(&b.draw_frame_info.draw_index)));
    for (index, sprite) in sprites.iter_mut().enumerate() {
        sprite.draw_frame_info.draw_index = index;
    }
    let bounds = compute_bounds(&sprites);
    let draw_frame_info = sprites.iter().map(|sprite| sprite.draw_frame_info.clone()).collect();
    EmoteStaticScene {
        base_object,
        sprites,
        bounds,
        draw_frame_info,
    }
}

pub fn load_emote_static_scene(psb: &PsbFile) -> Result<(EmoteModelSchema, EmoteStaticScene), EmoteSchemaError> {
    let schema = EmoteModelSchema::from_psb(psb)?;
    let scene = schema.build_static_scene(psb)?;
    Ok((schema, scene))
}

fn find_base_object(root: &PsbValue, object_table: &PsbValue) -> Result<String, EmoteSchemaError> {
    if let Some(chara) = root
        .field("metadata")
        .and_then(|metadata| metadata.field("base"))
        .and_then(|base| base.field_str("chara"))
        .filter(|s| !s.is_empty())
    {
        return Ok(chara.to_owned());
    }

    if object_table.field("all_parts").is_some() {
        return Ok("all_parts".to_owned());
    }

    let first = object_table
        .as_object()
        .and_then(|fields| fields.first())
        .map(|(key, _)| key.clone())
        .ok_or(EmoteSchemaError::MissingBaseObject)?;
    Ok(first)
}

fn collect_textures(source: &PsbValue) -> Result<BTreeMap<String, EmoteTextureSource>, EmoteSchemaError> {
    let mut textures = BTreeMap::new();
    let Some(entries) = source.as_object() else {
        return Ok(textures);
    };

    for (name, value) in entries {
        let Some(texture) = value.field("texture") else {
            continue;
        };
        let resource_index = texture
            .field_u32("pixel")
            .or_else(|| texture.field_u32("data"))
            .or_else(|| texture.field_u32("resource"))
            .ok_or_else(|| EmoteSchemaError::InvalidTextureResource { source: name.clone() })?;
        let width = number_to_positive_u32(texture.field("width"))
            .ok_or_else(|| EmoteSchemaError::InvalidSourceTexture { source: name.clone() })?;
        let height = number_to_positive_u32(texture.field("height"))
            .ok_or_else(|| EmoteSchemaError::InvalidSourceTexture { source: name.clone() })?;
        let format = texture.field_str("type").map(str::to_owned);
        let compress = texture.field_str("compress").map(str::to_owned);
        let bit_count = texture.field_u32("bitCount");

        let mut icons = BTreeMap::new();
        if let Some(icon_entries) = value.field("icon").and_then(PsbValue::as_object) {
            for (icon_name, icon_value) in icon_entries {
                let left = icon_value
                    .field_f32("left")
                    .ok_or_else(|| EmoteSchemaError::InvalidIcon {
                        source: name.clone(),
                        icon: icon_name.clone(),
                    })?;
                let top = icon_value
                    .field_f32("top")
                    .ok_or_else(|| EmoteSchemaError::InvalidIcon {
                        source: name.clone(),
                        icon: icon_name.clone(),
                    })?;
                let width = icon_value
                    .field_f32("width")
                    .ok_or_else(|| EmoteSchemaError::InvalidIcon {
                        source: name.clone(),
                        icon: icon_name.clone(),
                    })?;
                let height = icon_value
                    .field_f32("height")
                    .ok_or_else(|| EmoteSchemaError::InvalidIcon {
                        source: name.clone(),
                        icon: icon_name.clone(),
                    })?;
                let resolution = icon_value.field_f32("resolution").unwrap_or(1.0);
                icons.insert(
                    icon_name.clone(),
                    EmoteTextureIcon {
                        texture_name: name.clone(),
                        name: icon_name.clone(),
                        left,
                        top,
                        width,
                        height,
                        origin_x: icon_value.field_f32("originX").unwrap_or(0.0),
                        origin_y: icon_value.field_f32("originY").unwrap_or(0.0),
                        resolution,
                        attr: icon_value.field_u32("attr"),
                    },
                );
            }
        }

        textures.insert(
            name.clone(),
            EmoteTextureSource {
                name: name.clone(),
                resource_index,
                width,
                height,
                format,
                compress,
                bit_count,
                icons,
            },
        );
    }

    Ok(textures)
}

fn number_to_positive_u32(value: Option<&PsbValue>) -> Option<u32> {
    let n = value?.as_i64()?;
    (n > 0 && n <= u32::MAX as i64).then_some(n as u32)
}


#[derive(Debug, Clone, Copy, PartialEq)]
struct EmoteTransform2D {
    m11: f32,
    m12: f32,
    m21: f32,
    m22: f32,
    tx: f32,
    ty: f32,
}

impl EmoteTransform2D {
    fn identity() -> Self {
        Self {
            m11: 1.0,
            m12: 0.0,
            m21: 0.0,
            m22: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    fn translation(x: f32, y: f32) -> Self {
        Self {
            tx: x,
            ty: y,
            ..Self::identity()
        }
    }

    fn scale_rotation(scale_x: f32, scale_y: f32, rotation_degrees: f32) -> Self {
        let sx = finite_or(scale_x, 1.0);
        let sy = finite_or(scale_y, 1.0);
        let angle = finite_or(rotation_degrees, 0.0).to_radians();
        let cos = angle.cos();
        let sin = angle.sin();
        Self {
            m11: cos * sx,
            m12: -sin * sy,
            m21: sin * sx,
            m22: cos * sy,
            tx: 0.0,
            ty: 0.0,
        }
    }

    fn then(self, rhs: Self) -> Self {
        Self {
            m11: self.m11 * rhs.m11 + self.m12 * rhs.m21,
            m12: self.m11 * rhs.m12 + self.m12 * rhs.m22,
            m21: self.m21 * rhs.m11 + self.m22 * rhs.m21,
            m22: self.m21 * rhs.m12 + self.m22 * rhs.m22,
            tx: self.m11 * rhs.tx + self.m12 * rhs.ty + self.tx,
            ty: self.m21 * rhs.tx + self.m22 * rhs.ty + self.ty,
        }
    }

    fn apply(self, point: [f32; 2]) -> [f32; 2] {
        [
            self.m11 * point[0] + self.m12 * point[1] + self.tx,
            self.m21 * point[0] + self.m22 * point[1] + self.ty,
        ]
    }

    fn as_array(self) -> [f32; 6] {
        [self.m11, self.m12, self.m21, self.m22, self.tx, self.ty]
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn transform_from_array(values: [f32; 6]) -> EmoteTransform2D {
    EmoteTransform2D {
        m11: values[0],
        m12: values[1],
        m21: values[2],
        m22: values[3],
        tx: values[4],
        ty: values[5],
    }
}

#[derive(Debug, Clone)]
struct TravelContext {
    base_location: Option<[f32; 3]>,
    base_visible: bool,
    opacity_multiplier: f32,
    transform: EmoteTransform2D,
    path: String,
    draw_index: usize,
    mesh_transform: i64,
    mesh_combine: bool,
    mesh_sync_child: i64,
    join_target: Option<i64>,
    inherit_mask: Option<i64>,
    transform_order: Vec<i64>,
    coordinate: Option<i64>,
    ground_correction: Option<i64>,
    stencil_type: i64,
    stencil_composite_mask_layer_list: Vec<String>,
    control_parameter: Option<String>,
    control_value: Option<f32>,
    local_time_ticks: Option<f32>,
    frame_index: Option<usize>,
    next_frame_index: Option<usize>,
    interpolation_t: f32,
    mesh_division_x: u32,
    mesh_division_y: u32,
    mesh_patch: Option<EmoteMeshPatch>,
}

impl Default for TravelContext {
    fn default() -> Self {
        Self {
            base_location: None,
            base_visible: true,
            opacity_multiplier: 1.0,
            transform: EmoteTransform2D::identity(),
            path: String::new(),
            draw_index: 0,
            mesh_transform: 0,
            mesh_combine: false,
            mesh_sync_child: 0,
            join_target: None,
            inherit_mask: None,
            transform_order: Vec::new(),
            coordinate: None,
            ground_correction: None,
            stencil_type: 0,
            stencil_composite_mask_layer_list: Vec::new(),
            control_parameter: None,
            control_value: None,
            local_time_ticks: None,
            frame_index: None,
            next_frame_index: None,
            interpolation_t: 0.0,
            mesh_division_x: 1,
            mesh_division_y: 1,
            mesh_patch: None,
        }
    }
}

fn enter_layer_context(mut ctx: TravelContext, layer: &PsbValue, sibling_index: usize) -> TravelContext {
    let label = layer
        .field_str("label")
        .map(str::to_owned)
        .unwrap_or_else(|| sibling_index.to_string());
    ctx.path = if ctx.path.is_empty() {
        label
    } else {
        format!("{}/{}", ctx.path, label)
    };
    if let Some(v) = layer.field_i64("meshTransform") {
        ctx.mesh_transform = v;
    }
    if let Some(v) = layer.field_i64("meshCombine") {
        ctx.mesh_combine = v != 0;
    }
    if let Some(v) = layer.field_i64("meshSyncChildMask") {
        ctx.mesh_sync_child = v;
    }
    if let Some(v) = layer.field_i64("joinTarget") {
        ctx.join_target = Some(v);
    }
    if let Some(v) = layer.field_i64("inheritMask") {
        ctx.inherit_mask = Some(v);
    }
    if let Some(v) = layer.field_i64("coordinate") {
        ctx.coordinate = Some(v);
    }
    if let Some(v) = layer.field_i64("groundCorrection") {
        ctx.ground_correction = Some(v);
    }
    if let Some(v) = layer.field_i64("stencilType") {
        ctx.stencil_type = v;
    }
    if let Some(values) = layer.field("transformOrder").and_then(PsbValue::as_list) {
        ctx.transform_order = values.iter().filter_map(PsbValue::as_i64).collect();
    }
    if let Some(values) = layer.field("stencilCompositeMaskLayerList").and_then(PsbValue::as_list) {
        ctx.stencil_composite_mask_layer_list = values
            .iter()
            .filter_map(PsbValue::as_str)
            .map(str::to_owned)
            .collect();
    }
    if let Some((dx, dy)) = parse_mesh_division(layer.field("meshDivision")) {
        ctx.mesh_division_x = dx;
        ctx.mesh_division_y = dy;
    }
    ctx
}

fn draw_frame_info(label: Option<String>, ctx: TravelContext) -> EmoteDrawFrameInfo {
    let mesh_sync_child_mask = ctx.mesh_sync_child;
    let inherit_mask = ctx.inherit_mask.unwrap_or(0);
    let pass = if ctx.stencil_type != 0 && !ctx.stencil_composite_mask_layer_list.is_empty() {
        EmoteDrawPass::StencilCompositeMask
    } else if ctx.stencil_type != 0 {
        EmoteDrawPass::MaskGeneration
    } else {
        EmoteDrawPass::Normal
    };
    EmoteDrawFrameInfo {
        layer_label: label,
        draw_index: ctx.draw_index,
        path: ctx.path,
        mesh_transform: ctx.mesh_transform,
        mesh_combine: ctx.mesh_combine,
        mesh_sync_child_mask,
        mesh_sync_child_coord: (mesh_sync_child_mask & 1) != 0,
        mesh_sync_child_angle: (mesh_sync_child_mask & 2) != 0,
        mesh_sync_child_zoom: (mesh_sync_child_mask & 4) != 0,
        mesh_sync_child_shape: (mesh_sync_child_mask & 8) != 0,
        join_target: ctx.join_target,
        inherit_mask: ctx.inherit_mask,
        inherit_parent: (inherit_mask & (1 << 22)) != 0,
        inherit_opacity: (inherit_mask & (1 << 10)) != 0,
        inherit_shape: (inherit_mask & (1 << 25)) != 0,
        inherit_angle: (inherit_mask & (1 << 4)) != 0,
        transform_order: ctx.transform_order,
        coordinate: ctx.coordinate,
        ground_correction: ctx.ground_correction,
        stencil_type: ctx.stencil_type,
        stencil_composite_mask_layer_list: ctx.stencil_composite_mask_layer_list,
        control_parameter: ctx.control_parameter,
        control_value: ctx.control_value,
        local_time_ticks: ctx.local_time_ticks,
        frame_index: ctx.frame_index,
        next_frame_index: ctx.next_frame_index,
        interpolation_t: ctx.interpolation_t,
        pass,
    }
}

fn travel_layer(
    value: &PsbValue,
    object_table: &PsbValue,
    textures: &BTreeMap<String, EmoteTextureSource>,
    motion_name: &str,
    mut ctx: TravelContext,
    out: &mut Vec<EmoteStaticSprite>,
) -> Result<(), EmoteSchemaError> {
    let Some(fields) = value.as_object() else {
        return Ok(());
    };
    let layer = PsbValue::Object(fields.to_vec());

    ctx = enter_layer_context(ctx, &layer, out.len());

    if let Some(frame_list) = layer.field("frameList").and_then(PsbValue::as_list) {
        let label = layer.field_str("label").map(str::to_owned);
        let mut local_ox = 0.0;
        let mut local_oy = 0.0;

        if let Some(content) = first_frame_content_with(frame_list, "coord") {
            if let Some(coord) = content.field("coord").and_then(PsbValue::as_list) {
                if coord.len() >= 3 {
                    let delta = [
                        coord[0].as_f32().unwrap_or(0.0),
                        coord[1].as_f32().unwrap_or(0.0),
                        coord[2].as_f32().unwrap_or(0.0),
                    ];
                    local_ox = content.field_f32("ox").unwrap_or(0.0);
                    local_oy = content.field_f32("oy").unwrap_or(0.0);

                    ctx.base_location = Some(match ctx.base_location {
                        Some(base) => [base[0] + delta[0], base[1] + delta[1], base[2] + delta[2]],
                        None => delta,
                    });
                    ctx.transform = ctx.transform.then(EmoteTransform2D::translation(delta[0], delta[1]));
                }
            }
        }

        for frame in frame_list {
            let Some(frame_obj) = frame.as_object() else {
                continue;
            };
            let frame_value = PsbValue::Object(frame_obj.to_vec());
            let Some(content_fields) = frame_value.field("content").and_then(PsbValue::as_object) else {
                continue;
            };
            let content = PsbValue::Object(content_fields.to_vec());
            let Some(src) = content.field_str("src").filter(|s| !s.is_empty()) else {
                continue;
            };

            let opa_raw = content.field_f32("opa").unwrap_or(10.0);
            let time = frame_value.field_i64("time").unwrap_or(0);
            let visible = ctx.base_visible && time <= 0 && opa_raw > 0.0;
            let suggest_visible = ctx.base_visible && time <= 0 && opa_raw > 0.0;

            if let Some(rest) = src.strip_prefix("motion/") {
                if ctx.base_location.is_some() {
                    let mut parts = rest.split('/').filter(|s| !s.is_empty());
                    if let (Some(object_name), Some(child_motion_name)) = (parts.next(), parts.next()) {
                        recurse_motion(
                            object_table,
                            object_name,
                            child_motion_name,
                            textures,
                            ctx_with_visible(ctx.clone(), suggest_visible),
                            out,
                        )?;
                    }
                }
                continue;
            }

            let icon_name = content.field_str("icon");
            if let Some(icon_name) = icon_name {
                if textures.contains_key(src) {
                    if let Some(base) = ctx.base_location {
                        if let Some(sprite) = build_sprite(
                            textures,
                            src,
                            icon_name,
                            label.clone(),
                            motion_name,
                            base,
                            local_ox,
                            local_oy,
                            1.0,
                            1.0,
                            0.0,
                            visible,
                            opa_raw,
                            draw_ctx_with_index(ctx.clone(), out.len()),
                        ) {
                            out.push(sprite);
                        }
                    }
                } else if object_table.field(src).is_some() {
                    recurse_motion(
                        object_table,
                        src,
                        icon_name,
                        textures,
                        ctx_with_visible(ctx.clone(), suggest_visible),
                        out,
                    )?;
                }
            }
        }
    }

    if let Some(children) = layer.field("children").and_then(PsbValue::as_list) {
        for (index, child) in children.iter().enumerate() {
            let mut child_ctx = ctx.clone();
            child_ctx.draw_index = out.len() + index;
            travel_layer(child, object_table, textures, motion_name, child_ctx, out)?;
        }
    }
    if let Some(children) = layer.field("layer").and_then(PsbValue::as_list) {
        for (index, child) in children.iter().enumerate() {
            let mut child_ctx = ctx.clone();
            child_ctx.draw_index = out.len() + index;
            travel_layer(child, object_table, textures, motion_name, child_ctx, out)?;
        }
    }

    Ok(())
}

fn draw_ctx_with_index(mut ctx: TravelContext, draw_index: usize) -> TravelContext {
    ctx.draw_index = draw_index;
    ctx
}

fn first_frame_content_with(frame_list: &[PsbValue], key: &str) -> Option<PsbValue> {
    for frame in frame_list {
        let Some(content) = frame.field("content") else {
            continue;
        };
        if content.field(key).is_some() {
            return Some(content.clone());
        }
    }
    None
}

fn recurse_motion(
    object_table: &PsbValue,
    object_name: &str,
    motion_name: &str,
    textures: &BTreeMap<String, EmoteTextureSource>,
    ctx: TravelContext,
    out: &mut Vec<EmoteStaticSprite>,
) -> Result<(), EmoteSchemaError> {
    let Some(layers) = object_table
        .field(object_name)
        .and_then(|object| object.field("motion"))
        .and_then(|motion| motion.field(motion_name))
        .and_then(|motion| motion.field("layer"))
        .and_then(PsbValue::as_list)
    else {
        return Ok(());
    };

    for (index, layer) in layers.iter().enumerate() {
        let mut next_ctx = ctx.clone();
        next_ctx.draw_index = out.len() + index;
        travel_layer(layer, object_table, textures, motion_name, next_ctx, out)?;
    }
    Ok(())
}

fn ctx_with_visible(mut ctx: TravelContext, visible: bool) -> TravelContext {
    ctx.base_visible = visible;
    ctx
}

fn ctx_with_opacity(mut ctx: TravelContext, opa_raw: f32) -> TravelContext {
    ctx.opacity_multiplier = (ctx.opacity_multiplier * (opa_raw / 10.0)).clamp(0.0, 1.0);
    ctx
}

fn apply_layer_transform(mut ctx: TravelContext, coord: Option<[f32; 3]>, scale_x: f32, scale_y: f32, rotation_degrees: f32) -> TravelContext {
    let delta = coord.unwrap_or([0.0, 0.0, 0.0]);
    ctx.base_location = Some(match ctx.base_location {
        Some(base) => [base[0] + delta[0], base[1] + delta[1], base[2] + delta[2]],
        None => delta,
    });
    let local = EmoteTransform2D::translation(delta[0], delta[1])
        .then(EmoteTransform2D::scale_rotation(scale_x, scale_y, rotation_degrees));
    ctx.transform = ctx.transform.then(local);
    ctx
}


fn build_sprite(
    textures: &BTreeMap<String, EmoteTextureSource>,
    texture_name: &str,
    icon_name: &str,
    label: Option<String>,
    motion_name: &str,
    base: [f32; 3],
    ox: f32,
    oy: f32,
    scale_x: f32,
    scale_y: f32,
    rotation_degrees: f32,
    visible: bool,
    opa_raw: f32,
    ctx: TravelContext,
) -> Option<EmoteStaticSprite> {
    let texture = textures.get(texture_name)?;
    let icon = texture.icons.get(icon_name)?;
    let width = icon.resolved_width();
    let height = icon.resolved_height();

    // FreeMote's win-path static painter applies this subtraction only for
    // MeshTransform.None. Keep the same static behavior here; BezierPatch and
    // child-mesh-sync are intentionally left for StepFrameMeshChain parity work.
    let subtract_icon_origin = ctx.mesh_transform == 0;
    let center_x = if subtract_icon_origin {
        ox - icon.origin_x
    } else {
        ox
    };
    let center_y = if subtract_icon_origin {
        oy - icon.origin_y
    } else {
        oy
    };

    Some(EmoteStaticSprite {
        label: label.clone(),
        motion_name: motion_name.to_owned(),
        texture_name: texture_name.to_owned(),
        texture_resource_index: texture.resource_index,
        texture_width: texture.width,
        texture_height: texture.height,
        texture_format: texture.format.clone(),
        icon_name: icon_name.to_owned(),
        z: base[2],
        opacity: (ctx.opacity_multiplier * (opa_raw / 10.0)).clamp(0.0, 1.0),
        visible,
        center_x,
        center_y,
        width,
        height,
        scale_x,
        scale_y,
        rotation_degrees,
        world_transform: ctx.transform.as_array(),
        uv_left: icon.left / texture.width as f32,
        uv_top: icon.top / texture.height as f32,
        uv_right: (icon.left + width) / texture.width as f32,
        uv_bottom: (icon.top + height) / texture.height as f32,
        mesh: ctx.mesh_patch,
        draw_frame_info: draw_frame_info(label, ctx),
    })
}

fn compute_bounds(sprites: &[EmoteStaticSprite]) -> Option<EmoteSceneBounds> {
    let mut iter = sprites.iter().filter(|sprite| sprite.visible && sprite.opacity > 0.0);
    let first = iter.next()?;
    let (left, top, right, bottom) = first.bounds_rect();
    let mut bounds = EmoteSceneBounds {
        min_x: left,
        min_y: top,
        max_x: right,
        max_y: bottom,
    };
    for sprite in iter {
        let (left, top, right, bottom) = sprite.bounds_rect();
        bounds.include_rect(left, top, right, bottom);
    }
    Some(bounds)
}


#[derive(Debug, Clone)]
struct DynamicFrameState {
    coord: Option<[f32; 3]>,
    ox: f32,
    oy: f32,
    scale_x: f32,
    scale_y: f32,
    rotation_degrees: f32,
    src: Option<String>,
    icon: Option<String>,
    opa: f32,
    time_offset_ticks: f32,
    mesh_patch: Option<EmoteMeshPatch>,
    frame_index: Option<usize>,
    next_frame_index: Option<usize>,
    interpolation_t: f32,
}

impl Default for DynamicFrameState {
    fn default() -> Self {
        Self {
            coord: None,
            ox: 0.0,
            oy: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_degrees: 0.0,
            src: None,
            icon: None,
            opa: 10.0,
            time_offset_ticks: 0.0,
            mesh_patch: None,
            frame_index: None,
            next_frame_index: None,
            interpolation_t: 0.0,
        }
    }
}

fn wrap_motion_time(time_ticks: f32, duration_ticks: f32) -> f32 {
    if !time_ticks.is_finite() || time_ticks <= 0.0 {
        return 0.0;
    }
    if duration_ticks.is_finite() && duration_ticks > 0.0 {
        time_ticks.rem_euclid(duration_ticks.max(1.0))
    } else {
        time_ticks
    }
}

fn motion_duration_ticks(motion: &PsbValue) -> Option<f32> {
    let mut max_time = motion
        .field_f32("lastTime")
        .or_else(|| motion.field_f32("loopTime"))
        .unwrap_or(0.0)
        .max(0.0);
    if let Some(layers) = motion.field("layer").and_then(PsbValue::as_list) {
        for layer in layers {
            max_time = max_time.max(max_layer_frame_time(layer));
        }
    }
    (max_time > 0.0).then_some(max_time)
}

fn max_layer_frame_time(layer: &PsbValue) -> f32 {
    let mut max_time: f32 = 0.0;
    if let Some(frame_list) = layer.field("frameList").and_then(PsbValue::as_list) {
        for frame in frame_list {
            max_time = max_time.max(frame.field_f32("time").unwrap_or(0.0));
        }
    }
    if let Some(children) = layer.field("children").and_then(PsbValue::as_list) {
        for child in children {
            max_time = max_time.max(max_layer_frame_time(child));
        }
    }
    if let Some(children) = layer.field("layer").and_then(PsbValue::as_list) {
        for child in children {
            max_time = max_time.max(max_layer_frame_time(child));
        }
    }
    max_time
}

fn evaluate_frame_list(frame_list: &[PsbValue], time_ticks: f32) -> DynamicFrameState {
    let mut state = DynamicFrameState::default();
    let mut previous_content: Option<&PsbValue> = None;
    let mut previous_time = 0.0f32;
    let mut next_content: Option<&PsbValue> = None;
    let mut next_time = 0.0f32;

    for (index, frame) in frame_list.iter().enumerate() {
        let frame_time = frame.field_f32("time").unwrap_or(0.0);
        if frame_time > time_ticks {
            if next_content.is_none() {
                next_content = frame.field("content");
                next_time = frame_time;
                state.next_frame_index = Some(index);
            }
            break;
        }

        if frame.field_i64("type").unwrap_or(3) == 0 && frame.field("content").is_none() {
            state = DynamicFrameState::default();
            state.opa = 0.0;
            previous_content = None;
            previous_time = frame_time;
            state.frame_index = Some(index);
            continue;
        }

        let Some(content) = frame.field("content") else {
            continue;
        };
        merge_frame_content(&mut state, content);
        previous_content = Some(content);
        previous_time = frame_time;
        state.frame_index = Some(index);
    }

    if let (Some(prev), Some(next)) = (previous_content, next_content) {
        let span = next_time - previous_time;
        if span.is_finite() && span > f32::EPSILON {
            let t = ((time_ticks - previous_time) / span).clamp(0.0, 1.0);
            let easing = next.field_f32("easing").unwrap_or(0.0);
            let t = frame_easing(t, easing);
            state.interpolation_t = t;
            interpolate_frame_content(&mut state, prev, next, t);
        }
    }

    state
}

fn frame_easing(t: f32, easing: f32) -> f32 {
    if !easing.is_finite() || easing == 0.0 {
        t
    } else if easing > 0.0 {
        t * t * (3.0 - 2.0 * t)
    } else {
        1.0 - (1.0 - t) * (1.0 - t)
    }
}

fn interpolate_frame_content(state: &mut DynamicFrameState, prev: &PsbValue, next: &PsbValue, t: f32) {
    if let (Some(a), Some(b)) = (content_coord(prev), content_coord(next)) {
        state.coord = Some([
            lerp(a[0], b[0], t),
            lerp(a[1], b[1], t),
            lerp(a[2], b[2], t),
        ]);
    }
    if let (Some(a), Some(b)) = (prev.field_f32("ox"), next.field_f32("ox")) {
        state.ox = lerp(a, b, t);
    }
    if let (Some(a), Some(b)) = (prev.field_f32("oy"), next.field_f32("oy")) {
        state.oy = lerp(a, b, t);
    }
    if let (Some(a), Some(b)) = (prev.field_f32("opa"), next.field_f32("opa")) {
        state.opa = lerp(a, b, t);
    }
    if let (Some(a), Some(b)) = (content_scale_x(prev), content_scale_x(next)) {
        state.scale_x = lerp(a, b, t);
    }
    if let (Some(a), Some(b)) = (content_scale_y(prev), content_scale_y(next)) {
        state.scale_y = lerp(a, b, t);
    }
    if let (Some(a), Some(b)) = (content_rotation(prev), content_rotation(next)) {
        state.rotation_degrees = lerp(a, b, t);
    }
    if let (Some(a), Some(b)) = (parse_content_mesh_patch(prev, 1, 1), parse_content_mesh_patch(next, 1, 1)) {
        state.mesh_patch = Some(EmoteMeshPatch::interpolate(&a, &b, t));
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
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

fn content_scale_x(content: &PsbValue) -> Option<f32> {
    content.field_f32("scale_x")
        .or_else(|| content.field_f32("scaleX"))
        .or_else(|| content.field_f32("scale"))
        .or_else(|| content.field_f32("zoom"))
}

fn content_scale_y(content: &PsbValue) -> Option<f32> {
    content.field_f32("scale_y")
        .or_else(|| content.field_f32("scaleY"))
        .or_else(|| content.field_f32("scale"))
        .or_else(|| content.field_f32("zoom"))
}

fn content_rotation(content: &PsbValue) -> Option<f32> {
    content.field_f32("angle")
        .or_else(|| content.field_f32("rot"))
        .or_else(|| content.field_f32("rotation"))
}

fn merge_frame_content(state: &mut DynamicFrameState, content: &PsbValue) {
    if let Some(coord) = content.field("coord").and_then(PsbValue::as_list) {
        if coord.len() >= 3 {
            state.coord = Some([
                coord[0].as_f32().unwrap_or(0.0),
                coord[1].as_f32().unwrap_or(0.0),
                coord[2].as_f32().unwrap_or(0.0),
            ]);
        }
    }
    if let Some(ox) = content.field_f32("ox") {
        state.ox = ox;
    }
    if let Some(oy) = content.field_f32("oy") {
        state.oy = oy;
    }
    if let Some(scale_x) = content_scale_x(content) {
        state.scale_x = scale_x;
    }
    if let Some(scale_y) = content_scale_y(content) {
        state.scale_y = scale_y;
    }
    if let Some(rotation) = content_rotation(content) {
        state.rotation_degrees = rotation;
    }
    if let Some(src) = content.field_str("src") {
        state.src = Some(src.to_owned());
    }
    if let Some(icon) = content.field_str("icon") {
        state.icon = Some(icon.to_owned());
    }
    if let Some(opa) = content.field_f32("opa") {
        state.opa = opa;
    }
    if let Some(time_offset) = content
        .field("motion")
        .and_then(|motion| motion.field_f32("timeOffset"))
        .or_else(|| content.field_f32("timeOffset"))
    {
        state.time_offset_ticks = time_offset;
    }
    if let Some(mesh) = parse_content_mesh_patch(content, 1, 1) {
        state.mesh_patch = Some(mesh);
    }
}


#[derive(Debug, Clone)]
struct LayerParameterEval {
    id: Option<String>,
    value: Option<f32>,
    local_time_ticks: f32,
}

fn layer_parameter_eval(
    layer: &PsbValue,
    parameter_table: Option<&[PsbValue]>,
    variables: &BTreeMap<String, f32>,
    frame_list: &[PsbValue],
    fallback_time_ticks: f32,
) -> Option<LayerParameterEval> {
    if let Some(parameterize) = layer.field("parameterize") {
        let Some(parameter) = resolve_parameterize(parameterize, parameter_table) else {
            return Some(LayerParameterEval { id: None, value: None, local_time_ticks: 0.0 });
        };
        let Some(id) = parameter
            .field_str("id")
            .or_else(|| parameter.field_str("key"))
            .or_else(|| parameter.field_str("name"))
            .filter(|s| !s.is_empty())
        else {
            return Some(LayerParameterEval { id: None, value: None, local_time_ticks: 0.0 });
        };

        let Some(value) = variables.get(id).copied() else {
            #[cfg(debug_assertions)]
            {
                let seen = MISSING_PARAMETER_VARIABLES.get_or_init(|| Mutex::new(BTreeSet::new()));
                if let Ok(mut seen) = seen.lock() {
                    if seen.insert(id.to_owned()) {
                        eprintln!("parameterized layer references missing variable '{id}'");
                    }
                }
            }
            return Some(LayerParameterEval { id: Some(id.to_owned()), value: None, local_time_ticks: 0.0 });
        };

        let begin = parameter
            .field_f32("rangeBegin")
            .or_else(|| parameter.field_f32("min"))
            .unwrap_or(0.0);
        let end = parameter
            .field_f32("rangeEnd")
            .or_else(|| parameter.field_f32("max"))
            .unwrap_or(1.0);
        let max_time = max_frame_list_time(frame_list);
        if !max_time.is_finite() || max_time <= 0.0 || !begin.is_finite() || !end.is_finite() || (end - begin).abs() <= f32::EPSILON {
            return Some(LayerParameterEval { id: Some(id.to_owned()), value: Some(value), local_time_ticks: 0.0 });
        }
        let t = ((value - begin) / (end - begin)).clamp(0.0, 1.0);
        return Some(LayerParameterEval {
            id: Some(id.to_owned()),
            value: Some(value),
            local_time_ticks: t * max_time,
        });
    }

    let max_time = max_frame_list_time(frame_list);
    if !max_time.is_finite() || max_time <= 0.0 {
        return Some(LayerParameterEval { id: None, value: None, local_time_ticks: 0.0 });
    }

    let local_duration = layer
        .field_f32("loopTime")
        .or_else(|| layer.field_f32("lastTime"))
        .unwrap_or(max_time)
        .max(max_time);
    if local_duration.is_finite() && local_duration > 0.0 {
        Some(LayerParameterEval {
            id: None,
            value: None,
            local_time_ticks: fallback_time_ticks.rem_euclid(local_duration.max(1.0)),
        })
    } else {
        Some(LayerParameterEval { id: None, value: None, local_time_ticks: fallback_time_ticks })
    }
}

fn resolve_parameterize<'a>(parameterize: &'a PsbValue, parameter_table: Option<&'a [PsbValue]>) -> Option<&'a PsbValue> {
    match parameterize {
        PsbValue::Object(_) => Some(parameterize),
        PsbValue::Int(index) if *index >= 0 => parameter_table?.get(*index as usize),
        _ => None,
    }
}

fn max_frame_list_time(frame_list: &[PsbValue]) -> f32 {
    frame_list
        .iter()
        .map(|frame| frame.field_f32("time").unwrap_or(0.0))
        .fold(0.0, f32::max)
}

fn combine_patch(base: Option<EmoteMeshPatch>, patch: EmoteMeshPatch) -> Option<EmoteMeshPatch> {
    Some(match base {
        Some(prev) => prev.combined_with(&patch),
        None => patch,
    })
}

#[derive(Debug, Clone)]
struct MeshCombinatorEval {
    self_patch: Option<EmoteMeshPatch>,
    child_patch: Option<EmoteMeshPatch>,
}

fn evaluate_mesh_combinator_split(
    layer: &PsbValue,
    parameter_table: Option<&[PsbValue]>,
    psb: &PsbFile,
    psb_data: Option<&[u8]>,
    variables: &BTreeMap<String, f32>,
    division_x: u32,
    division_y: u32,
) -> Option<MeshCombinatorEval> {
    let data = psb_data?;
    let combinators = layer
        .field("meshCombinator")?
        .field("combinatorList")?
        .as_list()?;
    if combinators.is_empty() {
        return None;
    }

    let first_key = combinators
        .first()
        .and_then(|c| c.field("variable"))
        .and_then(|v| v.field_str("key"));
    let parent_key = layer
        .field("parameterize")
        .and_then(|p| resolve_parameterize(p, parameter_table))
        .and_then(|p| p.field_str("id").or_else(|| p.field_str("key")).or_else(|| p.field_str("name")));
    let restore_first_to_parent = match (parent_key, first_key) {
        (None, _) => true,
        (Some("param"), _) => true,
        (Some(parent), Some(first)) => parent == first,
        (Some(_), None) => false,
    };

    let mut self_patch = None;
    let mut child_patch = None;

    let mut start_index = 0usize;
    if restore_first_to_parent {
        if let Some(first) = combinators.first() {
            if let Some(patch) = evaluate_one_combinator(first, psb, data, variables, division_x, division_y, false) {
                self_patch = Some(patch);
                child_patch = combine_patch(child_patch, patch);
                start_index = 1;
            }
        }
    }

    for combinator in combinators.iter().skip(start_index) {
        if let Some(patch) = evaluate_one_combinator(combinator, psb, data, variables, division_x, division_y, true) {
            child_patch = combine_patch(child_patch, patch);
        }
    }

    if self_patch.is_none() && child_patch.is_none() {
        None
    } else {
        Some(MeshCombinatorEval { self_patch, child_patch })
    }
}

fn travel_layer_at(
    value: &PsbValue,
    object_table: &PsbValue,
    parameter_table: Option<&[PsbValue]>,
    psb: &PsbFile,
    psb_data: Option<&[u8]>,
    variables: &BTreeMap<String, f32>,
    textures: &BTreeMap<String, EmoteTextureSource>,
    motion_name: &str,
    time_ticks: f32,
    mut ctx: TravelContext,
    out: &mut Vec<EmoteStaticSprite>,
) -> Result<(), EmoteSchemaError> {
    let Some(fields) = value.as_object() else {
        return Ok(());
    };
    let layer = PsbValue::Object(fields.to_vec());

    ctx = enter_layer_context(ctx, &layer, out.len());

    let mesh_combinator = evaluate_mesh_combinator_split(
        &layer,
        parameter_table,
        psb,
        psb_data,
        variables,
        ctx.mesh_division_x,
        ctx.mesh_division_y,
    );
    let mut draw_ctx = ctx.clone();
    let mut child_ctx = ctx.clone();
    if let Some(mesh_combinator) = mesh_combinator {
        if let Some(patch) = mesh_combinator.self_patch {
            draw_ctx.mesh_patch = combine_patch(draw_ctx.mesh_patch.take(), patch);
            child_ctx.mesh_patch = combine_patch(child_ctx.mesh_patch.take(), patch);
        }
        if let Some(patch) = mesh_combinator.child_patch {
            child_ctx.mesh_patch = combine_patch(child_ctx.mesh_patch.take(), patch);
        }
    }

    let label = layer.field_str("label").map(str::to_owned);
    let mut active_external_motion = false;
    if let Some(frame_list) = layer.field("frameList").and_then(PsbValue::as_list) {
        let param_eval = layer_parameter_eval(&layer, parameter_table, variables, frame_list, time_ticks)
            .unwrap_or(LayerParameterEval { id: None, value: None, local_time_ticks: 0.0 });
        draw_ctx.control_parameter = param_eval.id.clone();
        draw_ctx.control_value = param_eval.value;
        draw_ctx.local_time_ticks = Some(param_eval.local_time_ticks);
        child_ctx.control_parameter = param_eval.id.clone();
        child_ctx.control_value = param_eval.value;
        child_ctx.local_time_ticks = Some(param_eval.local_time_ticks);
        let local_time = param_eval.local_time_ticks;
        let mut state = evaluate_frame_list(frame_list, local_time);
        draw_ctx.frame_index = state.frame_index;
        draw_ctx.next_frame_index = state.next_frame_index;
        draw_ctx.interpolation_t = state.interpolation_t;
        child_ctx.frame_index = state.frame_index;
        child_ctx.next_frame_index = state.next_frame_index;
        child_ctx.interpolation_t = state.interpolation_t;
        if let Some(mesh) = state.mesh_patch.take() {
            let mesh = EmoteMeshPatch {
                division_x: ctx.mesh_division_x.max(mesh.division_x),
                division_y: ctx.mesh_division_y.max(mesh.division_y),
                control_points: mesh.control_points,
            };
            draw_ctx.mesh_patch = combine_patch(draw_ctx.mesh_patch.take(), mesh);
            child_ctx.mesh_patch = combine_patch(child_ctx.mesh_patch.take(), mesh);
        }
        draw_ctx = apply_layer_transform(draw_ctx, state.coord, state.scale_x, state.scale_y, state.rotation_degrees);
        child_ctx = apply_layer_transform(child_ctx, state.coord, state.scale_x, state.scale_y, state.rotation_degrees);
        draw_ctx = ctx_with_opacity(draw_ctx, state.opa);
        child_ctx = ctx_with_opacity(child_ctx, state.opa);

        if let Some(src) = state.src.as_deref().filter(|src| !src.is_empty()) {
            let visible = draw_ctx.base_visible && draw_ctx.opacity_multiplier > 0.0;
            let visible_child_ctx = ctx_with_visible(child_ctx.clone(), visible);
            if let Some(rest) = src.strip_prefix("motion/") {
                if visible_child_ctx.base_location.is_some() {
                    let mut parts = rest.split('/').filter(|s| !s.is_empty());
                    if let (Some(object_name), Some(child_motion_name)) = (parts.next(), parts.next()) {
                        recurse_motion_at(
                            object_table,
                            object_name,
                            child_motion_name,
                            parameter_table,
                            psb,
                            psb_data,
                            variables,
                            textures,
                            time_ticks + state.time_offset_ticks,
                            visible_child_ctx,
                            out,
                        )?;
                        active_external_motion = true;
                    }
                }
            } else if let Some(icon_name) = state.icon.as_deref() {
                if textures.contains_key(src) {
                    if let Some(base) = draw_ctx.base_location {
                        if let Some(sprite) = build_sprite(
                            textures,
                            src,
                            icon_name,
                            label.clone(),
                            motion_name,
                            base,
                            state.ox,
                            state.oy,
                            1.0,
                            1.0,
                            0.0,
                            visible,
                            10.0,
                            draw_ctx_with_index(draw_ctx, out.len()),
                        ) {
                            out.push(sprite);
                        }
                    }
                } else if object_table.field(src).is_some() {
                    recurse_motion_at(
                        object_table,
                        src,
                        icon_name,
                        parameter_table,
                        psb,
                        psb_data,
                        variables,
                        textures,
                        time_ticks + state.time_offset_ticks,
                        visible_child_ctx,
                        out,
                    )?;
                    active_external_motion = true;
                }
            }
        }
    }

    if active_external_motion {
        return Ok(());
    }

    if let Some(children) = layer.field("children").and_then(PsbValue::as_list) {
        for (index, child) in children.iter().enumerate() {
            let mut next_ctx = child_ctx.clone();
            next_ctx.draw_index = out.len() + index;
            travel_layer_at(child, object_table, parameter_table, psb, psb_data, variables, textures, motion_name, time_ticks, next_ctx, out)?;
        }
    }
    if let Some(children) = layer.field("layer").and_then(PsbValue::as_list) {
        for (index, child) in children.iter().enumerate() {
            let mut next_ctx = child_ctx.clone();
            next_ctx.draw_index = out.len() + index;
            travel_layer_at(child, object_table, parameter_table, psb, psb_data, variables, textures, motion_name, time_ticks, next_ctx, out)?;
        }
    }

    Ok(())
}

fn recurse_motion_at(
    object_table: &PsbValue,
    object_name: &str,
    motion_name: &str,
    parameter_table: Option<&[PsbValue]>,
    psb: &PsbFile,
    psb_data: Option<&[u8]>,
    variables: &BTreeMap<String, f32>,
    textures: &BTreeMap<String, EmoteTextureSource>,
    time_ticks: f32,
    ctx: TravelContext,
    out: &mut Vec<EmoteStaticSprite>,
) -> Result<(), EmoteSchemaError> {
    let Some(motion) = object_table
        .field(object_name)
        .and_then(|object| object.field("motion"))
        .and_then(|motion| motion.field(motion_name))
    else {
        return Ok(());
    };
    let Some(layers) = motion.field("layer").and_then(PsbValue::as_list) else {
        return Ok(());
    };
    let motion_parameter_table = motion.field("parameter").and_then(PsbValue::as_list).or(parameter_table);
    let duration = motion_duration_ticks(motion).unwrap_or(0.0);
    let effective_time = wrap_motion_time(time_ticks, duration);
    for (index, layer) in layers.iter().enumerate() {
        let mut next_ctx = ctx.clone();
        next_ctx.draw_index = out.len() + index;
        travel_layer_at(layer, object_table, motion_parameter_table, psb, psb_data, variables, textures, motion_name, effective_time, next_ctx, out)?;
    }
    Ok(())
}

fn parse_mesh_division(value: Option<&PsbValue>) -> Option<(u32, u32)> {
    let value = value?;
    if let Some(n) = value.as_i64() {
        let n = n.clamp(1, 256) as u32;
        return Some((n, n));
    }
    if let Some(list) = value.as_list() {
        if list.len() >= 2 {
            let x = list[0].as_i64().unwrap_or(1).clamp(1, 256) as u32;
            let y = list[1].as_i64().unwrap_or(x as i64).clamp(1, 256) as u32;
            return Some((x, y));
        }
    }
    if let Some(x) = value.field_i64("x").or_else(|| value.field_i64("width")).or_else(|| value.field_i64("divisionX")) {
        let y = value.field_i64("y").or_else(|| value.field_i64("height")).or_else(|| value.field_i64("divisionY")).unwrap_or(x);
        return Some((x.clamp(1, 256) as u32, y.clamp(1, 256) as u32));
    }
    None
}

fn parse_content_mesh_patch(content: &PsbValue, division_x: u32, division_y: u32) -> Option<EmoteMeshPatch> {
    if let Some(mesh) = content.field("mesh") {
        if let Some(patch) = parse_mesh_dict_patch(mesh, division_x, division_y) {
            return Some(patch);
        }
    }
    if let Some(bp) = content.field("mbp") {
        return parse_bezier_patch(bp, division_x, division_y);
    }
    None
}

fn parse_mesh_dict_patch(mesh: &PsbValue, division_x: u32, division_y: u32) -> Option<EmoteMeshPatch> {
    parse_bezier_patch(mesh.field("bp")?, division_x, division_y)
}

fn parse_bezier_patch(bp: &PsbValue, division_x: u32, division_y: u32) -> Option<EmoteMeshPatch> {
    match bp {
        PsbValue::Null => Some(EmoteMeshPatch::identity(division_x, division_y)),
        PsbValue::List(values) if values.len() >= 32 => {
            let mut patch = EmoteMeshPatch::identity(division_x, division_y);
            for i in 0..16 {
                patch.control_points[i] = [
                    values[i * 2].as_f32().unwrap_or(patch.control_points[i][0]),
                    values[i * 2 + 1].as_f32().unwrap_or(patch.control_points[i][1]),
                ];
            }
            Some(patch)
        }
        _ => None,
    }
}

fn evaluate_one_combinator(
    combinator: &PsbValue,
    psb: &PsbFile,
    psb_data: &[u8],
    variables: &BTreeMap<String, f32>,
    division_x: u32,
    division_y: u32,
    is_delta: bool,
) -> Option<EmoteMeshPatch> {
    let variable = combinator.field("variable")?;
    let key = variable.field_str("key")?;
    let mesh_count = variable.field_i64("meshCount")?.max(1) as usize;
    let begin = variable.field_f32("rangeBegin").unwrap_or(0.0);
    let end = variable.field_f32("rangeEnd").unwrap_or(1.0);
    let value = variables.get(key).copied().unwrap_or_else(|| {
        if begin <= 0.0 && end >= 0.0 { 0.0 } else { (begin + end) * 0.5 }
    });
    let neutral_index = combinator.field_i64("neutralIndex").unwrap_or(-1);
    let resource_index = combinator.field("rawMeshList")?.as_u32()? as usize;
    let raw = psb.resource_bytes(psb_data, resource_index)?;
    let meshes = decode_raw_mesh_list(raw, mesh_count, is_delta)?;
    if meshes.is_empty() {
        return None;
    }

    let pos = if mesh_count <= 1 || !begin.is_finite() || !end.is_finite() || (end - begin).abs() <= f32::EPSILON {
        0.0
    } else {
        ((value - begin) / (end - begin)).clamp(0.0, 1.0) * (mesh_count as f32 - 1.0)
    };
    let i0 = pos.floor() as usize;
    let i1 = pos.ceil() as usize;
    let t = pos - i0 as f32;
    let p0 = mesh_patch_from_values(meshes.get(i0)?, neutral_index == i0 as i64, division_x, division_y);
    let p1 = mesh_patch_from_values(meshes.get(i1).unwrap_or(&meshes[i0]), neutral_index == i1 as i64, division_x, division_y);
    Some(EmoteMeshPatch::interpolate(&p0, &p1, t))
}

fn mesh_patch_from_values(values: &[f32; 32], neutral: bool, division_x: u32, division_y: u32) -> EmoteMeshPatch {
    if neutral {
        return EmoteMeshPatch::identity(division_x, division_y);
    }
    let mut patch = EmoteMeshPatch::identity(division_x, division_y);
    for i in 0..16 {
        patch.control_points[i] = [values[i * 2], values[i * 2 + 1]];
    }
    patch
}

fn decode_raw_mesh_list(raw: &[u8], mesh_count: usize, is_delta: bool) -> Option<Vec<[f32; 32]>> {
    let total = mesh_count.checked_mul(32)?;
    let mut values = vec![0.0f32; total];
    if raw.len() >= total * 8 {
        for (i, chunk) in raw.chunks_exact(8).take(total).enumerate() {
            values[i] = f64::from_le_bytes(chunk.try_into().ok()?) as f32;
        }
    } else if raw.len() >= total * 4 {
        for (i, chunk) in raw.chunks_exact(4).take(total).enumerate() {
            values[i] = f32::from_le_bytes(chunk.try_into().ok()?);
        }
    } else {
        return None;
    }

    if is_delta {
        for mesh_index in 0..mesh_count {
            for row in 0..4 {
                for col in 0..4 {
                    let base = mesh_index * 32 + (row * 4 + col) * 2;
                    values[base] += col as f32 / 3.0;
                    values[base + 1] += row as f32 / 3.0;
                }
            }
        }
    }

    let mut out = Vec::with_capacity(mesh_count);
    for mesh_index in 0..mesh_count {
        let mut mesh = [0.0f32; 32];
        mesh.copy_from_slice(&values[mesh_index * 32..mesh_index * 32 + 32]);
        out.push(mesh);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_bounds() {
        let sprite = EmoteStaticSprite {
            label: None,
            motion_name: "main".to_owned(),
            texture_name: "tex".to_owned(),
            texture_resource_index: 0,
            texture_width: 100,
            texture_height: 100,
            texture_format: Some("RGBA8".to_owned()),
            icon_name: "icon".to_owned(),
            z: 0.0,
            opacity: 1.0,
            visible: true,
            center_x: 10.0,
            center_y: 20.0,
            width: 40.0,
            height: 60.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_degrees: 0.0,
            world_transform: EmoteTransform2D::identity().as_array(),
            uv_left: 0.0,
            uv_top: 0.0,
            uv_right: 1.0,
            uv_bottom: 1.0,
            mesh: None,
        };
        let b = compute_bounds(&[sprite]).unwrap();
        assert_eq!(b.min_x, -10.0);
        assert_eq!(b.max_y, 50.0);
    }
}
