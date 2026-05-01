use std::{collections::BTreeMap, sync::Arc};

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

#[derive(Clone, Debug)]
pub struct FontSystem {
    db: Database,
    named_file_faces: BTreeMap<String, Vec<fontdb::ID>>,
    prerendered_fonts: BTreeMap<String, Arc<[u8]>>,
}

impl Default for FontSystem {
    fn default() -> Self {
        Self::new()
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
        Ok(())
    }

    pub fn map_prerendered_font(
        &mut self,
        name: impl Into<String>,
        data: Vec<u8>,
    ) -> Result<(), String> {
        let name = name.into();
        let ids = self
            .db
            .load_font_source(Source::Binary(Arc::new(data.clone())));
        if ids.is_empty() {
            return Err(format!(
                "prerendered font `{name}` is not a supported font payload"
            ));
        }
        self.named_file_faces
            .insert(name.clone(), ids.into_iter().collect());
        self.prerendered_fonts.insert(name, Arc::from(data));
        Ok(())
    }

    pub fn unmap_prerendered_font(&mut self, name: &str) -> bool {
        self.prerendered_fonts.remove(name).is_some()
    }

    pub fn text_metrics(&self, spec: &FontSpec, text: &str) -> TextMetrics {
        let layout = self.layout_text(spec, text);
        layout.metrics
    }

    pub fn esc_width(&self, spec: &FontSpec, text: &str) -> (f32, f32) {
        rotate_vector(self.text_metrics(spec, text).width, 0.0, spec.angle)
    }

    pub fn esc_height(&self, spec: &FontSpec, text: &str) -> (f32, f32) {
        rotate_vector(0.0, self.text_metrics(spec, text).height, spec.angle)
    }

    pub fn glyph_draw_rect(&self, spec: &FontSpec, ch: char) -> Option<GlyphDrawRect> {
        let face = self.select_face(spec, Some(ch))?;
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
        let width = layout.metrics.width.ceil().max(1.0) as u32;
        let height = layout.metrics.height.ceil().max(1.0) as u32;
        let mut rgba = vec![0; width as usize * height as usize * 4];
        if let Some(shadow) = style.shadow {
            self.draw_layout(
                spec,
                &layout,
                &mut rgba,
                width,
                height,
                shadow.offset_x,
                shadow.offset_y,
                shadow.color,
            );
        }
        self.draw_layout(spec, &layout, &mut rgba, width, height, 0, 0, style.color);
        RgbaTextImage {
            width,
            height,
            rgba,
            metrics: layout.metrics,
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
        if let Some(shadow) = style.shadow {
            self.draw_layout(
                spec,
                &layout,
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
            &layout,
            dest,
            dest_width,
            dest_height,
            x,
            y,
            style.color,
        );
    }

    fn layout_text(&self, spec: &FontSpec, text: &str) -> TextLayout {
        let face = self.select_face(spec, None);
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
            let ch_face = self.select_face(spec, Some(ch)).unwrap_or(primary_face);
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

    fn render_glyph(
        &self,
        face: fontdb::ID,
        spec: &FontSpec,
        glyph_id: u16,
        x: f32,
        y: f32,
    ) -> Option<GlyphImage> {
        let mut rendered = None;
        let size = spec.resolved_height();
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
                .offset(Vector::new(x.fract(), y.fract()))
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
            rendered = Some(GlyphImage {
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
            });
            Some(())
        })?;
        rendered
    }

    fn face_metrics(&self, face: fontdb::ID, size: f32) -> swash::Metrics {
        let mut metrics = None;
        self.with_font(face, |font| {
            let mut context = ShapeContext::new();
            let shaper = context.builder(font).size(size).build();
            metrics = Some(shaper.metrics());
            Some(())
        });
        metrics.unwrap_or_else(|| swash::Metrics {
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
        })
    }

    fn select_face(&self, spec: &FontSpec, ch: Option<char>) -> Option<fontdb::ID> {
        if spec.face_is_file_name
            && let Some(ids) = self.named_file_faces.get(&spec.face)
            && let Some(id) = ids
                .iter()
                .copied()
                .find(|id| ch.is_none_or(|ch| self.face_supports(*id, ch)))
                .or_else(|| ids.first().copied())
        {
            return Some(id);
        }

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

        if let Some(ch) = ch {
            return self
                .db
                .faces()
                .map(|face| face.id)
                .find(|id| self.face_supports(*id, ch));
        }

        self.db.faces().next().map(|face| face.id)
    }

    fn face_supports(&self, face: fontdb::ID, ch: char) -> bool {
        self.glyph_id(face, ch).is_some()
    }

    fn glyph_id(&self, face: fontdb::ID, ch: char) -> Option<u16> {
        let mut glyph_id = None;
        self.with_font(face, |font| {
            let mapped = font.charmap().map(ch);
            if mapped != 0 {
                glyph_id = Some(mapped);
            }
            Some(())
        });
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct RgbaTextImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub metrics: TextMetrics,
}

#[derive(Clone, Debug)]
struct TextLayout {
    glyphs: Vec<PositionedGlyph>,
    metrics: TextMetrics,
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

fn blit_glyph(
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    x: i32,
    y: i32,
    color: [u8; 4],
    image: &GlyphImage,
) {
    if image.width == 0 || image.height == 0 {
        return;
    }
    for row in 0..image.height as i32 {
        let dest_y = y + row;
        if dest_y < 0 || dest_y >= dest_height as i32 {
            continue;
        }
        for col in 0..image.width as i32 {
            let dest_x = x + col;
            if dest_x < 0 || dest_x >= dest_width as i32 {
                continue;
            }
            let src = glyph_pixel(image, col as u32, row as u32, color);
            if src[3] == 0 {
                continue;
            }
            let index = ((dest_y as u32 * dest_width + dest_x as u32) * 4) as usize;
            blend_pixel(&mut dest[index..index + 4], src);
        }
    }
}

fn glyph_pixel(image: &GlyphImage, x: u32, y: u32, color: [u8; 4]) -> [u8; 4] {
    let index = (y * image.width + x) as usize;
    match image.content {
        GlyphContent::Alpha => {
            let alpha = image.data.get(index).copied().unwrap_or(0);
            [color[0], color[1], color[2], multiply_u8(alpha, color[3])]
        }
        GlyphContent::Subpixel => {
            let index = index * 4;
            let r = image.data.get(index).copied().unwrap_or(0);
            let g = image.data.get(index + 1).copied().unwrap_or(0);
            let b = image.data.get(index + 2).copied().unwrap_or(0);
            let alpha = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
            [color[0], color[1], color[2], multiply_u8(alpha, color[3])]
        }
        GlyphContent::Color => {
            let index = index * 4;
            [
                image.data.get(index).copied().unwrap_or(0),
                image.data.get(index + 1).copied().unwrap_or(0),
                image.data.get(index + 2).copied().unwrap_or(0),
                multiply_u8(image.data.get(index + 3).copied().unwrap_or(0), color[3]),
            ]
        }
    }
}

fn multiply_u8(a: u8, b: u8) -> u8 {
    ((u16::from(a) * u16::from(b) + 127) / 255) as u8
}

fn blend_pixel(dest: &mut [u8], src: [u8; 4]) {
    let src_a = src[3] as f32 / 255.0;
    let dest_a = dest[3] as f32 / 255.0;
    let out_a = src_a + dest_a * (1.0 - src_a);
    if out_a <= f32::EPSILON {
        dest.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    for channel in 0..3 {
        let src_c = src[channel] as f32 / 255.0;
        let dest_c = dest[channel] as f32 / 255.0;
        let out_c = (src_c * src_a + dest_c * dest_a * (1.0 - src_a)) / out_a;
        dest[channel] = (out_c * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dest[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
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
