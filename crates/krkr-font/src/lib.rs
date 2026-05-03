use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

use fontdb::{Database, Family, Query, Source, Stretch, Style as FontStyle, Weight};
use swash::{
    FontRef,
    scale::{Render, ScaleContext, Source as GlyphSource, StrikeWith, image::Content},
    shape::ShapeContext,
    zeno::{Format, Vector},
};

#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub face: String,
    pub height: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub angle: i32,
    pub face_is_file_name: bool,
    pub rasterizer: String,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            face: String::new(),
            height: 24.0,
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            angle: 0,
            face_is_file_name: false,
            rasterizer: String::new(),
        }
    }
}

impl FontSpec {
    pub fn resolved_height(&self) -> f32 {
        self.height.abs().max(1.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextStyle {
    pub color: [u8; 4],
    pub anti_alias: bool,
    pub shadow: Option<ShadowStyle>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: [255, 255, 255, 255],
            anti_alias: true,
            shadow: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowStyle {
    pub offset_x: i32,
    pub offset_y: i32,
    pub color: [u8; 4],
}

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

#[derive(Debug)]
pub struct FontSystem {
    db: Database,
    named_file_faces: BTreeMap<String, Vec<fontdb::ID>>,
    prerendered_fonts: BTreeMap<String, Arc<[u8]>>,
    primary_faces: RefCell<BTreeMap<FaceSelectionKey, Option<fontdb::ID>>>,
    char_faces: RefCell<BTreeMap<CharFaceSelectionKey, Option<fontdb::ID>>>,
    recent_fallback_faces: RefCell<BTreeMap<FaceSelectionKey, fontdb::ID>>,
    glyph_ids: RefCell<BTreeMap<(FontFaceKey, char), Option<u16>>>,
    face_metrics: RefCell<BTreeMap<FaceMetricsKey, swash::Metrics>>,
    glyph_images: RefCell<BTreeMap<RenderedGlyphKey, Arc<GlyphImage>>>,
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
        }
    }
}

impl FontSystem {
    pub fn new() -> Self {
        let mut db = Database::new();
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
        let name = name.into();
        let ids = self
            .db
            .load_font_source(Source::Binary(Arc::new(data.as_ref().to_vec())));
        if ids.is_empty() {
            return Err(format!(
                "prerendered font `{name}` is not a supported font payload"
            ));
        }
        self.named_file_faces
            .insert(name.clone(), ids.into_iter().collect());
        self.prerendered_fonts.insert(name, data);
        self.clear_caches();
        Ok(())
    }

    pub fn unmap_prerendered_font(&mut self, name: &str) -> bool {
        let removed = self.prerendered_fonts.remove(name).is_some();
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
            let Some(image) = self.render_glyph(
                glyph.face,
                spec,
                glyph.glyph_id,
                glyph.pen_x + glyph.x,
                glyph.line_y - glyph.y,
            ) else {
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
                let mut src_index = (row * image.width + col_start) as usize;
                for _ in col_start..col_end {
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
                    src_index += 1;
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
                let mut src_index = ((row * image.width + col_start) * 4) as usize;
                for _ in col_start..col_end {
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
                    src_index += 4;
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
                let mut src_index = ((row * image.width + col_start) * 4) as usize;
                for _ in col_start..col_end {
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
                    src_index += 4;
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
}
