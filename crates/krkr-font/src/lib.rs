use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

use fontdb::{Database, Family, Query, Source, Stretch, Style as FontStyle, Weight};
pub use krkr_core::{FontSpec, ShadowStyle, TextStyle};
use swash::{
    FontRef,
    scale::{Render, ScaleContext, Source as GlyphSource, StrikeWith, image::Content},
    shape::ShapeContext,
    zeno::{Format, Vector},
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
    pub width: f32,
    pub height: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlyphDrawRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlyphKey {
    pub face: FontFaceKey,
    pub glyph_id: u16,
    pub size_px: u32,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontFaceKey(fontdb::ID);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlyphContent {
    Alpha,
    Subpixel,
    Color,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphImage {
    pub key: GlyphKey,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub content: GlyphContent,
    pub data: Vec<u8>,
}

const PRERENDERED_FONT_SIGNATURE: &[u8; 22] = b"TVP pre-rendered font\x1a";
const PRERENDERED_FONT_HEADER_LEN: usize = 36;
const PRERENDERED_FONT_ITEM_LEN: usize = 20;

#[derive(Clone, Debug)]
struct PrerenderedFont {
    version: u8,
    data: Arc<[u8]>,
    glyphs: BTreeMap<u16, PrerenderedGlyph>,
}

#[derive(Clone, Copy, Debug)]
struct PrerenderedGlyph {
    offset: usize,
    width: u16,
    height: u16,
    origin_x: i16,
    origin_y: i16,
    increment_x: i16,
    increment_y: i16,
}

impl PrerenderedFont {
    fn parse(data: Arc<[u8]>) -> Result<Self, String> {
        if data.len() < PRERENDERED_FONT_HEADER_LEN
            || data.get(..PRERENDERED_FONT_SIGNATURE.len()) != Some(PRERENDERED_FONT_SIGNATURE)
        {
            return Err("invalid TVP pre-rendered font signature".to_owned());
        }
        let version = data[22];
        if version > 1 {
            return Err(format!(
                "unsupported TVP pre-rendered font version {version}"
            ));
        }
        if data[23] != 2 {
            return Err(format!(
                "unsupported TVP pre-rendered font character width {}",
                data[23]
            ));
        }

        let count = read_u32_le(&data, 24)? as usize;
        let chars_offset = read_u32_le(&data, 28)? as usize;
        let items_offset = read_u32_le(&data, 32)? as usize;
        let chars_len = count
            .checked_mul(2)
            .ok_or_else(|| "TVP pre-rendered font character index is too large".to_owned())?;
        let items_len = count
            .checked_mul(PRERENDERED_FONT_ITEM_LEN)
            .ok_or_else(|| "TVP pre-rendered font glyph index is too large".to_owned())?;
        checked_range(data.len(), chars_offset, chars_len, "character index")?;
        checked_range(data.len(), items_offset, items_len, "glyph index")?;

        let mut glyphs = BTreeMap::new();
        for index in 0..count {
            let ch = read_u16_le(&data, chars_offset + index * 2)?;
            let item = items_offset + index * PRERENDERED_FONT_ITEM_LEN;
            let offset = read_u32_le(&data, item)? as usize;
            let width = read_u16_le(&data, item + 4)?;
            let height = read_u16_le(&data, item + 6)?;
            if width != 0 && height != 0 && offset >= data.len() {
                return Err(format!(
                    "TVP pre-rendered font glyph U+{ch:04X} has an invalid bitmap offset"
                ));
            }
            glyphs.insert(
                ch,
                PrerenderedGlyph {
                    offset,
                    width,
                    height,
                    origin_x: read_i16_le(&data, item + 8)?,
                    origin_y: read_i16_le(&data, item + 10)?,
                    increment_x: read_i16_le(&data, item + 12)?,
                    increment_y: read_i16_le(&data, item + 14)?,
                },
            );
        }

        Ok(Self {
            version,
            data,
            glyphs,
        })
    }

    fn decode_glyph(&self, glyph: PrerenderedGlyph) -> Option<Vec<u8>> {
        let pixel_count = usize::from(glyph.width).checked_mul(usize::from(glyph.height))?;
        if pixel_count == 0 {
            return Some(Vec::new());
        }
        let mut source = glyph.offset;
        let mut bitmap = Vec::with_capacity(pixel_count);
        while bitmap.len() < pixel_count {
            let value = *self.data.get(source)?;
            source += 1;
            let repeat = match self.version {
                0 if value == 0x41 => {
                    let count = usize::from(*self.data.get(source)?);
                    source += 1;
                    Some(count)
                }
                1 if value >= 0x41 => Some(usize::from(value - 0x40)),
                _ => None,
            };
            if let Some(repeat) = repeat {
                let previous = *bitmap.last()?;
                if bitmap.len().checked_add(repeat)? > pixel_count {
                    return None;
                }
                bitmap.resize(bitmap.len() + repeat, previous);
            } else {
                bitmap.push(value);
            }
        }
        for alpha in &mut bitmap {
            *alpha = ((u16::from((*alpha).min(64)) * 255 + 32) / 64) as u8;
        }
        Some(bitmap)
    }
}

fn checked_range(data_len: usize, offset: usize, len: usize, context: &str) -> Result<(), String> {
    if offset.checked_add(len).is_some_and(|end| end <= data_len) {
        Ok(())
    } else {
        Err(format!(
            "TVP pre-rendered font {context} is outside the payload"
        ))
    }
}

fn read_u16_le(data: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| "truncated TVP pre-rendered font".to_owned())?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_i16_le(data: &[u8], offset: usize) -> Result<i16, String> {
    read_u16_le(data, offset).map(|value| value as i16)
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| "truncated TVP pre-rendered font".to_owned())?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[derive(Debug)]
pub struct FontSystem {
    db: Database,
    named_file_faces: BTreeMap<String, Vec<fontdb::ID>>,
    prerendered_fonts: BTreeMap<PrerenderedFontKey, PrerenderedFont>,
    primary_faces: RefCell<BTreeMap<FaceSelectionKey, Option<fontdb::ID>>>,
    char_faces: RefCell<BTreeMap<CharFaceSelectionKey, Option<fontdb::ID>>>,
    recent_fallback_faces: RefCell<BTreeMap<FaceSelectionKey, fontdb::ID>>,
    glyph_ids: RefCell<BTreeMap<(FontFaceKey, char), Option<u16>>>,
    face_metrics: RefCell<BTreeMap<FaceMetricsKey, swash::Metrics>>,
    glyph_images: RefCell<BTreeMap<RenderedGlyphKey, Arc<GlyphImage>>>,
    prerendered_glyph_images: RefCell<BTreeMap<(PrerenderedFontKey, u16), Arc<GlyphImage>>>,
}

impl Default for FontSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FontSystem {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            named_file_faces: self.named_file_faces.clone(),
            prerendered_fonts: self.prerendered_fonts.clone(),
            primary_faces: RefCell::new(BTreeMap::new()),
            char_faces: RefCell::new(BTreeMap::new()),
            recent_fallback_faces: RefCell::new(BTreeMap::new()),
            glyph_ids: RefCell::new(BTreeMap::new()),
            face_metrics: RefCell::new(BTreeMap::new()),
            glyph_images: RefCell::new(BTreeMap::new()),
            prerendered_glyph_images: RefCell::new(BTreeMap::new()),
        }
    }
}

impl FontSystem {
    pub fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let mut db = Database::new();
        #[cfg(target_arch = "wasm32")]
        let db = Database::new();
        // Browsers do not expose a process-wide system font directory to WASM.
        // `fontdb::Database::load_system_fonts` may panic through unsupported
        // filesystem APIs on wasm targets, so defer font discovery there until
        // game-provided font data is loaded explicitly.
        #[cfg(not(target_arch = "wasm32"))]
        db.load_system_fonts();
        Self {
            db,
            named_file_faces: BTreeMap::new(),
            prerendered_fonts: BTreeMap::new(),
            primary_faces: RefCell::new(BTreeMap::new()),
            char_faces: RefCell::new(BTreeMap::new()),
            recent_fallback_faces: RefCell::new(BTreeMap::new()),
            glyph_ids: RefCell::new(BTreeMap::new()),
            face_metrics: RefCell::new(BTreeMap::new()),
            glyph_images: RefCell::new(BTreeMap::new()),
            prerendered_glyph_images: RefCell::new(BTreeMap::new()),
        }
    }

    pub fn families(&self) -> Vec<String> {
        let mut names = self
            .db
            .faces()
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    }

    pub fn load_font_data(&mut self, name: impl Into<String>, data: Vec<u8>) -> Result<(), String> {
        let name = name.into();
        if self.named_file_faces.contains_key(&name) {
            return Ok(());
        }
        let ids = self.db.load_font_source(Source::Binary(Arc::new(data)));
        if ids.is_empty() {
            return Err(format!("font `{name}` did not contain a supported face"));
        }
        self.named_file_faces
            .insert(name, ids.into_iter().collect());
        self.clear_caches();
        Ok(())
    }

    pub fn map_prerendered_font(
        &mut self,
        name: impl Into<String>,
        data: Vec<u8>,
    ) -> Result<(), String> {
        self.map_prerendered_font_arc(name, Arc::from(data))
    }

    pub fn map_prerendered_font_arc(
        &mut self,
        name: impl Into<String>,
        data: Arc<[u8]>,
    ) -> Result<(), String> {
        let spec = FontSpec {
            face: name.into(),
            ..FontSpec::default()
        };
        self.map_prerendered_font_for_spec_arc(&spec, data)
    }

    pub fn map_prerendered_font_for_spec_arc(
        &mut self,
        spec: &FontSpec,
        data: Arc<[u8]>,
    ) -> Result<(), String> {
        let font = PrerenderedFont::parse(data)?;
        self.prerendered_fonts
            .insert(PrerenderedFontKey::new(spec), font);
        self.clear_caches();
        Ok(())
    }

    pub fn unmap_prerendered_font(&mut self, name: &str) -> bool {
        let spec = FontSpec {
            face: name.to_owned(),
            ..FontSpec::default()
        };
        self.unmap_prerendered_font_for_spec(&spec)
    }

    pub fn unmap_prerendered_font_for_spec(&mut self, spec: &FontSpec) -> bool {
        let removed = self
            .prerendered_fonts
            .remove(&PrerenderedFontKey::new(spec))
            .is_some();
        if removed {
            self.clear_caches();
        }
        removed
    }

    pub fn text_metrics(&self, spec: &FontSpec, text: &str) -> TextMetrics {
        let layout = self.layout_text(spec, text);
        layout.metrics()
    }

    pub fn esc_width(&self, spec: &FontSpec, text: &str) -> (f32, f32) {
        rotate_vector(self.text_metrics(spec, text).width, 0.0, spec.angle)
    }

    pub fn esc_height(&self, spec: &FontSpec, text: &str) -> (f32, f32) {
        rotate_vector(0.0, self.text_metrics(spec, text).height, spec.angle)
    }

    pub fn glyph_draw_rect(&self, spec: &FontSpec, ch: char) -> Option<GlyphDrawRect> {
        if let Some(glyph) = self.prerendered_glyph(spec, ch) {
            return Some(GlyphDrawRect {
                left: i32::from(glyph.origin_x),
                top: i32::from(glyph.origin_y),
                width: u32::from(glyph.width),
                height: u32::from(glyph.height),
            });
        }
        let primary_face = self.select_primary_face(spec)?;
        let face = if self.face_supports(primary_face, ch) {
            primary_face
        } else {
            self.select_face_for_char(spec, ch, primary_face)
                .unwrap_or(primary_face)
        };
        let glyph_id = self
            .glyph_id(face, ch)
            .or_else(|| self.tofu_glyph_id(face))?;
        let image = self.render_glyph(face, spec, glyph_id, 0.0, 0.0)?;
        Some(GlyphDrawRect {
            left: image.left,
            top: image.top,
            width: image.width,
            height: image.height,
        })
    }

    pub fn rasterize_text(&self, spec: &FontSpec, style: TextStyle, text: &str) -> RgbaTextImage {
        let layout = self.layout_text(spec, text);
        let metrics = layout.metrics();
        let width = metrics.width.ceil().max(1.0) as u32;
        let height = metrics.height.ceil().max(1.0) as u32;
        let mut rgba = vec![0; width as usize * height as usize * 4];
        self.draw_text_layout_to_rgba(spec, style, &mut rgba, width, height, 0, 0, &layout);
        RgbaTextImage {
            width,
            height,
            rgba,
            metrics,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_to_rgba(
        &self,
        spec: &FontSpec,
        style: TextStyle,
        dest: &mut [u8],
        dest_width: u32,
        dest_height: u32,
        x: i32,
        y: i32,
        text: &str,
    ) {
        let layout = self.layout_text(spec, text);
        self.draw_text_layout_to_rgba(spec, style, dest, dest_width, dest_height, x, y, &layout);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_layout_to_rgba(
        &self,
        spec: &FontSpec,
        style: TextStyle,
        dest: &mut [u8],
        dest_width: u32,
        dest_height: u32,
        x: i32,
        y: i32,
        layout: &TextLayout,
    ) {
        if let Some(shadow) = style.shadow {
            self.draw_layout(
                spec,
                layout,
                dest,
                dest_width,
                dest_height,
                x + shadow.offset_x,
                y + shadow.offset_y,
                shadow.color,
            );
        }
        self.draw_layout(
            spec,
            layout,
            dest,
            dest_width,
            dest_height,
            x,
            y,
            style.color,
        );
    }

    pub fn layout_text(&self, spec: &FontSpec, text: &str) -> TextLayout {
        let face = self.select_primary_face(spec);
        let Some(primary_face) = face else {
            return fallback_layout(spec, text);
        };
        if self
            .prerendered_fonts
            .contains_key(&PrerenderedFontKey::new(spec))
        {
            return self.layout_prerendered_text(spec, text, primary_face);
        }
        let primary_metrics = self.face_metrics(primary_face, spec.resolved_height());
        let ascent = primary_metrics.ascent.max(spec.resolved_height() * 0.8);
        let descent = (-primary_metrics.descent).max(spec.resolved_height() * 0.2);
        let line_gap = primary_metrics.leading.max(0.0);
        let line_height = (ascent + descent + line_gap).max(spec.resolved_height());

        let mut glyphs = Vec::new();
        let mut pen_x = 0.0_f32;
        let mut pen_y = 0.0_f32;
        let mut max_width = 0.0_f32;
        let mut lines = 1_u32;
        let mut run = String::new();
        let mut run_face = primary_face;

        let flush_run = |run: &mut String,
                         run_face: fontdb::ID,
                         pen_x: &mut f32,
                         pen_y: f32,
                         glyphs: &mut Vec<PositionedGlyph>| {
            if run.is_empty() {
                return;
            }
            self.shape_run(spec, run_face, run, *pen_x, pen_y, glyphs);
            *pen_x = glyphs
                .last()
                .map(|glyph| glyph.pen_x + glyph.advance)
                .unwrap_or(*pen_x);
            run.clear();
        };

        for ch in text.chars() {
            if ch == '\n' {
                flush_run(&mut run, run_face, &mut pen_x, pen_y, &mut glyphs);
                max_width = max_width.max(pen_x);
                pen_x = 0.0;
                pen_y += line_height;
                lines = lines.saturating_add(1);
                continue;
            }
            let ch_face = if self.face_supports(run_face, ch) {
                run_face
            } else if run_face != primary_face && self.face_supports(primary_face, ch) {
                primary_face
            } else {
                self.select_face_for_char(spec, ch, primary_face)
                    .unwrap_or(primary_face)
            };
            if !run.is_empty() && ch_face != run_face {
                flush_run(&mut run, run_face, &mut pen_x, pen_y, &mut glyphs);
            }
            run_face = ch_face;
            run.push(ch);
        }
        flush_run(&mut run, run_face, &mut pen_x, pen_y, &mut glyphs);
        max_width = max_width.max(pen_x);

        TextLayout {
            glyphs,
            metrics: TextMetrics {
                width: max_width.max(0.0),
                height: (lines as f32 * line_height).max(line_height),
                ascent,
                descent,
                line_gap,
            },
        }
    }

    fn layout_prerendered_text(
        &self,
        spec: &FontSpec,
        text: &str,
        primary_face: fontdb::ID,
    ) -> TextLayout {
        let primary_metrics = self.face_metrics(primary_face, spec.resolved_height());
        let ascent = primary_metrics.ascent.max(spec.resolved_height() * 0.8);
        let descent = (-primary_metrics.descent).max(spec.resolved_height() * 0.2);
        let line_gap = primary_metrics.leading.max(0.0);
        let line_height = (ascent + descent + line_gap).max(spec.resolved_height());
        let key = PrerenderedFontKey::new(spec);
        let font = &self.prerendered_fonts[&key];

        let mut glyphs = Vec::new();
        let mut pen_x = 0.0_f32;
        let mut pen_y = 0.0_f32;
        let mut max_width = 0.0_f32;
        let mut lines = 1_u32;
        for ch in text.chars() {
            if ch == '\n' {
                max_width = max_width.max(pen_x);
                pen_x = 0.0;
                pen_y += line_height;
                lines = lines.saturating_add(1);
                continue;
            }
            let code = u32::from(ch);
            if code <= u32::from(u16::MAX)
                && let Some(item) = font.glyphs.get(&(code as u16))
            {
                glyphs.push(PositionedGlyph {
                    face: primary_face,
                    glyph_id: code as u16,
                    prerendered_char: Some(code as u16),
                    pen_x,
                    line_y: pen_y,
                    x: 0.0,
                    y: 0.0,
                    advance: f32::from(item.increment_x),
                });
                pen_x += f32::from(item.increment_x);
                // The original renderer also advances vertically for rotated
                // fonts. Kirakira's current text layout is horizontal, so keep
                // the value parsed for compatibility without applying it yet.
                let _ = item.increment_y;
                continue;
            }

            let face = if self.face_supports(primary_face, ch) {
                primary_face
            } else {
                self.select_face_for_char(spec, ch, primary_face)
                    .unwrap_or(primary_face)
            };
            let start = glyphs.len();
            self.shape_run(spec, face, &ch.to_string(), pen_x, pen_y, &mut glyphs);
            if let Some(last) = glyphs.get(start..).and_then(|run| run.last()) {
                pen_x = last.pen_x + last.advance;
            }
        }
        max_width = max_width.max(pen_x);

        TextLayout {
            glyphs,
            metrics: TextMetrics {
                width: max_width.max(0.0),
                height: (lines as f32 * line_height).max(line_height),
                ascent,
                descent,
                line_gap,
            },
        }
    }

    fn shape_run(
        &self,
        spec: &FontSpec,
        face: fontdb::ID,
        text: &str,
        start_x: f32,
        line_y: f32,
        output: &mut Vec<PositionedGlyph>,
    ) {
        let size = spec.resolved_height();
        let mut shaped = false;
        self.with_font(face, |font| {
            let mut context = ShapeContext::new();
            let mut pen_x = start_x;
            let mut shaper = context.builder(font).size(size).build();
            shaper.add_str(text);
            shaper.shape_with(|cluster| {
                for glyph in cluster.glyphs {
                    output.push(PositionedGlyph {
                        face,
                        glyph_id: glyph.id,
                        prerendered_char: None,
                        pen_x,
                        line_y,
                        x: glyph.x,
                        y: glyph.y,
                        advance: glyph.advance,
                    });
                    pen_x += glyph.advance;
                }
            });
            shaped = true;
            Some(())
        });
        if shaped {
            return;
        }

        let advance = size * 0.6;
        for _ in text.chars() {
            output.push(PositionedGlyph {
                face,
                glyph_id: 0,
                prerendered_char: None,
                pen_x: start_x,
                line_y,
                x: 0.0,
                y: 0.0,
                advance,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_layout(
        &self,
        spec: &FontSpec,
        layout: &TextLayout,
        dest: &mut [u8],
        dest_width: u32,
        dest_height: u32,
        x: i32,
        y: i32,
        color: [u8; 4],
    ) {
        self.prepare_layout_glyphs(spec, layout);
        for glyph in &layout.glyphs {
            let image = if let Some(ch) = glyph.prerendered_char {
                self.render_prerendered_glyph(glyph.face, spec, ch)
            } else {
                self.render_glyph(
                    glyph.face,
                    spec,
                    glyph.glyph_id,
                    glyph.pen_x + glyph.x,
                    glyph.line_y - glyph.y,
                )
            };
            let Some(image) = image else {
                continue;
            };
            let baseline_y = y as f32 + layout.metrics.ascent + glyph.line_y;
            let draw_x = x + (glyph.pen_x + glyph.x).floor() as i32 + image.left;
            let draw_y = baseline_y.floor() as i32 - image.top;
            blit_glyph(dest, dest_width, dest_height, draw_x, draw_y, color, &image);
        }
    }

    fn prepare_layout_glyphs(&self, spec: &FontSpec, layout: &TextLayout) {
        let size = spec.resolved_height();
        let mut missing: BTreeMap<FontFaceKey, BTreeMap<RenderedGlyphKey, GlyphRenderRequest>> =
            BTreeMap::new();
        {
            let cache = self.glyph_images.borrow();
            for glyph in &layout.glyphs {
                if glyph.prerendered_char.is_some() {
                    continue;
                }
                let x_subpixel = subpixel_bin(glyph.pen_x + glyph.x);
                let y_subpixel = subpixel_bin(glyph.line_y - glyph.y);
                let key = RenderedGlyphKey {
                    face: FontFaceKey(glyph.face),
                    glyph_id: glyph.glyph_id,
                    size_bits: size.to_bits(),
                    bold: spec.bold,
                    italic: spec.italic,
                    x_subpixel,
                    y_subpixel,
                };
                if cache.contains_key(&key) {
                    continue;
                }
                missing.entry(FontFaceKey(glyph.face)).or_default().insert(
                    key,
                    GlyphRenderRequest {
                        key,
                        glyph_id: glyph.glyph_id,
                        x_subpixel,
                        y_subpixel,
                    },
                );
            }
        }

        for (face, requests) in missing {
            self.render_missing_glyphs(face.0, spec, requests);
        }
    }

    fn render_missing_glyphs(
        &self,
        face: fontdb::ID,
        spec: &FontSpec,
        requests: BTreeMap<RenderedGlyphKey, GlyphRenderRequest>,
    ) {
        let size = spec.resolved_height();
        let embolden = if spec.bold {
            (size / 28.0).max(0.35)
        } else {
            0.0
        };
        self.with_font(face, |font| {
            let mut context = ScaleContext::new();
            let mut scaler = context.builder(font).size(size).hint(true).build();
            let mut renderer = Render::new(&[
                GlyphSource::ColorOutline(0),
                GlyphSource::ColorBitmap(StrikeWith::BestFit),
                GlyphSource::Outline,
            ]);
            renderer.format(Format::Alpha).embolden(embolden);
            for request in requests.into_values() {
                if self.glyph_images.borrow().contains_key(&request.key) {
                    continue;
                }
                renderer.offset(Vector::new(
                    subpixel_offset(request.x_subpixel),
                    subpixel_offset(request.y_subpixel),
                ));
                let Some(image) = renderer.render(&mut scaler, request.glyph_id) else {
                    continue;
                };
                let content = match image.content {
                    Content::Mask => GlyphContent::Alpha,
                    Content::SubpixelMask => GlyphContent::Subpixel,
                    Content::Color => GlyphContent::Color,
                };
                let rendered = Arc::new(GlyphImage {
                    key: GlyphKey {
                        face: FontFaceKey(face),
                        glyph_id: request.glyph_id,
                        size_px: size.round().max(1.0) as u32,
                        bold: spec.bold,
                        italic: spec.italic,
                    },
                    left: image.placement.left,
                    top: image.placement.top,
                    width: image.placement.width,
                    height: image.placement.height,
                    content,
                    data: image.data,
                });
                self.glyph_images.borrow_mut().insert(request.key, rendered);
            }
            Some(())
        });
    }

    fn render_glyph(
        &self,
        face: fontdb::ID,
        spec: &FontSpec,
        glyph_id: u16,
        x: f32,
        y: f32,
    ) -> Option<Arc<GlyphImage>> {
        let size = spec.resolved_height();
        let x_subpixel = subpixel_bin(x);
        let y_subpixel = subpixel_bin(y);
        let key = RenderedGlyphKey {
            face: FontFaceKey(face),
            glyph_id,
            size_bits: size.to_bits(),
            bold: spec.bold,
            italic: spec.italic,
            x_subpixel,
            y_subpixel,
        };
        if let Some(image) = self.glyph_images.borrow().get(&key).cloned() {
            return Some(image);
        }

        let mut rendered = None;
        self.with_font(face, |font| {
            let mut context = ScaleContext::new();
            let mut scaler = context.builder(font).size(size).hint(true).build();
            let mut renderer = Render::new(&[
                GlyphSource::ColorOutline(0),
                GlyphSource::ColorBitmap(StrikeWith::BestFit),
                GlyphSource::Outline,
            ]);
            renderer
                .format(Format::Alpha)
                .offset(Vector::new(
                    subpixel_offset(x_subpixel),
                    subpixel_offset(y_subpixel),
                ))
                .embolden(if spec.bold {
                    (size / 28.0).max(0.35)
                } else {
                    0.0
                });
            let image = renderer.render(&mut scaler, glyph_id)?;
            let content = match image.content {
                Content::Mask => GlyphContent::Alpha,
                Content::SubpixelMask => GlyphContent::Subpixel,
                Content::Color => GlyphContent::Color,
            };
            rendered = Some(Arc::new(GlyphImage {
                key: GlyphKey {
                    face: FontFaceKey(face),
                    glyph_id,
                    size_px: size.round().max(1.0) as u32,
                    bold: spec.bold,
                    italic: spec.italic,
                },
                left: image.placement.left,
                top: image.placement.top,
                width: image.placement.width,
                height: image.placement.height,
                content,
                data: image.data,
            }));
            Some(())
        })?;
        if let Some(image) = rendered {
            self.glyph_images.borrow_mut().insert(key, image.clone());
            Some(image)
        } else {
            None
        }
    }

    fn prerendered_glyph(&self, spec: &FontSpec, ch: char) -> Option<&PrerenderedGlyph> {
        let code = u32::from(ch);
        if code > u32::from(u16::MAX) {
            return None;
        }
        self.prerendered_fonts
            .get(&PrerenderedFontKey::new(spec))?
            .glyphs
            .get(&(code as u16))
    }

    fn render_prerendered_glyph(
        &self,
        face: fontdb::ID,
        spec: &FontSpec,
        ch: u16,
    ) -> Option<Arc<GlyphImage>> {
        let font_key = PrerenderedFontKey::new(spec);
        let cache_key = (font_key.clone(), ch);
        if let Some(image) = self
            .prerendered_glyph_images
            .borrow()
            .get(&cache_key)
            .cloned()
        {
            return Some(image);
        }
        let font = self.prerendered_fonts.get(&font_key)?;
        let glyph = *font.glyphs.get(&ch)?;
        let data = font.decode_glyph(glyph)?;
        let image = Arc::new(GlyphImage {
            key: GlyphKey {
                face: FontFaceKey(face),
                glyph_id: ch,
                size_px: spec.resolved_height().round().max(1.0) as u32,
                bold: spec.bold,
                italic: spec.italic,
            },
            left: i32::from(glyph.origin_x),
            top: i32::from(glyph.origin_y),
            width: u32::from(glyph.width),
            height: u32::from(glyph.height),
            content: GlyphContent::Alpha,
            data,
        });
        self.prerendered_glyph_images
            .borrow_mut()
            .insert(cache_key, image.clone());
        Some(image)
    }

    fn face_metrics(&self, face: fontdb::ID, size: f32) -> swash::Metrics {
        let key = FaceMetricsKey {
            face: FontFaceKey(face),
            size_bits: size.to_bits(),
        };
        if let Some(metrics) = self.face_metrics.borrow().get(&key).copied() {
            return metrics;
        }
        let mut metrics = None;
        self.with_font(face, |font| {
            let mut context = ShapeContext::new();
            let shaper = context.builder(font).size(size).build();
            metrics = Some(shaper.metrics());
            Some(())
        });
        let metrics = metrics.unwrap_or_else(|| swash::Metrics {
            units_per_em: 1,
            glyph_count: 0,
            is_monospace: false,
            has_vertical_metrics: false,
            ascent: size * 0.8,
            descent: -size * 0.2,
            leading: 0.0,
            vertical_ascent: 0.0,
            vertical_descent: 0.0,
            vertical_leading: 0.0,
            cap_height: size * 0.7,
            x_height: size * 0.5,
            average_width: size * 0.5,
            max_width: size,
            underline_offset: 0.0,
            strikeout_offset: size * 0.35,
            stroke_size: 1.0,
        });
        self.face_metrics.borrow_mut().insert(key, metrics);
        metrics
    }

    fn select_primary_face(&self, spec: &FontSpec) -> Option<fontdb::ID> {
        if spec.face_is_file_name {
            return self.select_named_file_face(spec, None);
        }

        let key = FaceSelectionKey::new(spec);
        if let Some(cached) = self.primary_faces.borrow().get(&key).copied() {
            return cached;
        }

        let selected = self
            .query_requested_faces(spec, None)
            .or_else(|| self.query_fallback_faces(spec, None))
            .or_else(|| self.db.faces().next().map(|face| face.id));
        self.primary_faces.borrow_mut().insert(key, selected);
        selected
    }

    fn select_face_for_char(
        &self,
        spec: &FontSpec,
        ch: char,
        primary_face: fontdb::ID,
    ) -> Option<fontdb::ID> {
        if spec.face_is_file_name {
            return self.select_named_file_face(spec, Some(ch));
        }

        let key = FaceSelectionKey::new(spec);
        let char_key = CharFaceSelectionKey {
            key: key.clone(),
            ch,
        };
        if let Some(cached) = self.char_faces.borrow().get(&char_key).copied() {
            return cached;
        }

        let selected = self
            .query_requested_faces(spec, Some(ch))
            .or_else(|| {
                self.recent_fallback_faces
                    .borrow()
                    .get(&key)
                    .copied()
                    .filter(|id| self.face_supports(*id, ch))
            })
            .or_else(|| self.query_fallback_faces(spec, Some(ch)))
            .or_else(|| {
                self.db
                    .faces()
                    .map(|face| face.id)
                    .find(|id| self.face_supports(*id, ch))
            });
        if let Some(id) = selected
            && id != primary_face
        {
            self.recent_fallback_faces
                .borrow_mut()
                .insert(key.clone(), id);
        }
        self.char_faces.borrow_mut().insert(char_key, selected);
        selected
    }

    fn select_named_file_face(&self, spec: &FontSpec, ch: Option<char>) -> Option<fontdb::ID> {
        self.named_file_faces.get(&spec.face).and_then(|ids| {
            ids.iter()
                .copied()
                .find(|id| ch.is_none_or(|ch| self.face_supports(*id, ch)))
                .or_else(|| ids.first().copied())
        })
    }

    fn query_requested_faces(&self, spec: &FontSpec, ch: Option<char>) -> Option<fontdb::ID> {
        let weight = if spec.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        };
        let style = if spec.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };

        for face_name in spec
            .face
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let families = [Family::Name(face_name)];
            let query = Query {
                families: &families,
                weight,
                stretch: Stretch::Normal,
                style,
            };
            if let Some(id) = self.db.query(&query)
                && ch.is_none_or(|ch| self.face_supports(id, ch))
            {
                return Some(id);
            }
        }

        None
    }

    fn query_fallback_faces(&self, spec: &FontSpec, ch: Option<char>) -> Option<fontdb::ID> {
        let weight = if spec.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        };
        let style = if spec.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };

        let fallback_families = [Family::SansSerif, Family::Serif, Family::Monospace];
        for family in fallback_families {
            let families = [family];
            let query = Query {
                families: &families,
                weight,
                stretch: Stretch::Normal,
                style,
            };
            if let Some(id) = self.db.query(&query)
                && ch.is_none_or(|ch| self.face_supports(id, ch))
            {
                return Some(id);
            }
        }

        None
    }

    fn face_supports(&self, face: fontdb::ID, ch: char) -> bool {
        self.glyph_id(face, ch).is_some()
    }

    fn glyph_id(&self, face: fontdb::ID, ch: char) -> Option<u16> {
        let key = (FontFaceKey(face), ch);
        if let Some(glyph_id) = self.glyph_ids.borrow().get(&key).copied() {
            return glyph_id;
        }
        let mut glyph_id = None;
        self.with_font(face, |font| {
            let mapped = font.charmap().map(ch);
            if mapped != 0 {
                glyph_id = Some(mapped);
            }
            Some(())
        });
        self.glyph_ids.borrow_mut().insert(key, glyph_id);
        glyph_id
    }

    fn tofu_glyph_id(&self, face: fontdb::ID) -> Option<u16> {
        ['\u{25a1}', '\u{fffd}', '?']
            .into_iter()
            .find_map(|ch| self.glyph_id(face, ch))
    }

    fn with_font<T>(
        &self,
        face: fontdb::ID,
        f: impl FnOnce(FontRef<'_>) -> Option<T>,
    ) -> Option<T> {
        self.db.with_face_data(face, |data, index| {
            FontRef::from_index(data, index as usize).and_then(f)
        })?
    }

    fn clear_caches(&self) {
        self.primary_faces.borrow_mut().clear();
        self.char_faces.borrow_mut().clear();
        self.recent_fallback_faces.borrow_mut().clear();
        self.glyph_ids.borrow_mut().clear();
        self.face_metrics.borrow_mut().clear();
        self.glyph_images.borrow_mut().clear();
        self.prerendered_glyph_images.borrow_mut().clear();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PrerenderedFontKey {
    face: String,
    height_bits: u32,
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
    angle: i32,
    face_is_file_name: bool,
}

impl PrerenderedFontKey {
    fn new(spec: &FontSpec) -> Self {
        Self {
            face: spec.face.clone(),
            height_bits: spec.resolved_height().to_bits(),
            bold: spec.bold,
            italic: spec.italic,
            underline: spec.underline,
            strikeout: spec.strikeout,
            angle: spec.angle,
            face_is_file_name: spec.face_is_file_name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FaceSelectionKey {
    face: String,
    bold: bool,
    italic: bool,
    face_is_file_name: bool,
}

impl FaceSelectionKey {
    fn new(spec: &FontSpec) -> Self {
        Self {
            face: spec.face.clone(),
            bold: spec.bold,
            italic: spec.italic,
            face_is_file_name: spec.face_is_file_name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CharFaceSelectionKey {
    key: FaceSelectionKey,
    ch: char,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FaceMetricsKey {
    face: FontFaceKey,
    size_bits: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RenderedGlyphKey {
    face: FontFaceKey,
    glyph_id: u16,
    size_bits: u32,
    bold: bool,
    italic: bool,
    x_subpixel: u8,
    y_subpixel: u8,
}

#[derive(Clone, Copy, Debug)]
struct GlyphRenderRequest {
    key: RenderedGlyphKey,
    glyph_id: u16,
    x_subpixel: u8,
    y_subpixel: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RgbaTextImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub metrics: TextMetrics,
}

#[derive(Clone, Debug)]
pub struct TextLayout {
    glyphs: Vec<PositionedGlyph>,
    metrics: TextMetrics,
}

impl TextLayout {
    pub fn metrics(&self) -> TextMetrics {
        self.metrics
    }
}

#[derive(Clone, Copy, Debug)]
struct PositionedGlyph {
    face: fontdb::ID,
    glyph_id: u16,
    prerendered_char: Option<u16>,
    pen_x: f32,
    line_y: f32,
    x: f32,
    y: f32,
    advance: f32,
}

fn fallback_layout(spec: &FontSpec, text: &str) -> TextLayout {
    let size = spec.resolved_height();
    let line_count = text.chars().filter(|ch| *ch == '\n').count() as f32 + 1.0;
    let max_cols = text
        .lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0) as f32;
    TextLayout {
        glyphs: Vec::new(),
        metrics: TextMetrics {
            width: max_cols * size * 0.6,
            height: line_count * size,
            ascent: size * 0.8,
            descent: size * 0.2,
            line_gap: 0.0,
        },
    }
}

fn rotate_vector(x: f32, y: f32, angle_tenths: i32) -> (f32, f32) {
    let radians = (angle_tenths as f32 / 10.0).to_radians();
    let cos = radians.cos();
    let sin = radians.sin();
    (x * cos - y * sin, x * sin + y * cos)
}

fn subpixel_bin(value: f32) -> u8 {
    (value.rem_euclid(1.0) * 64.0).floor().clamp(0.0, 63.0) as u8
}

fn subpixel_offset(bin: u8) -> f32 {
    f32::from(bin.min(63)) / 64.0
}

fn blit_glyph(
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    x: i32,
    y: i32,
    color: [u8; 4],
    image: &GlyphImage,
) {
    if image.width == 0 || image.height == 0 || color[3] == 0 {
        return;
    }

    let col_start = (-x).max(0) as u32;
    let row_start = (-y).max(0) as u32;
    let col_end = (dest_width as i32 - x).min(image.width as i32).max(0) as u32;
    let row_end = (dest_height as i32 - y).min(image.height as i32).max(0) as u32;
    if col_start >= col_end || row_start >= row_end {
        return;
    }

    let pixel_count = image.width as usize * image.height as usize;
    match image.content {
        GlyphContent::Alpha => {
            if image.data.len() < pixel_count {
                return;
            }
            for row in row_start..row_end {
                let dest_y = (y + row as i32) as u32;
                let dest_x = (x + col_start as i32) as u32;
                let mut dest_index = ((dest_y * dest_width + dest_x) * 4) as usize;
                let src_start = (row * image.width + col_start) as usize;
                let src_end = (row * image.width + col_end) as usize;
                for src_index in src_start..src_end {
                    let src_a = multiply_u8(image.data[src_index], color[3]);
                    if src_a != 0 {
                        blend_pixel_channels(
                            &mut dest[dest_index..dest_index + 4],
                            color[0],
                            color[1],
                            color[2],
                            src_a,
                        );
                    }
                    dest_index += 4;
                }
            }
        }
        GlyphContent::Subpixel => {
            if image.data.len() < pixel_count * 4 {
                return;
            }
            for row in row_start..row_end {
                let dest_y = (y + row as i32) as u32;
                let dest_x = (x + col_start as i32) as u32;
                let mut dest_index = ((dest_y * dest_width + dest_x) * 4) as usize;
                let src_start = ((row * image.width + col_start) * 4) as usize;
                let src_end = ((row * image.width + col_end) * 4) as usize;
                for src_index in (src_start..src_end).step_by(4) {
                    let r = image.data[src_index];
                    let g = image.data[src_index + 1];
                    let b = image.data[src_index + 2];
                    let alpha = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
                    let src_a = multiply_u8(alpha, color[3]);
                    if src_a != 0 {
                        blend_pixel_channels(
                            &mut dest[dest_index..dest_index + 4],
                            color[0],
                            color[1],
                            color[2],
                            src_a,
                        );
                    }
                    dest_index += 4;
                }
            }
        }
        GlyphContent::Color => {
            if image.data.len() < pixel_count * 4 {
                return;
            }
            for row in row_start..row_end {
                let dest_y = (y + row as i32) as u32;
                let dest_x = (x + col_start as i32) as u32;
                let mut dest_index = ((dest_y * dest_width + dest_x) * 4) as usize;
                let src_start = ((row * image.width + col_start) * 4) as usize;
                let src_end = ((row * image.width + col_end) * 4) as usize;
                for src_index in (src_start..src_end).step_by(4) {
                    let src_a = multiply_u8(image.data[src_index + 3], color[3]);
                    if src_a != 0 {
                        blend_pixel_channels(
                            &mut dest[dest_index..dest_index + 4],
                            image.data[src_index],
                            image.data[src_index + 1],
                            image.data[src_index + 2],
                            src_a,
                        );
                    }
                    dest_index += 4;
                }
            }
        }
    }
}

fn multiply_u8(a: u8, b: u8) -> u8 {
    divide_by_255(u32::from(a) * u32::from(b)) as u8
}

fn divide_by_255(value: u32) -> u32 {
    (value + 127) / 255
}

fn blend_pixel_channels(dest: &mut [u8], src_r: u8, src_g: u8, src_b: u8, src_a: u8) {
    if src_a == 0 {
        return;
    }
    let src_a = u32::from(src_a);
    let dest_a = u32::from(dest[3]);
    if src_a == 255 || dest_a == 0 {
        dest.copy_from_slice(&[src_r, src_g, src_b, src_a as u8]);
        return;
    }

    let inv_src_a = 255 - src_a;
    let denom = src_a * 255 + dest_a * inv_src_a;
    dest[0] = blend_channel(src_r, src_a, dest[0], dest_a, inv_src_a, denom);
    dest[1] = blend_channel(src_g, src_a, dest[1], dest_a, inv_src_a, denom);
    dest[2] = blend_channel(src_b, src_a, dest[2], dest_a, inv_src_a, denom);
    dest[3] = divide_by_255(denom) as u8;
}

fn blend_channel(src: u8, src_a: u32, dest: u8, dest_a: u32, inv_src_a: u32, denom: u32) -> u8 {
    let value = u32::from(src) * src_a * 255 + u32::from(dest) * dest_a * inv_src_a;
    ((value + denom / 2) / denom) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_prerendered_font() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(PRERENDERED_FONT_SIGNATURE);
        data.extend_from_slice(&[1, 2]);
        data.extend_from_slice(&1_u32.to_le_bytes());
        data.extend_from_slice(&39_u32.to_le_bytes());
        data.extend_from_slice(&41_u32.to_le_bytes());
        data.extend_from_slice(&[0, 64, 0x42]);
        data.extend_from_slice(&(b'A' as u16).to_le_bytes());
        data.extend_from_slice(&36_u32.to_le_bytes());
        data.extend_from_slice(&2_u16.to_le_bytes());
        data.extend_from_slice(&2_u16.to_le_bytes());
        data.extend_from_slice(&1_i16.to_le_bytes());
        data.extend_from_slice(&2_i16.to_le_bytes());
        data.extend_from_slice(&3_i16.to_le_bytes());
        data.extend_from_slice(&0_i16.to_le_bytes());
        data.extend_from_slice(&3_i16.to_le_bytes());
        data.extend_from_slice(&0_u16.to_le_bytes());
        data
    }

    #[test]
    fn measures_ascii_text_with_system_fallback() {
        let system = FontSystem::new();
        let metrics = system.text_metrics(&FontSpec::default(), "Hello");
        assert!(metrics.width > 0.0);
        assert!(metrics.height > 0.0);
    }

    #[test]
    fn rasterizes_text_into_non_empty_rgba() {
        let system = FontSystem::new();
        let image = system.rasterize_text(&FontSpec::default(), TextStyle::default(), "A");
        assert!(image.rgba.chunks_exact(4).any(|px| px[3] != 0));
    }

    #[test]
    fn rotates_escape_width() {
        let system = FontSystem::new();
        let spec = FontSpec {
            angle: 900,
            ..FontSpec::default()
        };
        let (x, y) = system.esc_width(&spec, "A");
        assert!(x.abs() < y.abs().max(1.0));
    }

    #[test]
    fn parses_and_expands_tvp_prerendered_font() {
        let font = PrerenderedFont::parse(Arc::from(test_prerendered_font())).unwrap();
        let glyph = *font.glyphs.get(&(b'A' as u16)).unwrap();
        assert_eq!(glyph.width, 2);
        assert_eq!(glyph.height, 2);
        assert_eq!(glyph.origin_x, 1);
        assert_eq!(glyph.origin_y, 2);
        assert_eq!(glyph.increment_x, 3);
        assert_eq!(font.decode_glyph(glyph).unwrap(), [0, 255, 255, 255]);
    }

    #[test]
    fn mapped_tvp_prerendered_font_drives_metrics_and_rasterization() {
        let mut system = FontSystem::new();
        let spec = FontSpec {
            height: 10.0,
            ..FontSpec::default()
        };
        system
            .map_prerendered_font_for_spec_arc(&spec, Arc::from(test_prerendered_font()))
            .unwrap();

        let metrics = system.text_metrics(&spec, "AA");
        assert_eq!(metrics.width, 6.0);
        let image = system.rasterize_text(&spec, TextStyle::default(), "A");
        assert!(image.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255));
    }
}
