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
        self.psb
            .resource_bytes(&self.normalized_data, resource_index as usize)
    }

    /// Samples `animation` at `ticks` and returns the draw list in draw order.
    ///
    /// One tick is 1/60 s ([`EMOTE_TICKS_PER_SECOND`]); the caller advances its
    /// own clock and passes the accumulated ticks. Sampling past the animation
    /// duration wraps around it.
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

impl MotionDrawItem {
    fn from_sprite(sprite: &EmoteStaticSprite) -> Self {
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
            world_transform: sprite.world_transform,
            mesh: sprite.mesh,
            label: sprite.label.clone(),
            motion: sprite.motion_name.clone(),
            draw_index: sprite.draw_frame_info.draw_index,
            pass: sprite.draw_frame_info.pass,
        }
    }
}
