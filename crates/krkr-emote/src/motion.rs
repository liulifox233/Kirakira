//! Loading a `.mtn` motion and sampling its draw list.

use std::collections::BTreeMap;

use eluna::{
    EmoteModelSchema, EmoteStaticScene, EmoteStaticSprite, PsbDecryptionKey, PsbFile,
    PsbNormalizeOptions,
};

use crate::error::MotionError;
use crate::model::{
    MotionAnimation, MotionDrawItem, MotionSource, build_animations, build_sources,
};
use crate::normalize::{NormalizeReport, normalize_source_table};

/// A loaded E-mote motion and everything needed to draw it.
///
/// The model (`sources`, `animations`) is built from the file's own shape;
/// `psb`/`schema` are the adapted view eluna's scene builder consumes.
#[derive(Debug, Clone)]
pub struct Motion {
    base_object: String,
    spec: Option<String>,
    sources: BTreeMap<String, MotionSource>,
    animations: Vec<MotionAnimation>,
    schema: EmoteModelSchema,
    psb: PsbFile,
    normalized_data: Vec<u8>,
    normalize_report: NormalizeReport,
    texture_resources: BTreeMap<u32, TextureResource>,
}

/// How to decode one icon's pixel resource.
#[derive(Debug, Clone, PartialEq)]
struct TextureResource {
    width: u32,
    height: u32,
    palette_resource_index: Option<u32>,
    compress: Option<String>,
}

impl Motion {
    /// Parses an unencrypted `.mtn` (PSB v3/v4) motion from its storage bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MotionError> {
        Self::from_bytes_with_options(bytes, PsbNormalizeOptions::default())
    }

    /// Parses a motion whose PSB header/body is encrypted with the Emote key
    /// family and the given private key DWORD.
    pub fn from_bytes_with_key(bytes: &[u8], key: u32) -> Result<Self, MotionError> {
        Self::from_bytes_with_options(
            bytes,
            PsbNormalizeOptions {
                decrypt_key: Some(PsbDecryptionKey::emote_key(key)),
                ..PsbNormalizeOptions::default()
            },
        )
    }

    fn from_bytes_with_options(
        bytes: &[u8],
        options: PsbNormalizeOptions,
    ) -> Result<Self, MotionError> {
        let (normalized_data, psb) = PsbFile::parse_normalized(bytes, &options)?;

        let sources = build_sources(&psb);
        let spec = psb.root.field_str("spec").map(str::to_owned);
        let texture_resources = build_texture_resources(&sources);

        let mut adapted = psb.clone();
        let normalize_report = normalize_source_table(&mut adapted);
        let schema = EmoteModelSchema::from_psb(&adapted)?;
        let animations = build_animations(&psb, &schema.base_object, &sources);

        Ok(Self {
            base_object: schema.base_object.clone(),
            spec,
            sources,
            animations,
            schema,
            psb: adapted,
            normalized_data,
            normalize_report,
            texture_resources,
        })
    }

    /// The `object.<name>` entry the motions hang off.
    pub fn base_object(&self) -> &str {
        &self.base_object
    }

    /// The root `spec` string, when the file carries one.
    pub fn spec(&self) -> Option<&str> {
        self.spec.as_deref()
    }

    /// The file's source table: every texture source with its icons.
    pub fn sources(&self) -> &BTreeMap<String, MotionSource> {
        &self.sources
    }

    /// One source of the table, by name.
    pub fn source(&self, name: &str) -> Option<&MotionSource> {
        self.sources.get(name)
    }

    /// The file's animations (`object.<base>.motion.<name>`), in file order.
    ///
    /// Only the base object's motions are listed; a layer can still pull in
    /// another motion (`src` of the form `motion/<object>/<motion>`) from a
    /// different object of the same file, and the draw list reports it as that
    /// motion's sprites.
    pub fn animations(&self) -> &[MotionAnimation] {
        &self.animations
    }

    /// One animation, by name.
    pub fn animation(&self, name: &str) -> Option<&MotionAnimation> {
        self.animations
            .iter()
            .find(|animation| animation.name == name)
    }

    /// What the PARQUET-flavor adaptation changed while loading.
    pub fn normalize_report(&self) -> NormalizeReport {
        self.normalize_report
    }

    /// The adapted (eluna-ready) schema, including the synthetic texture keys.
    pub fn schema(&self) -> &EmoteModelSchema {
        &self.schema
    }

    /// The adapted PSB tree eluna's scene builder reads.
    pub fn psb(&self) -> &PsbFile {
        &self.psb
    }

    /// The decrypted/normalised PSB bytes matching [`Motion::psb`].
    pub fn psb_bytes(&self) -> &[u8] {
        &self.normalized_data
    }

    /// Raw bytes of one PSB resource (the pixels of `MotionIcon`s, meshes, …).
    pub fn texture_bytes(&self, resource_index: u32) -> Option<&[u8]> {
        self.resource_bytes(resource_index)
    }

    /// Raw bytes of any PSB resource, by index.
    pub fn resource_bytes(&self, resource_index: u32) -> Option<&[u8]> {
        self.psb
            .resource_bytes(&self.normalized_data, resource_index as usize)
    }

    /// One icon's pixels decoded to RGBA (the RL/palette decoder in
    /// `src/decode.rs`).
    ///
    /// The resource's pixel format comes from the icon that references it: a
    /// `pal` makes it 8-bit paletted, otherwise it is RGBA, and the `compress`
    /// tag selects the RL codec. A resource that no icon references has no
    /// known dimensions and is reported as
    /// [`MotionError::UnknownTextureResource`].
    pub fn texture_pixels(
        &self,
        resource_index: u32,
    ) -> Result<crate::decode::DecodedTexture, MotionError> {
        let info = self
            .texture_resources
            .get(&resource_index)
            .ok_or(MotionError::UnknownTextureResource(resource_index))?;
        let pixels = self
            .resource_bytes(resource_index)
            .ok_or(MotionError::MissingResource(resource_index))?;
        let palette = match info.palette_resource_index {
            Some(index) => Some(
                self.resource_bytes(index)
                    .ok_or(MotionError::MissingResource(index))?,
            ),
            None => None,
        };
        crate::decode::decode_icon(
            pixels,
            palette,
            info.width,
            info.height,
            info.compress.as_deref(),
        )
        .map_err(MotionError::from)
    }

    /// Samples `animation` at `ticks` and returns the draw list in draw order.
    ///
    /// One tick is 1/60 s ([`crate::EMOTE_TICKS_PER_SECOND`]); the caller advances its
    /// own clock and passes the accumulated ticks. Sampling at or past the
    /// animation's duration holds its final frame — eluna does not wrap, the
    /// caller does (`crates/krkr-plugins/src/motion_player.rs`'s
    /// `advance_player` wraps on the motion's `loopTime`).
    pub fn draw_list(
        &self,
        animation: &str,
        ticks: f32,
    ) -> Result<Vec<MotionDrawItem>, MotionError> {
        self.scene_at(animation, ticks).map(|scene| {
            scene
                .sprites
                .iter()
                .map(MotionDrawItem::from_sprite)
                .collect()
        })
    }

    /// [`Motion::draw_list`] with runtime variable values (parameterised layers).
    pub fn draw_list_with_variables(
        &self,
        animation: &str,
        ticks: f32,
        variables: &BTreeMap<String, f32>,
    ) -> Result<Vec<MotionDrawItem>, MotionError> {
        self.scene_at_with_variables(animation, ticks, variables)
            .map(|scene| {
                scene
                    .sprites
                    .iter()
                    .map(MotionDrawItem::from_sprite)
                    .collect()
            })
    }

    /// The full eluna scene (sprites plus bounds and per-sprite draw metadata).
    pub fn scene_at(&self, animation: &str, ticks: f32) -> Result<EmoteStaticScene, MotionError> {
        self.scene_at_with_variables(animation, ticks, &BTreeMap::new())
    }

    /// [`Motion::scene_at`] with runtime variable values.
    pub fn scene_at_with_variables(
        &self,
        animation: &str,
        ticks: f32,
        variables: &BTreeMap<String, f32>,
    ) -> Result<EmoteStaticScene, MotionError> {
        if self.animation(animation).is_none() {
            return Err(MotionError::MissingAnimation(animation.to_owned()));
        }
        Ok(self
            .schema
            .build_motion_scene_at_with_resources_and_variables(
                &self.psb,
                &self.normalized_data,
                animation,
                ticks,
                variables,
            )?)
    }

    /// Texture metadata for one adapted texture key (`"<source>/<icon>"`).
    pub fn texture_source(&self, name: &str) -> Option<&eluna::EmoteTextureSource> {
        self.schema.textures.get(name)
    }
}

/// Indexes every icon's pixel resource so [`Motion::texture_pixels`] knows a
/// resource's dimensions, palette and codec without walking the source table.
///
/// A resource shared by several icons takes the first icon's description; the
/// file's own writers never mix formats for one resource.
fn build_texture_resources(
    sources: &BTreeMap<String, MotionSource>,
) -> BTreeMap<u32, TextureResource> {
    let mut resources = BTreeMap::new();
    for source in sources.values() {
        // A FreeMote source's icons are sub-rectangles of one texture: the
        // resource's own dimensions are what decodes, and the icon rectangle
        // travels as the draw item's `uv`.
        if let Some(texture) = &source.texture {
            resources.insert(
                texture.resource_index,
                TextureResource {
                    width: pixel_dimension(texture.width),
                    height: pixel_dimension(texture.height),
                    palette_resource_index: None,
                    compress: source
                        .icons
                        .values()
                        .next()
                        .and_then(|icon| icon.compress.clone()),
                },
            );
        }
        for icon in source.icons.values() {
            resources
                .entry(icon.resource_index)
                .or_insert_with(|| TextureResource {
                    width: pixel_dimension(icon.width),
                    height: pixel_dimension(icon.height),
                    palette_resource_index: icon.palette_resource_index,
                    compress: icon.compress.clone(),
                });
        }
    }
    resources
}

fn pixel_dimension(value: f32) -> u32 {
    if !value.is_finite() || value < 1.0 {
        1
    } else {
        value.round() as u32
    }
}

impl MotionDrawItem {
    pub(crate) fn from_sprite(sprite: &EmoteStaticSprite) -> Self {
        Self {
            texture: sprite.texture_name.clone(),
            resource_index: sprite.texture_resource_index,
            icon: sprite.icon_name.clone(),
            uv: [
                sprite.uv_left,
                sprite.uv_top,
                sprite.uv_right,
                sprite.uv_bottom,
            ],
            center: [sprite.center_x, sprite.center_y],
            size: [sprite.width, sprite.height],
            scale: [sprite.scale_x, sprite.scale_y],
            rotation_degrees: sprite.rotation_degrees,
            opacity: sprite.opacity,
            z: sprite.z,
            visible: sprite.visible,
            blend_mode: sprite.blend_mode,
            blend_parameter: sprite.blend_parameter,
            corner_colors: sprite.corner_colors,
            world_transform: sprite.world_transform,
            mesh: sprite.mesh,
            label: sprite.label.clone(),
            motion: sprite.motion_name.clone(),
            draw_index: sprite.draw_frame_info.draw_index,
            pass: sprite.draw_frame_info.pass,
        }
    }
}
