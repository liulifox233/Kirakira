//! Adaptation of PARQUET's `.mtn` source-table flavor to eluna's schema.
//!
//! eluna models the FreeMote flavor: `source.<name>.texture` carries the
//! resource index and pixel size, and layer content names a texture plus a
//! separate `icon` field. PARQUET's `.mtn` files instead hang the resource off
//! each icon — `source.<name>.icon.<icon>.{pixel,pal,width,height,originX,
//! originY,clip,compress,resolution}` with no `texture` sub-object — and name
//! the icon in a single `content.src = "src/<source>/<icon>"` string.
//!
//! This module bridges the two by rewriting the *parsed* tree before eluna's
//! schema/scene code reads it:
//!
//! - one synthetic `source["<source>/<icon>"]` entry per icon, carrying a
//!   `texture` (`pixel`/`width`/`height` of that icon) and a single-icon
//!   `icon` table with a zero origin rectangle,
//! - layer content `src` rewritten to that synthetic key with `content.icon`
//!   set, keeping every other reference (`motion/<object>/<motion>`) intact,
//! - every frame `content.opa` rescaled from the file's 0..255 byte to the
//!   0..10 scale eluna divides by ([`rescale_opacity`]), and
//! - `parameterize: null` dropped so a non-parameterised layer follows its
//!   timeline instead of freezing at local time 0
//!   ([`strip_null_parameterize`]).
//!
//! Files that already follow the FreeMote flavor are left alone: a source with
//! a `texture` sub-object is not synthesised, and content that does not use the
//! `src/` spelling is not rewritten. The vendored eluna code stays unpatched;
//! the original bytes are never modified.

use std::collections::{BTreeMap, BTreeSet};

use eluna::{PsbFile, PsbValue};

use crate::reference::split_icon_reference;

/// What one normalisation pass changed in the parsed tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NormalizeReport {
    /// `source.*` entries seen.
    pub sources: usize,
    /// Synthetic `source["<source>/<icon>"]` entries added.
    pub synthesized_textures: usize,
    /// `content.src` values rewritten to a resolved texture key.
    pub rewritten_contents: usize,
    /// `src/<source>/<icon>` references that no source/icon matched.
    pub unresolved_icon_references: usize,
    /// Icons skipped because they carry no `pixel` resource.
    pub icons_without_pixel: usize,
    /// Frame `content.opa` values rescaled from the file's 0..255 byte to the
    /// 0..10 scale eluna's scene builder divides by (the `rescale_opacity`
    /// pass).
    pub rescaled_opacity: usize,
    /// `parameterize: null` fields dropped so eluna reads the layer's own
    /// timeline (the `strip_null_parameterize` pass).
    pub dropped_null_parameterize: usize,
}

pub(crate) fn normalize_source_table(psb: &mut PsbFile) -> NormalizeReport {
    let mut report = NormalizeReport::default();

    let PsbValue::Object(root) = &mut psb.root else {
        return report;
    };
    let Some(PsbValue::Object(sources)) = root
        .iter_mut()
        .find_map(|(key, value)| (key == "source").then_some(value))
    else {
        return report;
    };

    let mut additions = Vec::new();
    for (source_name, source) in sources.iter() {
        report.sources += 1;
        if source.field("texture").is_some() {
            continue;
        }
        let Some(icons) = source.field("icon").and_then(PsbValue::as_object) else {
            continue;
        };
        for (icon_name, icon) in icons {
            match synthesize_texture(source, icon_name, icon) {
                Some(entry) => {
                    additions.push((format!("{source_name}/{icon_name}"), entry));
                    report.synthesized_textures += 1;
                }
                None => report.icons_without_pixel += 1,
            }
        }
    }
    sources.extend(additions);

    let (texture_keys, source_icons) = index_sources(sources);
    rewrite_references(&mut psb.root, &texture_keys, &source_icons, &mut report);
    rescale_opacity(&mut psb.root, false, &mut report);
    strip_null_parameterize(&mut psb.root, &mut report);

    report
}

/// Drops every `parameterize: null` field.
///
/// PARQUET writes `parameterize: null` on layers that are *not* parameterised
/// (every layer of `sd101.mtn`, for one). eluna's `layer_parameter_eval` tests
/// only for the field's presence: a present-but-null `parameterize` resolves to
/// no parameter and the layer's local time collapses to 0
/// (`vendor/eluna/crates/eluna/src/emote.rs:1486-1489`), so the layer is drawn
/// at its first frame's state forever and any later keyframe — PARQUET's
/// `opa: 192` fade on `ef_moya/bgef1` at tick 90, for example — never
/// activates. The reference has no such freeze; removing the null field is the
/// in-adapter equivalent of eluna's own "absent means not parameterised" path.
fn strip_null_parameterize(value: &mut PsbValue, report: &mut NormalizeReport) {
    match value {
        PsbValue::Object(fields) => {
            let before = fields.len();
            fields.retain(|(name, value)| {
                !(name == "parameterize" && matches!(value, PsbValue::Null))
            });
            report.dropped_null_parameterize += before - fields.len();
            for (_, child) in fields.iter_mut() {
                strip_null_parameterize(child, report);
            }
        }
        PsbValue::List(values) => {
            for value in values {
                strip_null_parameterize(value, report);
            }
        }
        _ => {}
    }
}

/// Rescales every frame `content.opa` from the file's 0..255 byte scale to the
/// 0..10 scale eluna's `ctx_with_opacity`/`build_sprite` divide by
/// (`vendor/eluna/crates/eluna/src/emote.rs:1113-1177`).
///
/// The reference reads the field as an integer and keeps one byte of it —
/// `motionplayer_nod3d.dll` `FUN_1001d000` does
/// `movzbl %al, %ecx; movl %ecx, 0x48(%ebx)` after its `opa` lookup, with the
/// frame state's opacity byte initialised to `0xff` — so a value of 192 is
/// 192/255 (≈0.75), an absent `opa` is fully opaque, and `opa: 0` is
/// transparent. eluna's `/10` reading turns every value ≥ 10 into full
/// opacity, which is why PARQUET's `opa: 192` layer renders solid and why the
/// 0..9 range collapses; rescaling here keeps the vendored copy unpatched and
/// makes the draw list's [`crate::MotionDrawItem::opacity`] agree with the
/// reference for every value. `crates/krkr-emote/tests/parquet.rs` measures
/// this on the game's own `sd101.mtn`.
fn rescale_opacity(value: &mut PsbValue, in_content: bool, report: &mut NormalizeReport) {
    match value {
        PsbValue::Object(fields) => {
            if in_content
                && let Some((_, opa)) = fields.iter_mut().find(|(name, _)| name == "opa")
                && let Some(byte) = opacity_byte(opa)
            {
                *opa = PsbValue::Float(byte as f32 * 10.0 / 255.0);
                report.rescaled_opacity += 1;
            }
            for (name, child) in fields.iter_mut() {
                rescale_opacity(child, name == "content" || in_content, report);
            }
        }
        PsbValue::List(values) => {
            for value in values {
                rescale_opacity(value, in_content, report);
            }
        }
        _ => {}
    }
}

/// The byte the reference's `opa` read keeps (`vp & 0xff` in
/// `motionplayer_nod3d.dll` `FUN_1001d000`).
fn opacity_byte(value: &PsbValue) -> Option<u8> {
    match value {
        PsbValue::Int(value) => Some(*value as u8),
        PsbValue::Float(value) => Some(value.round().clamp(0.0, 255.0) as u8),
        PsbValue::Double(value) => Some(value.round().clamp(0.0, 255.0) as u8),
        _ => None,
    }
}

/// Builds one synthetic `source` entry for an icon.
fn synthesize_texture(source: &PsbValue, icon_name: &str, icon: &PsbValue) -> Option<PsbValue> {
    let resource_index = icon.field_u32("pixel")?;
    let width = icon.field_f32("width")?;
    let height = icon.field_f32("height")?;
    let resolution = icon
        .field_f32("resolution")
        .filter(|resolution| resolution.is_finite() && *resolution > 0.0)
        .unwrap_or(1.0);

    let mut texture = vec![
        ("pixel".to_owned(), PsbValue::Resource(resource_index)),
        (
            "width".to_owned(),
            PsbValue::Int(scaled_dimension(width, resolution)),
        ),
        (
            "height".to_owned(),
            PsbValue::Int(scaled_dimension(height, resolution)),
        ),
    ];
    if let Some(kind) = source.field("type") {
        texture.push(("type".to_owned(), kind.clone()));
    }
    if let Some(compress) = icon.field("compress") {
        texture.push(("compress".to_owned(), compress.clone()));
    }

    let mut icon_fields = vec![
        ("left".to_owned(), PsbValue::Float(0.0)),
        ("top".to_owned(), PsbValue::Float(0.0)),
        ("width".to_owned(), PsbValue::Float(width)),
        ("height".to_owned(), PsbValue::Float(height)),
        ("resolution".to_owned(), PsbValue::Float(resolution)),
    ];
    for name in ["originX", "originY", "attr"] {
        if let Some(value) = icon.field(name) {
            icon_fields.push((name.to_owned(), value.clone()));
        }
    }

    Some(PsbValue::Object(vec![
        ("texture".to_owned(), PsbValue::Object(texture)),
        (
            "icon".to_owned(),
            PsbValue::Object(vec![(icon_name.to_owned(), PsbValue::Object(icon_fields))]),
        ),
    ]))
}

fn scaled_dimension(value: f32, resolution: f32) -> i64 {
    let scaled = f64::from(value) * f64::from(resolution);
    if !scaled.is_finite() || scaled < 1.0 {
        1
    } else {
        scaled.round() as i64
    }
}

/// Collects which texture keys exist and which icons each source declares.
fn index_sources(
    sources: &[(String, PsbValue)],
) -> (BTreeSet<String>, BTreeMap<String, BTreeSet<String>>) {
    let mut texture_keys = BTreeSet::new();
    let mut source_icons: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for (name, value) in sources {
        if value.field("texture").is_some() {
            texture_keys.insert(name.clone());
        }
        if let Some(icons) = value.field("icon").and_then(PsbValue::as_object) {
            source_icons
                .entry(name.clone())
                .or_default()
                .extend(icons.iter().map(|(icon, _)| icon.clone()));
        }
    }

    (texture_keys, source_icons)
}

fn rewrite_references(
    value: &mut PsbValue,
    texture_keys: &BTreeSet<String>,
    source_icons: &BTreeMap<String, BTreeSet<String>>,
    report: &mut NormalizeReport,
) {
    match value {
        PsbValue::Object(fields) => {
            if let Some((src, icon)) = content_reference(fields)
                && src.starts_with("src/")
            {
                match split_icon_reference(&src, icon.as_deref()).and_then(|reference| {
                    texture_key(
                        texture_keys,
                        source_icons,
                        &reference.source,
                        &reference.icon,
                    )
                    .map(|key| (key, reference.icon))
                }) {
                    Some((key, icon)) => {
                        set_field(fields, "src", PsbValue::String(key));
                        set_field(fields, "icon", PsbValue::String(icon));
                        report.rewritten_contents += 1;
                    }
                    None => report.unresolved_icon_references += 1,
                }
            }
            for (_, child) in fields.iter_mut() {
                rewrite_references(child, texture_keys, source_icons, report);
            }
        }
        PsbValue::List(values) => {
            for value in values {
                rewrite_references(value, texture_keys, source_icons, report);
            }
        }
        _ => {}
    }
}

fn content_reference(fields: &[(String, PsbValue)]) -> Option<(String, Option<String>)> {
    let src = fields
        .iter()
        .find_map(|(name, value)| (name == "src").then_some(value)?.as_str())
        .map(str::to_owned)?;
    let icon = fields
        .iter()
        .find_map(|(name, value)| (name == "icon").then_some(value)?.as_str())
        .map(str::to_owned);
    Some((src, icon))
}

fn set_field(fields: &mut Vec<(String, PsbValue)>, name: &str, value: PsbValue) {
    match fields.iter_mut().find(|(field, _)| field == name) {
        Some((_, field_value)) => *field_value = value,
        None => fields.push((name.to_owned(), value)),
    }
}

/// The texture key `(source, icon)` resolves to in the adapted tree:
/// the synthetic `"<source>/<icon>"` entry when one was added, otherwise a
/// FreeMote-style source that already carries the icon under a `texture`.
fn texture_key(
    texture_keys: &BTreeSet<String>,
    source_icons: &BTreeMap<String, BTreeSet<String>>,
    source: &str,
    icon: &str,
) -> Option<String> {
    let split = format!("{source}/{icon}");
    if texture_keys.contains(&split) {
        return Some(split);
    }
    let source_carries_icon = source_icons
        .get(source)
        .is_some_and(|icons| icons.contains(icon));
    (texture_keys.contains(source) && source_carries_icon).then(|| source.to_owned())
}
