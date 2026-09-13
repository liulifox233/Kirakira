use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

use fontdb::{Database, Family, Query, Source, Stretch, Style as FontStyle, Weight};
pub use krkr_core::{FontSpec, ShadowStyle, TextStyle};
use swash::{
    FontRef, StringId,
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
        // TVP pre-rendered fonts store scanlines bottom-to-top. The native
        // loader writes decoded rows into the destination from the last row
        // backwards, so reverse rows before rendering.
        let row_width = usize::from(glyph.width);
        let height = usize::from(glyph.height);
        for row in 0..height / 2 {
            let opposite = height - 1 - row;
            let (head, tail) = bitmap.split_at_mut(opposite * row_width);
            let first = &mut head[row * row_width..(row + 1) * row_width];
            let second = &mut tail[..row_width];
            first.swap_with_slice(second);
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

/// One entry of a game's `embfontlist.tjs`: the name scenarios ask for, the
/// storage the font file lives in, and the face name the table declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedFontEntry {
    pub name: String,
    pub file: String,
    pub face: String,
}

/// Parses the `(const)%[ "name"=>..., "file"=>..., "face"=>... ]` dictionaries
/// that KAG games ship as `embfontlist.tjs`. The table is TJS source rather
/// than data, so the parser only collects quoted `key => "value"` pairs out of
/// every `%[ ... ]` dictionary and keeps the ones that name a font file.
pub fn parse_embedded_font_list(text: &str) -> Vec<EmbeddedFontEntry> {
    let mut entries = Vec::new();
    for block in tjs_dictionary_blocks(text) {
        let fields = tjs_string_pairs(block);
        let Some(file) = string_pair(&fields, "file") else {
            continue;
        };
        let Some(name) = string_pair(&fields, "name") else {
            continue;
        };
        entries.push(EmbeddedFontEntry {
            name,
            file,
            face: string_pair(&fields, "face").unwrap_or_default(),
        });
    }
    entries
}

/// Parses the plain `"alias" => "name"` pairs of `deffontmap.tjs`; nested
/// dictionaries (the language tables) contribute their own string pairs, which
/// is exactly the alias set the table defines.
pub fn parse_font_alias_pairs(text: &str) -> Vec<(String, String)> {
    tjs_string_pairs(text)
        .into_iter()
        .filter(|(key, value)| !key.is_empty() && !value.is_empty())
        .collect()
}

fn string_pair(pairs: &[(String, String)], key: &str) -> Option<String> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

fn tjs_dictionary_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find("%[") {
        let start = cursor + offset + 1;
        let mut depth = 0usize;
        let mut index = start;
        for (position, ch) in text[start..].char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        index = start + position;
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth != 0 {
            break;
        }
        blocks.push(&text[start..index]);
        cursor = index + 1;
    }
    blocks
}

fn tjs_string_pairs(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let Some(offset) = text[cursor..].find('"') else {
            break;
        };
        let key_start = cursor + offset;
        let Some((key, after_key)) = tjs_quoted_string(text, key_start) else {
            cursor = key_start + 1;
            continue;
        };
        let rest = &text[after_key..];
        let mut arrow = rest.trim_start();
        if let Some(stripped) = arrow.strip_prefix('=') {
            arrow = stripped.trim_start();
            if let Some(stripped) = arrow.strip_prefix('>') {
                let value_offset = after_key + (rest.len() - stripped.len());
                let value_start = text[value_offset..]
                    .find('"')
                    .map(|offset| value_offset + offset);
                let Some(value_start) = value_start else {
                    break;
                };
                if let Some((value, after_value)) = tjs_quoted_string(text, value_start) {
                    pairs.push((key, value));
                    cursor = after_value;
                    continue;
                }
            }
        }
        cursor = after_key;
    }
    pairs
}

fn tjs_quoted_string(text: &str, start: usize) -> Option<(String, usize)> {
    let mut chars = text[start..].char_indices();
    if chars.next().map(|(_, ch)| ch) != Some('"') {
        return None;
    }
    let mut value = String::new();
    let mut escaped = false;
    for (position, ch) in chars {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some((value, start + position + ch.len_utf8())),
            _ => value.push(ch),
        }
    }
    None
}

/// Lowercased lookup key for a font name; the reference resolves names through
/// the platform font table, which is case-insensitive.
fn face_name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

const REGION_TOKENS: [&str; 6] = ["jp", "sc", "tc", "kr", "cn", "hk"];
const REGION_LETTERS: [&str; 2] = ["j", "k"];

/// A name with its regional-variant token removed (`Source Han Sans SC Bold`
/// and `Source Han Sans JP Bold` both reduce to their region-free form), so a
/// game table can name a face the shipped font does not carry under that
/// regional spelling. `None` when the name carries no region token.
fn region_free_key(name: &str) -> Option<String> {
    let key = face_name_key(name).replace(['-', '_'], " ");
    let mut tokens = Vec::new();
    let mut removed = false;
    for token in key.split_whitespace() {
        if REGION_TOKENS.contains(&token) || REGION_LETTERS.contains(&token) {
            removed = true;
            continue;
        }
        // File stems glue the region onto the family ("sourcehansansjp"), so a
        // multi-letter token may also end with one.
        let mut token = token.to_string();
        for region in REGION_TOKENS {
            if token.len() > region.len() && token.ends_with(region) {
                token.truncate(token.len() - region.len());
                removed = true;
                break;
            }
        }
        tokens.push(token);
    }
    if removed && !tokens.is_empty() {
        Some(tokens.join(" "))
    } else {
        None
    }
}

/// Basename of a file with its extension dropped and lowercased. No region
/// handling, so two regional variants of one family stay distinct.
fn stem_key(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = base.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(base);
    stem.trim().to_lowercase()
}

/// Key for matching a file cited by a game table against a loaded storage when
/// the named file is not shipped: the directory and extension are dropped and
/// the region token ignored, so the shipped regional variant of the same family
/// answers for it.
fn file_key(name: &str) -> String {
    let stem = stem_key(name);
    region_free_key(&stem).unwrap_or(stem)
}

#[derive(Debug)]
pub struct FontSystem {
    db: Database,
    named_file_faces: BTreeMap<String, Vec<fontdb::ID>>,
    /// Every face the game registered through `System.addFont`
    /// (`load_font_data`), in registration order — the default being font a
    /// spec with no resolvable face measures through (see
    /// `query_default_faces`).
    registered_faces: Vec<fontdb::ID>,
    embedded_fonts: Vec<EmbeddedFontEntry>,
    font_aliases: Vec<(String, String)>,
    loaded_face_names: BTreeMap<String, fontdb::ID>,
    prerendered_fonts: BTreeMap<PrerenderedFontKey, PrerenderedFont>,
    /// The being font each spec resolves to: a requested candidate when one
    /// answers, else the default being font (`query_default_faces`). One entry
    /// per spec — the reference resolves `GetBeingFont` once per font spec
    /// (`FreeTypeFontRasterizer.cpp:43-90`), so no measurement may depend on
    /// the text a spec carries.
    primary_faces: RefCell<BTreeMap<FaceSelectionKey, Option<fontdb::ID>>>,
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
            registered_faces: self.registered_faces.clone(),
            embedded_fonts: self.embedded_fonts.clone(),
            font_aliases: self.font_aliases.clone(),
            loaded_face_names: self.loaded_face_names.clone(),
            prerendered_fonts: self.prerendered_fonts.clone(),
            primary_faces: RefCell::new(BTreeMap::new()),
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
            registered_faces: Vec::new(),
            embedded_fonts: Vec::new(),
            font_aliases: Vec::new(),
            loaded_face_names: BTreeMap::new(),
            prerendered_fonts: BTreeMap::new(),
            primary_faces: RefCell::new(BTreeMap::new()),
            glyph_ids: RefCell::new(BTreeMap::new()),
            face_metrics: RefCell::new(BTreeMap::new()),
            glyph_images: RefCell::new(BTreeMap::new()),
            prerendered_glyph_images: RefCell::new(BTreeMap::new()),
        }
    }

    pub fn families(&self) -> Vec<String> {
        let mut names = Vec::new();
        for face in self.db.faces() {
            names.extend(face.families.iter().map(|(name, _)| name.clone()));
            let mut table_names = Vec::new();
            self.with_font(face.id, |font| {
                collect_face_names(font, &mut table_names, false);
                Some(())
            });
            names.extend(table_names);
        }
        names.sort();
        names.dedup();
        names
    }

    /// Registers the embedded-font table a game ships (`embfontlist.tjs`), so
    /// the names scenarios ask for resolve to the fonts `System.addFont`
    /// actually loaded.
    pub fn register_embedded_font_list(&mut self, entries: Vec<EmbeddedFontEntry>) {
        // A build can carry the list twice (patch and data archives) with the
        // same names but a different face spelling for an entry — GINKA's JP
        // font is `Source Han Sans JP Bold` in one and `源ノ角ゴシック JP Bold`
        // in the other — so keep both spellings and drop only exact duplicates.
        // The first table read still wins, like the archive priority the game
        // loader sees.
        for entry in entries {
            let known = self.embedded_fonts.iter().any(|existing| {
                existing.name == entry.name
                    && existing.file == entry.file
                    && existing.face == entry.face
            });
            if !known {
                self.embedded_fonts.push(entry);
            }
        }
        self.clear_caches();
    }

    /// Registers the `"alias" => "name"` pairs of a game's `deffontmap.tjs`.
    pub fn register_font_aliases(&mut self, aliases: Vec<(String, String)>) {
        for (alias, target) in aliases {
            if !self.font_aliases.iter().any(|(name, _)| *name == alias) {
                self.font_aliases.push((alias, target));
            }
        }
        self.clear_caches();
    }

    /// `register_embedded_font_list` on the table's source text.
    pub fn register_embedded_font_list_text(&mut self, text: &str) {
        self.register_embedded_font_list(parse_embedded_font_list(text));
    }

    /// `register_font_aliases` on the table's source text.
    pub fn register_font_alias_text(&mut self, text: &str) {
        self.register_font_aliases(parse_font_alias_pairs(text));
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
        let ids = ids.into_iter().collect::<Vec<_>>();
        for id in &ids {
            self.index_face_names(*id);
        }
        self.registered_faces.extend(ids.iter().copied());
        self.named_file_faces.insert(name, ids);
        self.clear_caches();
        Ok(())
    }

    /// Records every name the face's name table carries, so a request for a
    /// legacy family name (`Source Han Sans SC Bold`), a localized name
    /// (`思源黑体`) or a PostScript name can find the face even though
    /// `fontdb` only indexes one family per face.
    fn index_face_names(&mut self, face: fontdb::ID) {
        let mut names = Vec::new();
        self.with_font(face, |font| {
            collect_face_names(font, &mut names, true);
            Some(())
        });
        for name in names {
            self.loaded_face_names
                .entry(face_name_key(&name))
                .or_insert(face);
            if let Some(key) = region_free_key(&name) {
                self.loaded_face_names.entry(key).or_insert(face);
            }
        }
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
        let face = primary_face;
        let glyph_id = self
            .glyph_id(face, self.char_for_face(face, ch))
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

        // One face serves the whole spec, the way `FreeTypeFontRasterizer`
        // resolves a being font once per font spec and never per character
        // (`FreeTypeFontRasterizer.cpp:43-90`) — including the default being
        // font `select_primary_face` picked when nothing resolved. A character
        // the face has no glyph for draws the face's own default character
        // (:107-129) instead of a glyph borrowed from another face, so one odd
        // character cannot re-route the run.
        let flush_run =
            |run: &mut String, pen_x: &mut f32, pen_y: f32, glyphs: &mut Vec<PositionedGlyph>| {
                if run.is_empty() {
                    return;
                }
                self.shape_run(spec, primary_face, run, *pen_x, pen_y, glyphs);
                *pen_x = glyphs
                    .last()
                    .map(|glyph| glyph.pen_x + glyph.advance)
                    .unwrap_or(*pen_x);
                run.clear();
            };

        for ch in text.chars() {
            if ch == '\n' {
                flush_run(&mut run, &mut pen_x, pen_y, &mut glyphs);
                max_width = max_width.max(pen_x);
                pen_x = 0.0;
                pen_y += line_height;
                lines = lines.saturating_add(1);
                continue;
            }
            let ch = self.char_for_face(primary_face, ch);
            run.push(ch);
        }
        flush_run(&mut run, &mut pen_x, pen_y, &mut glyphs);
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

            let face = primary_face;
            let start = glyphs.len();
            let fallback = self.char_for_face(face, ch);
            self.shape_run(spec, face, &fallback.to_string(), pen_x, pen_y, &mut glyphs);
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

    /// The being font of a spec: a requested candidate that resolves, else the
    /// default being font. One face serves the whole spec — every text of it —
    /// the way `FreeTypeFontRasterizer` resolves a being font once per font
    /// spec and never per character (`FreeTypeFontRasterizer.cpp:43-90`).
    fn select_primary_face(&self, spec: &FontSpec) -> Option<fontdb::ID> {
        if spec.face_is_file_name {
            return self.select_named_file_face(spec);
        }

        let key = FaceSelectionKey::new(spec);
        if let Some(cached) = self.primary_faces.borrow().get(&key).copied() {
            return cached;
        }
        let selected = self
            .query_requested_faces(spec)
            .or_else(|| self.query_default_faces(spec))
            .or_else(|| self.query_fallback_faces(spec))
            .or_else(|| self.db.faces().next().map(|face| face.id));
        self.primary_faces.borrow_mut().insert(key, selected);
        selected
    }

    /// The default being font: what `GetBeingFont` answers when no requested
    /// candidate resolves (`FontSystem.cpp:92-96` returns
    /// `TVPGetDefaultFontName()`, `ＭＳ Ｐゴシック` on a Japanese build and
    /// `微软雅黑` on a Chinese one — a system font that can draw the game's
    /// text, `TVPSysFont.cpp:16-54`; the platform's font mapper even
    /// substitutes a name no font carries, `NativeFreeTypeFace.cpp:47-85`).
    /// A bare `fontdb` generic family is not that font: it usually carries no
    /// CJK coverage, so a spec with an empty or unknown face measured CJK
    /// through the fallback face's missing-glyph advance (0.8 em) while the
    /// game drew real glyphs 1 em wide — the 少女世界 message font did exactly
    /// that, and 纸上的魔法使's `华文细黑` message font resolves to nothing at
    /// all. So the default is the loaded face that can draw CJK, preferring,
    /// in the reference's order, the game's own `System.addFont` faces
    /// (`registered_faces`, the closest thing to the platform list), then the
    /// generic families, then any loaded face; within a set the best probe
    /// coverage wins and a sans-shaped family breaks ties (`best_probe_face`,
    /// `being_font_rank`), the way the reference's default is a sans UI face.
    /// One face is picked per spec, like `GetBeingFont`, so no advance depends
    /// on the rest of the string; a character the chosen face still lacks
    /// draws its own default character
    /// (`FreeTypeFontRasterizer.cpp:119-127`) — one odd character must not
    /// re-route the whole run. Only when no loaded face can draw CJK at all
    /// does the choice fall back to the game's default face and the generic
    /// families, which then draw their own missing-glyph character, exactly as
    /// the reference does when even its default font lacks a glyph.
    fn query_default_faces(&self, spec: &FontSpec) -> Option<fontdb::ID> {
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
        let matches_style = |id: fontdb::ID| {
            self.db
                .face(id)
                .is_some_and(|face| face.weight == weight && face.style == style)
        };

        self.best_probe_face(
            self.registered_faces
                .iter()
                .copied()
                .filter(|id| matches_style(*id)),
        )
        .or_else(|| self.best_probe_face(self.registered_faces.iter().copied()))
        .or_else(|| self.best_probe_face(self.generic_faces(spec)))
        .or_else(|| self.best_probe_face(self.db.faces().map(|face| face.id)))
        .or_else(|| {
            self.registered_faces
                .iter()
                .copied()
                .find(|id| matches_style(*id))
        })
        .or_else(|| self.registered_faces.first().copied())
    }

    /// The best face of a candidate set to become the default being font: the
    /// highest probe coverage, a UI-shaped family over a serif/mono/unlabeled
    /// one, the earliest candidate over a later one. Early exits when the
    /// first candidate is already a full-coverage UI face, so the common case
    /// does not score every loaded face.
    fn best_probe_face<I: Iterator<Item = fontdb::ID>>(&self, candidates: I) -> Option<fontdb::ID> {
        let mut best: Option<(usize, u8, fontdb::ID)> = None;
        for face in candidates {
            let score = self.probe_coverage(face);
            if score == 0 {
                continue;
            }
            let rank = self.being_font_rank(face);
            if score == DEFAULT_BEING_FONT_PROBE.len() && rank == 0 {
                return Some(face);
            }
            // Lower rank wins; `u8::MAX - rank` turns it into a larger-is-
            // better component beside the score.
            if best.is_none_or(|(best_score, best_rank, _)| {
                (score, u8::MAX - rank) > (best_score, u8::MAX - best_rank)
            }) {
                best = Some((score, rank, face));
            }
        }
        best.map(|(_, _, face)| face)
    }

    /// How many characters of the default probe a face carries.
    fn probe_coverage(&self, face: fontdb::ID) -> usize {
        DEFAULT_BEING_FONT_PROBE
            .into_iter()
            .filter(|ch| self.face_supports(face, *ch))
            .count()
    }

    /// How well a face's family names read as the reference's default being
    /// font: the platform default is a sans UI family (`ＭＳ Ｐゴシック` on a
    /// Japanese build, `微软雅黑` on a Chinese one), so a sans-shaped family
    /// is preferred over a serif, a mono-spaced or an unlabeled one. Without
    /// this a bare probe scan stops on a bitmap fallback font such as Unifont,
    /// which covers everything but is not the platform's UI face. Faces of
    /// every rank are still usable — the rank only breaks ties between faces
    /// that cover the same amount of the probe.
    fn being_font_rank(&self, face: fontdb::ID) -> u8 {
        let Some(info) = self.db.face(face) else {
            return 3;
        };
        let mut rank = 3;
        for (name, _) in &info.families {
            let name = name.to_lowercase();
            let candidate = if name.contains("mono") {
                2
            } else if name.contains("sans")
                || name.contains("gothic")
                || name.contains("hei")
                || name.contains('黑')
            {
                0
            } else if name.contains("serif")
                || name.contains("mincho")
                || name.contains("明朝")
                || name.contains("song")
                || name.contains('宋')
            {
                1
            } else {
                3
            };
            rank = rank.min(candidate);
        }
        rank
    }

    fn select_named_file_face(&self, spec: &FontSpec) -> Option<fontdb::ID> {
        self.named_file_faces
            .get(&spec.face)
            .and_then(|ids| ids.first().copied())
    }

    fn query_requested_faces(&self, spec: &FontSpec) -> Option<fontdb::ID> {
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
            if let Some(id) = self.resolve_face_name(face_name, weight, style, 0) {
                return Some(id);
            }
        }

        None
    }

    /// Resolves one candidate name of a font spec the way `GetBeingFont`
    /// (`FontSystem.cpp:53-97`) picks the being face — a family query against
    /// the loaded face set — extended with the names a game's own font tables
    /// use for the files `System.addFont` registered.
    fn resolve_face_name(
        &self,
        name: &str,
        weight: Weight,
        style: FontStyle,
        depth: usize,
    ) -> Option<fontdb::ID> {
        if depth > MAX_FONT_ALIAS_DEPTH {
            return None;
        }
        let name = name.trim();
        if name.is_empty() {
            return None;
        }

        let families = [Family::Name(name)];
        let query = Query {
            families: &families,
            weight,
            stretch: Stretch::Normal,
            style,
        };
        if let Some(id) = self.db.query(&query) {
            return Some(id);
        }

        // `fontdb` indexes one family per face; these come from the font's own
        // name table (family, typographic family, full and PostScript names in
        // every language), which is what the game tables quote.
        if let Some(id) = self.loaded_face_names.get(&face_name_key(name)).copied() {
            return Some(id);
        }
        if let Some(key) = region_free_key(name)
            && let Some(id) = self.loaded_face_names.get(&key).copied()
        {
            return Some(id);
        }

        let key = face_name_key(name);
        for entry in &self.embedded_fonts {
            // An entry answers for the name the scenarios ask for and for the
            // face spelling its table declares. A build's two archives can
            // disagree on that spelling (GINKA: `源ノ角ゴシック JP Bold` in
            // data.xp3, `Source Han Sans JP Bold` in patch.xp3), and the
            // region-free comparison lets either spelling reach the entry.
            let region_match = match (region_free_key(&entry.face), region_free_key(name)) {
                (Some(entry_key), Some(name_key)) => entry_key == name_key,
                _ => false,
            };
            if face_name_key(&entry.name) != key
                && face_name_key(&entry.face) != key
                && !region_match
            {
                continue;
            }
            if let Some(id) = self.resolve_embedded_entry(entry) {
                return Some(id);
            }
        }

        for (alias, target) in &self.font_aliases {
            if alias == name
                && let Some(id) = self.resolve_face_name(target, weight, style, depth + 1)
            {
                return Some(id);
            }
        }

        None
    }

    /// The face an embedded-font entry names. The entry's file is the storage
    /// `System.addFont` receives, and when the build ships that file it wins;
    /// the region-free key only takes over when it does not ship it, because
    /// then the same typeface in the region the build does ship is the table's
    /// counterpart (GINKA's `源ノ角ゴシックB` → `SourceHanSansJP-Bold.otf` →
    /// the shipped `SourceHanSansSC-Bold.otf`).
    fn resolve_embedded_entry(&self, entry: &EmbeddedFontEntry) -> Option<fontdb::ID> {
        if let Some(id) = self.face_for_entry_file(&entry.file, true) {
            return Some(id);
        }
        if let Some(id) = self.face_for_entry_file(&entry.file, false) {
            return Some(id);
        }

        if !entry.face.is_empty() {
            if let Some(id) = self
                .loaded_face_names
                .get(&face_name_key(&entry.face))
                .copied()
            {
                return Some(id);
            }
            if let Some(key) = region_free_key(&entry.face)
                && let Some(id) = self.loaded_face_names.get(&key).copied()
            {
                return Some(id);
            }
        }

        None
    }

    /// The loaded face whose storage file an entry names: `exact_stem` matches
    /// the file name itself, otherwise the regional-variant token is ignored.
    fn face_for_entry_file(&self, file: &str, exact_stem: bool) -> Option<fontdb::ID> {
        let key_for = |name: &str| {
            if exact_stem {
                stem_key(name)
            } else {
                file_key(name)
            }
        };
        let wanted = key_for(file);
        for (storage, ids) in &self.named_file_faces {
            if key_for(storage) != wanted {
                continue;
            }
            if let Some(id) = ids.first().copied() {
                return Some(id);
            }
        }
        None
    }

    fn query_fallback_faces(&self, spec: &FontSpec) -> Option<fontdb::ID> {
        self.generic_faces(spec).next()
    }

    /// The `fontdb` generic-family faces that answer the spec's weight and
    /// style, in family order.
    fn generic_faces(&self, spec: &FontSpec) -> impl Iterator<Item = fontdb::ID> + '_ {
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

        [Family::SansSerif, Family::Serif, Family::Monospace]
            .into_iter()
            .filter_map(move |family| {
                let families = [family];
                let query = Query {
                    families: &families,
                    weight,
                    stretch: Stretch::Normal,
                    style,
                };
                self.db.query(&query)
            })
    }

    fn face_supports(&self, face: fontdb::ID, ch: char) -> bool {
        self.glyph_id(face, ch).is_some()
    }

    /// The character a run draws for `ch` in its face. `FreeTypeFontRasterizer`
    /// never borrows a glyph from another face: a character the being face
    /// lacks draws that face's default character and advances by it
    /// (`FreeTypeFontRasterizer.cpp:107-129`). Named faces resolve through the
    /// platform face class there, whose default character is the font's
    /// `tmDefaultChar`/`tmBreakChar` (`NativeFreeTypeFace.cpp:250-257`), so a
    /// visible replacement glyph comes first and the generic face's space
    /// (`FreeType.cpp:76`) is the last resort.
    fn char_for_face(&self, face: fontdb::ID, ch: char) -> char {
        if self.face_supports(face, ch) {
            return ch;
        }
        self.default_char(face).unwrap_or(ch)
    }

    fn default_char(&self, face: fontdb::ID) -> Option<char> {
        ['\u{25a1}', '\u{fffd}', '?', ' ']
            .into_iter()
            .find(|ch| self.face_supports(face, *ch))
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
        self.glyph_ids.borrow_mut().clear();
        self.face_metrics.borrow_mut().clear();
        self.glyph_images.borrow_mut().clear();
        self.prerendered_glyph_images.borrow_mut().clear();
    }
}

/// What the default being font has to be able to draw. The reference's default
/// is the system's CJK-capable UI font — `ＭＳ Ｐゴシック` on a Japanese build,
/// `微软雅黑` on a Chinese one (`MsgLoad.cpp`'s `TVPDefaultFontName`) — so the
/// default here is the loaded face that carries the scripts the games write:
/// a simplified-Chinese ideograph (忆, the first character of 纸上的魔法使's
/// first message), kana (あ), a kanji (漢, traditional in shape) and the CJK
/// full stop (。). A face covering all four is what the reference's default
/// gives; a face covering part of it still beats a Latin-only fallback, and a
/// character outside even that draws the face's own default glyph.
const DEFAULT_BEING_FONT_PROBE: [char; 4] = ['忆', 'あ', '漢', '。'];

const MAX_FONT_ALIAS_DEPTH: usize = 8;

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

fn collect_face_names(font: FontRef<'_>, names: &mut Vec<String>, include_full: bool) {
    for entry in font.localized_strings() {
        let wanted = match entry.id() {
            StringId::Family | StringId::TypographicFamily => true,
            StringId::Full | StringId::PostScript => include_full,
            _ => false,
        };
        if !wanted || !entry.is_decodable() {
            continue;
        }
        let name = entry.chars().collect::<String>();
        if !name.is_empty() {
            names.push(name);
        }
    }
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
        assert_eq!(font.decode_glyph(glyph).unwrap(), [255, 255, 0, 255]);
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

    /// GINKA's own `data.xp3 > font/embfontlist.tjs`, verbatim apart from the
    /// trimming of the groups the tests do not use. This is the table the
    /// reviewer's scratch probe and the live runs see, so the JP entries carry
    /// the Japanese face spelling (`源ノ角ゴシック JP Bold`).
    const GINKA_EMBEDDED_FONT_LIST: &str = r#"
(const)[
	// 小杉ゴシックフォント（＝旧モトヤフォント）
	(const)%[ "license" => "Kosugi-NOTICE.txt", "detail" => "Kosugi-LICENSE.txt", "ignorelang"=>"cn,tw" ], (const)[
		(const)%[ "name"=>"小杉ゴシック",   "file"=>"Kosugi-Regular.ttf",     "face"=>"MotoyaLCedar", "capname"=>"Kosugi"   ],
		(const)%[ "name"=>"小杉丸ゴシック", "file"=>"KosugiMaru-Regular.ttf", "face"=>"MotoyaLMaru",  "capname"=>"Kosugi Maru" ]
		],

	// 源ノ｛角ゴシック／明朝｝
	(const)%[ "license" => "SourceHanSansAndSerif-NOTICE.txt", "detail" => "SourceHanSansAndSerif-LICENSE.txt", "ignorelang"=>"cn,tw" ], (const)[
		(const)%[ "name"=>"源ノ角ゴシックR", "file"=>"SourceHanSansJP-Regular.otf", "face"=>"源ノ角ゴシック JP Regular" ],
		(const)%[ "name"=>"源ノ角ゴシックB", "file"=>"SourceHanSansJP-Bold.otf",    "face"=>"源ノ角ゴシック JP Bold"    ],
		(const)%[ "name"=>"源ノ角ゴシックH", "file"=>"SourceHanSansJP-Heavy.otf",   "face"=>"源ノ角ゴシック JP Heavy"   ]
		],

	// SourceHanSans/Serif (Simplified_Chinese)
	(const)%[ "license" => "SourceHanSansAndSerif-NOTICE.txt", "detail" => "SourceHanSansAndSerif-LICENSE.txt", "ignorelang"=>"jp,en,tw" ], (const)[
		(const)%[ "name"=>"思源黑体R", "file"=>"SourceHanSansSC-Regular.otf", "face"=>"Source Han Sans SC Regular" ],
		(const)%[ "name"=>"思源黑体B", "file"=>"SourceHanSansSC-Bold.otf",    "face"=>"Source Han Sans SC Bold"    ],
		(const)%[ "name"=>"思源黑体H", "file"=>"SourceHanSansSC-Heavy.otf",   "face"=>"Source Han Sans SC Heavy"   ]
		],

	// Nunito Sans (For English only)
	(const)%[ "license" => "NunitoSans-NOTICE.txt", "detail" => "NunitoSans-LICENSE.txt", "uselang"=>"en" ], (const)[
		(const)%[ "name"=>"NunitoSans-SB",  "file"=>"NunitoSans_10pt-SemiBold.ttf",        "face"=>"Nunito Sans 10pt SemiBold", "substyle" => (const)[ "NunitoSans-EB", "NunitoSans-SBI", "NunitoSans-EBI" ] ],
		(const)%[ "name"=>"NunitoSans-BK",  "file"=>"NunitoSans_10pt-Black.ttf",           "face"=>"Nunito Sans 10pt Black", "bold"=>true,                 "noentry"=>true ]
		]
	]
"#;

    /// The same build's `patch.xp3 > embfontlist.tjs` for the JP/SC groups: it
    /// spells the JP faces in English, so one build registers both spellings of
    /// the same entry (`Source Han Sans JP Bold` and `源ノ角ゴシック JP Bold`).
    const GINKA_PATCH_EMBEDDED_FONT_LIST: &str = r#"
(const)[
	// 源ノ｛角ゴシック／明朝｝
	(const)%[ "license" => "SourceHanSansAndSerif-NOTICE.txt", "detail" => "SourceHanSansAndSerif-LICENSE.txt", "ignorelang"=>"cn,tw" ], (const)[
		(const)%[ "name"=>"源ノ角ゴシックR", "file"=>"SourceHanSansJP-Regular.otf", "face"=>"Source Han Sans JP Regular" ],
		(const)%[ "name"=>"源ノ角ゴシックB", "file"=>"SourceHanSansJP-Bold.otf",    "face"=>"Source Han Sans JP Bold"    ],
		(const)%[ "name"=>"源ノ角ゴシックH", "file"=>"SourceHanSansJP-Heavy.otf",   "face"=>"Source Han Sans JP Heavy"   ]
		],

	// SourceHanSans/Serif (Simplified_Chinese)
	(const)%[ "license" => "SourceHanSansAndSerif-NOTICE.txt", "detail" => "SourceHanSansAndSerif-LICENSE.txt", "ignorelang"=>"jp,en,tw" ], (const)[
		(const)%[ "name"=>"思源黑体R", "file"=>"SourceHanSansSC-Regular.otf", "face"=>"Source Han Sans SC Regular" ],
		(const)%[ "name"=>"思源黑体B", "file"=>"SourceHanSansSC-Bold.otf",    "face"=>"Source Han Sans SC Bold"    ],
		(const)%[ "name"=>"思源黑体H", "file"=>"SourceHanSansSC-Heavy.otf",   "face"=>"Source Han Sans SC Heavy"   ]
		]
	]
"#;

    /// GINKA's `patch.xp3 > deffontmap.tjs`, verbatim: the `"*"` language
    /// table, the legacy alias names and the `$記号$` macro alias.
    const GINKA_DEFAULT_FONT_MAP: &str = r#"
%[
	// 共通定義
	"*" => %[
		"lang_*"  => "源ノ角ゴシックB", // failsafe用
		"lang_jp" => "源ノ角ゴシックB",
		"lang_en" => "源ノ角ゴシックB",
		"lang_cn" => "思源黑体B",
		"lang_tw" => "思源黑體B",
		],

	// 旧フォント名のエイリアス
	"シーダ"   => "小杉ゴシック",
	"マルベリ" => "小杉丸ゴシック",

	// 日本語固定フォント[▼]マクロ用
	"$記号$" => "源ノ角ゴシックB",

	"SystemDefault"  => %[
	lang:"ui",alias:"SystemFont", // システム用
		"lang_jp" => "源ノ角ゴシックB",
		"lang_en" => "NunitoSans-SB", //"源ノ角ゴシックB",
		"lang_cn" => "思源黑体B",
		"lang_tw" => "思源黑體B",
		"lang_*" => "MS Shell Dlg 2",
		]
	]
"#;

    /// The second game's table maps everything onto `思源黑体中等`, whose entry
    /// points at the renamed `sourcehansansjp-bold.otf` file.
    const SHOUJO_EMBEDDED_FONT_LIST: &str = r#"
(const)[		//思源黑体中等
	(const)%[ "license" => "SourceHanSansAndSerif-NOTICE.txt", "detail" => "SourceHanSansAndSerifJP-LICENSE.txt" ], (const)[
		(const)%[ "name"=>"Noto Sans SC Medium", "file"=>"SourceHanSansJP-Regular.otf", "face"=>"Noto Sans SC Medium" ],
		(const)%[ "name"=>"思源黑体中等", "file"=>"sourcehansansjp-bold.otf",    "face"=>"思源黑体 CN Medium"    ],
		(const)%[ "name"=>"霞鹜文楷", "file"=>"SourceHanSansJP-Heavy.otf",    "face"=>"霞鹜文楷"    ]
		]
	]
"#;

    /// Builds a minimal SFNT face: the tables `fontdb`/`swash` need to index
    /// names and advances (head, hhea, maxp, hmtx, cmap, name) plus a `glyf`
    /// rectangle so that `glyph_draw_rect` has ink to measure. `outline` gives
    /// every mapped glyph the same box, which keeps the fixtures tiny while the
    /// drawn rects still differ between faces.
    fn build_test_font(
        names: &[(u16, u16, &str)],
        glyphs: &[(char, u16)],
        outline: Option<(i16, i16)>,
    ) -> Vec<u8> {
        let num_glyphs = glyphs.len() as u16 + 1;

        let mut head = Vec::new();
        head.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        head.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        head.extend_from_slice(&0u32.to_be_bytes());
        head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        head.extend_from_slice(&0u16.to_be_bytes());
        head.extend_from_slice(&1000u16.to_be_bytes());
        head.extend_from_slice(&0i64.to_be_bytes());
        head.extend_from_slice(&0i64.to_be_bytes());
        for value in [0i16, 0, 1000, 1000] {
            head.extend_from_slice(&value.to_be_bytes());
        }
        head.extend_from_slice(&0u16.to_be_bytes());
        head.extend_from_slice(&8u16.to_be_bytes());
        head.extend_from_slice(&2i16.to_be_bytes());
        head.extend_from_slice(&0i16.to_be_bytes());
        head.extend_from_slice(&0i16.to_be_bytes());
        assert_eq!(head.len(), 54);

        let mut hhea = Vec::new();
        hhea.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        hhea.extend_from_slice(&880i16.to_be_bytes());
        hhea.extend_from_slice(&(-120i16).to_be_bytes());
        hhea.extend_from_slice(&0i16.to_be_bytes());
        hhea.extend_from_slice(
            &glyphs
                .iter()
                .map(|(_, advance)| *advance)
                .max()
                .unwrap_or(0)
                .to_be_bytes(),
        );
        for _ in 0..11 {
            hhea.extend_from_slice(&0i16.to_be_bytes());
        }
        hhea.extend_from_slice(&num_glyphs.to_be_bytes());
        assert_eq!(hhea.len(), 36);

        let mut maxp = Vec::new();
        maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        maxp.extend_from_slice(&num_glyphs.to_be_bytes());

        let mut hmtx = Vec::new();
        hmtx.extend_from_slice(&0u16.to_be_bytes());
        hmtx.extend_from_slice(&0i16.to_be_bytes());
        for (_, advance) in glyphs {
            hmtx.extend_from_slice(&advance.to_be_bytes());
            hmtx.extend_from_slice(&0i16.to_be_bytes());
        }

        let mut ordered = glyphs
            .iter()
            .enumerate()
            .map(|(index, (ch, _))| (*ch, index as u16 + 1))
            .collect::<Vec<_>>();
        ordered.sort_by_key(|(ch, _)| *ch);
        let seg_count = ordered.len() as u16 + 1;
        let seg_count_x2 = seg_count * 2;
        let entry_selector = (seg_count as f32).log2().floor() as u16;
        let search_range = 2 * (1u16 << entry_selector);
        let range_shift = seg_count_x2 - search_range;
        let mut subtable = Vec::new();
        subtable.extend_from_slice(&4u16.to_be_bytes());
        subtable.extend_from_slice(&(14 + 8 * seg_count).to_be_bytes());
        subtable.extend_from_slice(&0u16.to_be_bytes());
        subtable.extend_from_slice(&seg_count_x2.to_be_bytes());
        subtable.extend_from_slice(&search_range.to_be_bytes());
        subtable.extend_from_slice(&entry_selector.to_be_bytes());
        subtable.extend_from_slice(&range_shift.to_be_bytes());
        for (ch, _) in &ordered {
            subtable.extend_from_slice(&(*ch as u16).to_be_bytes());
        }
        subtable.extend_from_slice(&0xFFFFu16.to_be_bytes());
        subtable.extend_from_slice(&0u16.to_be_bytes());
        for (ch, _) in &ordered {
            subtable.extend_from_slice(&(*ch as u16).to_be_bytes());
        }
        subtable.extend_from_slice(&0xFFFFu16.to_be_bytes());
        for (ch, glyph) in &ordered {
            let delta = (*glyph as i32 - *ch as i32) as u16;
            subtable.extend_from_slice(&delta.to_be_bytes());
        }
        subtable.extend_from_slice(&1u16.to_be_bytes());
        for _ in &ordered {
            subtable.extend_from_slice(&0u16.to_be_bytes());
        }
        subtable.extend_from_slice(&0u16.to_be_bytes());
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&1u16.to_be_bytes());
        cmap.extend_from_slice(&3u16.to_be_bytes());
        cmap.extend_from_slice(&1u16.to_be_bytes());
        cmap.extend_from_slice(&12u32.to_be_bytes());
        cmap.extend_from_slice(&subtable);

        let records = names
            .iter()
            .map(|(id, language, value)| {
                let bytes = value
                    .encode_utf16()
                    .flat_map(u16::to_be_bytes)
                    .collect::<Vec<_>>();
                (*id, *language, bytes)
            })
            .collect::<Vec<_>>();
        let mut name = Vec::new();
        name.extend_from_slice(&0u16.to_be_bytes());
        name.extend_from_slice(&(records.len() as u16).to_be_bytes());
        name.extend_from_slice(&(6 + 12 * records.len() as u16).to_be_bytes());
        let mut string_offset = 0u16;
        for (id, language, bytes) in &records {
            name.extend_from_slice(&3u16.to_be_bytes());
            name.extend_from_slice(&1u16.to_be_bytes());
            name.extend_from_slice(&language.to_be_bytes());
            name.extend_from_slice(&id.to_be_bytes());
            name.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            name.extend_from_slice(&string_offset.to_be_bytes());
            string_offset += bytes.len() as u16;
        }
        for (_, _, bytes) in &records {
            name.extend_from_slice(bytes);
        }

        // One square outline per mapped glyph; `.notdef` stays empty. All the
        // faces of a fixture share the box, which is all the rect assertions
        // need (the advance still comes from `hmtx`).
        fn push_glyph(glyf: &mut Vec<u8>, loca: &mut Vec<u16>, outline: Option<(i16, i16)>) {
            loca.push((glyf.len() / 2) as u16);
            let Some((width, height)) = outline else {
                return;
            };
            glyf.extend_from_slice(&1i16.to_be_bytes());
            glyf.extend_from_slice(&0i16.to_be_bytes());
            glyf.extend_from_slice(&0i16.to_be_bytes());
            glyf.extend_from_slice(&width.to_be_bytes());
            glyf.extend_from_slice(&height.to_be_bytes());
            glyf.extend_from_slice(&3u16.to_be_bytes());
            glyf.extend_from_slice(&0u16.to_be_bytes());
            glyf.extend_from_slice(&[0x01, 0x01, 0x01, 0x01]);
            for delta in [0i16, width, 0, -width] {
                glyf.extend_from_slice(&delta.to_be_bytes());
            }
            for delta in [0i16, 0, height, -height] {
                glyf.extend_from_slice(&delta.to_be_bytes());
            }
            while !glyf.len().is_multiple_of(4) {
                glyf.push(0);
            }
        }
        let (mut glyf, mut loca) = (Vec::new(), Vec::new());
        push_glyph(&mut glyf, &mut loca, None);
        for _ in glyphs {
            push_glyph(&mut glyf, &mut loca, outline);
        }
        loca.push((glyf.len() / 2) as u16);

        let mut tables = vec![
            (*b"cmap", cmap),
            (*b"glyf", glyf),
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"hmtx", hmtx),
            (
                *b"loca",
                loca.iter()
                    .flat_map(|offset| offset.to_be_bytes())
                    .collect(),
            ),
            (*b"maxp", maxp),
            (*b"name", name),
        ];
        tables.sort_by_key(|(tag, _)| *tag);
        let num_tables = tables.len() as u16;
        let entry_selector = (num_tables as f32).log2().floor() as u16;
        let search_range = 16 * (1u32 << entry_selector);
        let mut font = Vec::new();
        font.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        font.extend_from_slice(&num_tables.to_be_bytes());
        font.extend_from_slice(&(search_range as u16).to_be_bytes());
        font.extend_from_slice(&entry_selector.to_be_bytes());
        font.extend_from_slice(&((u32::from(num_tables) * 16 - search_range) as u16).to_be_bytes());
        let mut offset = 12 + 16 * tables.len();
        for (tag, data) in &tables {
            font.extend_from_slice(tag);
            font.extend_from_slice(&0u32.to_be_bytes());
            font.extend_from_slice(&(offset as u32).to_be_bytes());
            font.extend_from_slice(&(data.len() as u32).to_be_bytes());
            offset += (data.len() + 3) & !3;
        }
        for (_, data) in &tables {
            font.extend_from_slice(data);
            while !font.len().is_multiple_of(4) {
                font.push(0);
            }
        }
        font
    }

    /// `font/sourcehansanssc-bold.otf`: the face GINKA's repack ships for the
    /// `Source Han Sans SC Bold` entry. Its boxes are full width squares.
    fn sc_bold_test_font() -> Vec<u8> {
        build_test_font(
            &[
                (1, 0x0409, "Source Han Sans SC Bold"),
                (1, 0x0804, "思源黑体 Bold"),
                (16, 0x0409, "Source Han Sans SC"),
                (16, 0x0804, "思源黑体"),
                (4, 0x0409, "Source Han Sans SC Bold"),
                (6, 0x0409, "SourceHanSansSC-Bold"),
            ],
            &[
                ('忆', 1000),
                ('あ', 1000),
                ('い', 1000),
                ('♪', 1000),
                ('□', 1000),
                ('?', 1000),
                (' ', 300),
            ],
            Some((800, 800)),
        )
    }

    /// The JP variant the tables name but GINKA's build does not ship: a
    /// narrower face (あ advances 800 units, i.e. 38.4 px at height 48, the
    /// reviewer's reproduction) with a different box shape.
    fn jp_bold_test_font() -> Vec<u8> {
        build_test_font(
            &[
                (1, 0x0409, "Source Han Sans JP Bold"),
                (1, 0x0411, "源ノ角ゴシック JP Bold"),
                (16, 0x0409, "Source Han Sans JP"),
                (16, 0x0411, "源ノ角ゴシック JP"),
                (4, 0x0409, "Source Han Sans JP Bold"),
                (6, 0x0409, "SourceHanSansJP-Bold"),
            ],
            &[
                ('あ', 800),
                ('い', 800),
                ('♪', 800),
                ('□', 800),
                ('?', 800),
                (' ', 300),
            ],
            Some((800, 400)),
        )
    }

    /// `sourcehansansjp-bold.otf` as the second game ships it: a renamed
    /// `Source Han Sans CN Medium` carrying the localized family name the
    /// table declares.
    fn cn_medium_test_font() -> Vec<u8> {
        build_test_font(
            &[
                (1, 0x0409, "Source Han Sans CN Medium"),
                (1, 0x0804, "思源黑体 CN Medium"),
                (16, 0x0409, "Source Han Sans CN"),
                (16, 0x0804, "思源黑体 CN"),
                (4, 0x0409, "Source Han Sans CN Medium"),
                (6, 0x0409, "SourceHanSansCN-Medium"),
            ],
            &[
                ('あ', 1000),
                ('い', 1000),
                ('♪', 1000),
                ('□', 1000),
                ('?', 1000),
                (' ', 300),
            ],
            Some((800, 800)),
        )
    }

    /// The Latin face the games only use for English: `♪` and `&` are half
    /// width there, and `&` exists in no CJK fixture.
    fn nunito_test_font() -> Vec<u8> {
        build_test_font(
            &[
                (1, 0x0409, "Nunito Sans 10pt SemiBold"),
                (16, 0x0409, "Nunito Sans 10pt"),
                (4, 0x0409, "Nunito Sans 10pt SemiBold"),
                (6, 0x0409, "NunitoSans10pt-SemiBold"),
            ],
            &[
                ('A', 600),
                ('♪', 500),
                ('&', 500),
                ('□', 500),
                ('?', 500),
                (' ', 300),
            ],
            Some((300, 300)),
        )
    }

    fn spec(face: &str, height: f32) -> FontSpec {
        FontSpec {
            face: face.to_owned(),
            height,
            ..FontSpec::default()
        }
    }

    #[test]
    fn parses_the_embedded_font_lists_the_games_ship() {
        let entries = parse_embedded_font_list(GINKA_EMBEDDED_FONT_LIST);
        assert_eq!(entries.len(), 10);
        assert_eq!(
            entries[3],
            EmbeddedFontEntry {
                name: "源ノ角ゴシックB".to_owned(),
                file: "SourceHanSansJP-Bold.otf".to_owned(),
                face: "源ノ角ゴシック JP Bold".to_owned(),
            }
        );
        assert_eq!(
            entries[6],
            EmbeddedFontEntry {
                name: "思源黑体B".to_owned(),
                file: "SourceHanSansSC-Bold.otf".to_owned(),
                face: "Source Han Sans SC Bold".to_owned(),
            }
        );

        // The patch archive carries the same entries with English face names.
        let patch = parse_embedded_font_list(GINKA_PATCH_EMBEDDED_FONT_LIST);
        assert_eq!(patch.len(), 6);
        assert_eq!(patch[1].name, "源ノ角ゴシックB");
        assert_eq!(patch[1].face, "Source Han Sans JP Bold");

        let shoujo = parse_embedded_font_list(SHOUJO_EMBEDDED_FONT_LIST);
        assert_eq!(shoujo.len(), 3);
        assert_eq!(shoujo[1].name, "思源黑体中等");
        assert_eq!(shoujo[1].file, "sourcehansansjp-bold.otf");
        assert_eq!(shoujo[1].face, "思源黑体 CN Medium");
    }

    #[test]
    fn parses_the_default_font_map_aliases() {
        let aliases = parse_font_alias_pairs(GINKA_DEFAULT_FONT_MAP);
        assert!(aliases.contains(&("シーダ".to_owned(), "小杉ゴシック".to_owned())));
        assert!(aliases.contains(&("$記号$".to_owned(), "源ノ角ゴシックB".to_owned())));
        assert!(aliases.contains(&("lang_jp".to_owned(), "源ノ角ゴシックB".to_owned())));
        // The dict keys `lang:"ui"`/`alias:"SystemFont"` are not string pairs.
        assert!(!aliases.iter().any(|(key, _)| key == "ui"));
    }

    #[test]
    fn resolves_the_faces_ginka_requests() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();
        system
            .load_font_data("font/NunitoSans_10pt-SemiBold.ttf", nunito_test_font())
            .unwrap();
        system.register_embedded_font_list(parse_embedded_font_list(GINKA_EMBEDDED_FONT_LIST));
        system.register_font_alias_text(GINKA_DEFAULT_FONT_MAP);

        for face in [
            "源ノ角ゴシックB",                        // kag.chDefaultFace
            "思源黑体B",                              // kag.getLanguageFont(2)
            "Source Han Sans SC Bold",                // the embfontlist face name
            "思源黑体",                               // a localized name from the name table
            "$記号$",                                 // a deffontmap alias
            ",Source Han Sans SC Bold,ＭＳ ゴシック", // the realFace string the loader builds
        ] {
            let cjk = system.text_metrics(&spec(face, 48.0), "あ").width;
            let symbol = system.text_metrics(&spec(face, 48.0), "♪").width;
            assert_eq!(
                cjk, 48.0,
                "{face}: the CJK advance must come from the shipped face"
            );
            assert_eq!(symbol, 48.0, "{face}: the symbol must keep the CJK advance");
        }

        // The English-only face still resolves to the Latin font.
        assert_eq!(
            system.text_metrics(&spec("NunitoSans-SB", 48.0), "♪").width,
            24.0
        );
    }

    #[test]
    fn an_unresolved_face_measures_through_the_games_default_font() {
        let mut system = FontSystem::new();
        // The live 少女世界 message font measures with an *empty* face: the
        // game's `onGetTextWidth` sets only `font.height` and calls
        // `Font.getEscWidthX`, whose face is whatever the layer font last
        // carried. `GetBeingFont` (`FontSystem.cpp:92-96`) answers a default
        // font there, and the reference's default can draw the game's text;
        // a generic `fontdb` family cannot (the live box comes from FreeSans,
        // whose missing-glyph advance lays CJK out at 0.8 em — 32 px at the
        // game's size 40 — while the drawn glyphs are 1 em wide and overlap).
        system.db.set_sans_serif_family("Nunito Sans 10pt");
        system
            .load_font_data("font/sourcehansanssc-medium.otf", cn_medium_test_font())
            .unwrap();
        system
            .load_font_data("font/NunitoSans_10pt-SemiBold.ttf", nunito_test_font())
            .unwrap();

        assert_eq!(
            system.text_metrics(&spec("", 40.0), "あ").width,
            40.0,
            "the empty face must measure through the game's own font"
        );
        assert_eq!(
            system.text_metrics(&spec("No Such Face", 40.0), "あ").width,
            40.0,
            "an unknown face gets the same default"
        );
        // A requested face still wins over the default.
        assert_eq!(
            system
                .text_metrics(&spec("Nunito Sans 10pt", 40.0), "A")
                .width,
            24.0
        );
    }

    /// 纸上的魔法使 asks for `华文细黑`, which resolves to nothing: no
    /// `System.addFont` face, no table, no alias. The spec must still draw CJK
    /// through the default being font (`FontSystem.cpp:92-96`), and that
    /// default has to be a face that can draw the text — the game's own faces
    /// first. Registering a Latin-only face before a CJK face must not send a
    /// CJK character through the Latin face's missing-glyph character.
    #[test]
    fn an_unresolved_face_measures_cjk_through_a_registered_face_that_can_draw_it() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/NunitoSans_10pt-SemiBold.ttf", nunito_test_font())
            .unwrap();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();

        let unresolved = spec("华文细黑", 48.0);
        // 忆 is the first character of the game's first message; the CJK face
        // draws it a full em wide, the Latin face only has its own □ (half
        // width, 0.5 em).
        assert_eq!(system.text_metrics(&unresolved, "忆").width, 48.0);
        assert_eq!(system.text_metrics(&unresolved, "忆あ").width, 96.0);
        // The drawn rect is the CJK face's glyph, not the Latin face's □.
        let resolved = spec("Source Han Sans SC Bold", 48.0);
        assert_eq!(
            system.glyph_draw_rect(&unresolved, '忆'),
            system.glyph_draw_rect(&resolved, '忆')
        );
        assert_ne!(
            system.glyph_draw_rect(&unresolved, '忆'),
            system.glyph_draw_rect(&spec("Nunito Sans 10pt SemiBold", 48.0), '□')
        );
    }

    /// The being font is resolved once per spec, like the reference's
    /// `GetBeingFont` (`FontSystem.cpp:53-97`), so a character's advance cannot
    /// depend on the rest of the string. With a CJK-capable registered default,
    /// a Latin character the CJK face lacks draws that face's own default glyph
    /// (□, full width) whether it is measured alone or inside a CJK run.
    #[test]
    fn one_being_font_per_spec_keeps_advances_context_free() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/NunitoSans_10pt-SemiBold.ttf", nunito_test_font())
            .unwrap();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();

        let unresolved = spec("华文细黑", 48.0);
        // 'A' exists only in the Latin registered face, at a half-em advance
        // (28.8 here); through this spec it must be the pinned CJK face's
        // default character (48), not the Latin glyph a text-dependent default
        // would hand it.
        assert_eq!(system.text_metrics(&unresolved, "A").width, 48.0);
        assert_eq!(system.text_metrics(&unresolved, "忆").width, 48.0);
        assert_eq!(system.text_metrics(&unresolved, "忆A").width, 96.0);
        assert_eq!(system.text_metrics(&unresolved, "A忆A").width, 144.0);
    }

    /// A character no loaded face carries must not re-route the run: the spec
    /// keeps its default being font and only that one character draws the
    /// face's own default glyph (`FreeTypeFontRasterizer.cpp:119-127`).
    #[test]
    fn an_uncovered_character_does_not_reroute_the_whole_run() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/NunitoSans_10pt-SemiBold.ttf", nunito_test_font())
            .unwrap();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();

        let unresolved = spec("华文细黑", 48.0);
        // U+1F600 is in no fixture face. 忆 must still come from the CJK face
        // (48 + the emoji's default glyph 48); a selection that asks a single
        // face to cover the whole text falls back to the Latin face and boxes
        // the entire run (24 + 24).
        assert_eq!(system.text_metrics(&unresolved, "忆\u{1f600}").width, 96.0);
        assert_eq!(
            system.glyph_draw_rect(&unresolved, '忆'),
            system.glyph_draw_rect(&spec("Source Han Sans SC Bold", 48.0), '忆')
        );
    }

    /// The live 纸上的魔法使 registers no fonts at all, so the default being
    /// font is the platform's own — which in the reference is a system font
    /// that can draw the game's text (`TVPSysFont.cpp:16-54`), never a bare
    /// `fontdb` generic family. On a machine with no CJK face there is nothing
    /// to measure through, and the test says so instead of failing.
    #[test]
    fn an_unresolved_face_measures_cjk_through_a_capable_system_face() {
        let system = FontSystem::new();
        if !system
            .db
            .faces()
            .any(|face| system.face_supports(face.id, '忆'))
        {
            eprintln!("no CJK-capable system face installed; nothing to measure through");
            return;
        }

        // At height 24 a CJK ideograph is a full em wide; the generic fallback
        // face's missing-glyph advance was the live bug's 0.8 em (20 px).
        let width = system.text_metrics(&spec("华文细黑", 24.0), "忆").width;
        assert!(
            width > 22.0,
            "an unknown face must draw 忆 through a face that has it, got {width}"
        );
    }

    /// A face a candidate resolves to is never replaced by the covering
    /// default: one face serves the spec (`FreeTypeFontRasterizer.cpp:43-90`),
    /// and only an unresolvable request reaches the default being font.
    #[test]
    fn a_resolved_face_is_still_preferred_over_a_covering_default() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/NunitoSans_10pt-SemiBold.ttf", nunito_test_font())
            .unwrap();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();

        // `Nunito Sans 10pt SemiBold` resolves to the Latin face, which has no
        // あ: it draws its own □ (0.5 em), not the CJK face's あ (1 em).
        let latin = system.text_metrics(&spec("Nunito Sans 10pt SemiBold", 48.0), "あ");
        assert_eq!(latin.width, 24.0);
        // The same character through an unresolvable face gets the default.
        let unresolved = system.text_metrics(&spec("华文细黑", 48.0), "あ");
        assert_eq!(unresolved.width, 48.0);
    }

    #[test]
    fn a_face_spelling_of_either_table_resolves_to_the_shipped_variant() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();
        // Both archives are registered in the order the plugin reads them
        // (`embfontlist.tjs` first, then `font/embfontlist.tjs`), and one entry
        // differing only in its face spelling must survive the second read.
        system
            .register_embedded_font_list(parse_embedded_font_list(GINKA_PATCH_EMBEDDED_FONT_LIST));
        system.register_embedded_font_list(parse_embedded_font_list(GINKA_EMBEDDED_FONT_LIST));

        // The JP files are not shipped, so every spelling the two tables give
        // that entry has to land on the shipped SC face; the Japanese spelling
        // is the one the live build's table declares.
        for face in [
            "源ノ角ゴシック JP Bold",
            "Source Han Sans JP Bold",
            "源ノ角ゴシックB",
        ] {
            assert_eq!(
                system.text_metrics(&spec(face, 48.0), "あ").width,
                48.0,
                "{face}: must resolve to the shipped SC face"
            );
            assert_eq!(
                system.text_metrics(&spec(face, 48.0), "♪").width,
                48.0,
                "{face}"
            );
        }
    }

    #[test]
    fn an_entries_own_file_wins_over_the_region_free_match() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/sourcehansansjp-bold.otf", jp_bold_test_font())
            .unwrap();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();
        system
            .register_embedded_font_list(parse_embedded_font_list(GINKA_PATCH_EMBEDDED_FONT_LIST));
        system.register_embedded_font_list(parse_embedded_font_list(GINKA_EMBEDDED_FONT_LIST));

        // Both regional variants are loaded, so each entry must get its own
        // file. Matching only through the region-free key would hand the SC
        // entry the JP file, whose あ advances 800/1000 em (38.4 px at 48).
        let sc = system.text_metrics(&spec("思源黑体B", 48.0), "あ").width;
        assert_eq!(sc, 48.0, "思源黑体B must resolve to its own SC file");
        let jp = system
            .text_metrics(&spec("源ノ角ゴシックB", 48.0), "あ")
            .width;
        assert!(
            (jp - 38.4).abs() < 0.05,
            "源ノ角ゴシックB must resolve to its own JP file, got {jp}"
        );
        // The same mix-up shows up in the drawn rect: the fixtures' boxes
        // differ between the two variants.
        let sc_rect = system.glyph_draw_rect(&spec("思源黑体B", 48.0), 'あ');
        let jp_rect = system.glyph_draw_rect(&spec("源ノ角ゴシックB", 48.0), 'あ');
        assert!(sc_rect.is_some(), "the SC face must draw");
        assert!(jp_rect.is_some(), "the JP face must draw");
        assert_ne!(sc_rect, jp_rect);
    }

    #[test]
    fn resolves_the_face_the_second_game_requests() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/sourcehansansjp-bold.otf", cn_medium_test_font())
            .unwrap();
        system.register_embedded_font_list(parse_embedded_font_list(SHOUJO_EMBEDDED_FONT_LIST));

        for face in [
            "思源黑体中等",
            "思源黑体 CN Medium",
            "Source Han Sans CN Medium",
        ] {
            let cjk = system.text_metrics(&spec(face, 48.0), "あ").width;
            let symbol = system.text_metrics(&spec(face, 48.0), "♪").width;
            assert_eq!(
                cjk, 48.0,
                "{face}: the CJK advance must come from the shipped face"
            );
            assert_eq!(symbol, 48.0, "{face}: the symbol must keep the CJK advance");
        }
    }

    #[test]
    fn a_symbol_inside_a_cjk_run_never_switches_faces_or_halves_the_advance() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();
        system
            .load_font_data("font/NunitoSans_10pt-SemiBold.ttf", nunito_test_font())
            .unwrap();
        system.register_embedded_font_list(parse_embedded_font_list(GINKA_EMBEDDED_FONT_LIST));

        let face = "源ノ角ゴシックB";
        // ♪ is in the shipped face, so it keeps that face's full advance.
        assert_eq!(system.text_metrics(&spec(face, 48.0), "あ♪").width, 96.0);

        // `&` exists only in the loaded Latin face (half width there). The run
        // must not borrow it: the CJK face draws its own default character
        // (□) instead, so the advance stays 48 and not the Latin 24.
        let ampersand = system.text_metrics(&spec(face, 48.0), "&").width;
        let default_char = system.text_metrics(&spec(face, 48.0), "□").width;
        assert_eq!(ampersand, default_char);
        assert_eq!(ampersand, 48.0);
        assert_eq!(system.text_metrics(&spec(face, 48.0), "あ&あ").width, 144.0);
        assert_eq!(
            system.glyph_draw_rect(&spec(face, 48.0), '&'),
            system.glyph_draw_rect(&spec(face, 48.0), '□')
        );
    }

    #[test]
    fn a_missing_glyph_draws_the_faces_own_default_character() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();
        system
            .load_font_data("font/NunitoSans_10pt-SemiBold.ttf", nunito_test_font())
            .unwrap();
        system.register_embedded_font_list(parse_embedded_font_list(GINKA_EMBEDDED_FONT_LIST));

        // U+1F600 is absent from every loaded face, so it draws the resolved
        // face's own default character (□) — glyph and advance both.
        let missing = system
            .text_metrics(&spec("思源黑体B", 48.0), "\u{1f600}")
            .width;
        let default_char = system.text_metrics(&spec("思源黑体B", 48.0), "□").width;
        assert_eq!(missing, default_char);
        assert_eq!(missing, 48.0);

        let missing_rect = system.glyph_draw_rect(&spec("思源黑体B", 48.0), '\u{1f600}');
        assert_eq!(
            missing_rect,
            system.glyph_draw_rect(&spec("思源黑体B", 48.0), '□')
        );
        // It is the CJK face's box, not the Latin face's glyph.
        assert_ne!(
            missing_rect,
            system.glyph_draw_rect(&spec("NunitoSans-SB", 48.0), '♪')
        );
    }

    #[test]
    fn a_face_without_the_glyphs_keeps_its_own_advances() {
        let mut system = FontSystem::new();
        system
            .load_font_data("font/sourcehansanssc-bold.otf", sc_bold_test_font())
            .unwrap();
        system
            .load_font_data("font/NunitoSans_10pt-SemiBold.ttf", nunito_test_font())
            .unwrap();
        system.register_embedded_font_list(parse_embedded_font_list(GINKA_EMBEDDED_FONT_LIST));

        // The Latin face has no CJK glyphs; the CJK advance must not leak in
        // from the other loaded face.
        let latin = system.text_metrics(&spec("NunitoSans-SB", 48.0), "あ");
        assert_eq!(latin.width, 24.0);
    }
}
