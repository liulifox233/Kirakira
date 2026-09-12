//! Parsing of `src`/`icon` content references.
//!
//! Layer frame content names its target with two fields. FreeMote writes a
//! texture key plus a separate `icon` field; PARQUET writes a single
//! `"src/<source>/<icon>"` string. Both spellings are split into a
//! `(source, icon)` pair here, and `"motion/<object>/<motion>"` references
//! (which name another motion, not an icon) are rejected.

/// A `src`/`icon` pair naming one icon of the model's source table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IconReference {
    pub source: String,
    pub icon: String,
}

/// Splits `src` (and an optional `icon` field) into a source/icon pair.
///
/// Accepted spellings:
///
/// - `"src/<source>/<icon>"` (PARQUET) — the icon comes from the path,
/// - `"<source>/<icon>"` — already normalised,
/// - `"src/<source>"` or `"<source>"` plus a separate `icon` field.
///
/// `"motion/..."`, empty and icon-less references return `None`.
pub(crate) fn split_icon_reference(src: &str, icon: Option<&str>) -> Option<IconReference> {
    let src = src.strip_prefix("src/").unwrap_or(src);
    if src.is_empty() || src.starts_with("motion/") {
        return None;
    }

    let mut parts = src.split('/').filter(|part| !part.is_empty());
    let first = parts.next()?;
    let second = parts.next();
    if parts.next().is_some() {
        return None;
    }

    match second {
        Some(icon) => Some(IconReference {
            source: first.to_owned(),
            icon: icon.to_owned(),
        }),
        None => icon
            .filter(|icon| !icon.is_empty())
            .map(|icon| IconReference {
                source: first.to_owned(),
                icon: icon.to_owned(),
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_spellings() {
        let parquet = split_icon_reference("src/SD101/bg", None).unwrap();
        assert_eq!(parquet.source, "SD101");
        assert_eq!(parquet.icon, "bg");

        let freemote = split_icon_reference("SD101", Some("bg")).unwrap();
        assert_eq!(freemote, parquet);
    }

    #[test]
    fn rejects_motion_and_incomplete_references() {
        assert!(split_icon_reference("motion/SD101/ef_moya", None).is_none());
        assert!(split_icon_reference("src/SD101", None).is_none());
        assert!(split_icon_reference("src/SD101/bg/extra", None).is_none());
        assert!(split_icon_reference("", Some("bg")).is_none());
    }
}
