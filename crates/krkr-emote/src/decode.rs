//! Pixel decoding for `.mtn` icon resources.
//!
//! A motion's icons do not carry raw images. Each icon names two PSB resources
//! (`pixel` and, for paletted art, `pal`) plus a `compress` tag; PARQUET writes
//! `"RL"` on every icon of every `.mtn` file it ships, and the few resources
//! without the tag are stored verbatim.
//!
//! # The RL container
//!
//! `RL` is M2's run-length codec, with a per-pixel alignment because the same
//! codec stores both 8-bit paletted icons and 32-bit RGBA textures:
//!
//! - a control byte with bit `0x80` set is a *run*: `(control & 0x7f) + 3`
//!   copies of the next `align` bytes;
//! - any other control byte is a *literal*: `(control + 1) * align` raw bytes.
//!
//! The rule matches FreeMote's `RleCompress.Decompress` (the tool the community
//! reads M2 containers with): `count = (current ^ 0x80) + 3` for the run case
//! and `count = (current + 1) * align` for the literal case. Every icon of
//! PARQUET's 23 `.mtn` files decodes to exactly `width * height * align` bytes
//! under it — the check `crates/krkr-emote/tests/parquet.rs` keeps.
//!
//! # Paletted icons
//!
//! An icon with a `pal` resource stores one byte per pixel (a palette index);
//! the palette is 1024 bytes, 256 little-endian `0xAARRGGBB` entries. Icons
//! without one store RGBA directly, four bytes per pixel, byte order R, G, B, A
//! (the engine's own order, and the order the decoded bytes are returned in).

use std::fmt;

/// One decoded icon texture: tightly packed R, G, B, A bytes, top-down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedTexture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl DecodedTexture {
    /// The byte offset of pixel `(x, y)`, or `None` outside the texture.
    pub fn offset(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        Some((y as usize * self.width as usize + x as usize) * 4)
    }

    /// The RGBA pixel at `(x, y)`, clamped to the texture edges.
    pub fn pixel_clamped(&self, x: i32, y: i32) -> [u8; 4] {
        let x = x.clamp(0, self.width.saturating_sub(1) as i32) as usize;
        let y = y.clamp(0, self.height.saturating_sub(1) as i32) as usize;
        let offset = (y * self.width as usize + x) * 4;
        [
            self.rgba[offset],
            self.rgba[offset + 1],
            self.rgba[offset + 2],
            self.rgba[offset + 3],
        ]
    }
}

/// Why an icon's pixels could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The RL stream ends inside a run or a literal.
    Truncated { needed: usize, available: usize },
    /// The decoded stream is not the size the icon's dimensions call for.
    SizeMismatch {
        expected: usize,
        actual: usize,
        width: u32,
        height: u32,
        bytes_per_pixel: usize,
    },
    /// The `compress` tag names a codec this decoder does not implement.
    UnsupportedCompression(String),
    /// A paletted icon's `pal` resource is too short for 256 entries.
    PaletteTooShort { actual: usize },
    /// The icon has no usable dimensions.
    EmptyTexture { width: u32, height: u32 },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, available } => write!(
                f,
                "RL stream is truncated: needs {needed} bytes, {available} available"
            ),
            Self::SizeMismatch {
                expected,
                actual,
                width,
                height,
                bytes_per_pixel,
            } => write!(
                f,
                "decoded {actual} bytes, expected {expected} for a {width}x{height} icon at \
                 {bytes_per_pixel} bytes per pixel"
            ),
            Self::UnsupportedCompression(compress) => {
                write!(f, "unsupported texture compression '{compress}'")
            }
            Self::PaletteTooShort { actual } => write!(
                f,
                "palette holds {actual} bytes, a 256-entry ARGB palette needs 1024"
            ),
            Self::EmptyTexture { width, height } => {
                write!(f, "icon has no pixels ({width}x{height})")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decodes an M2 `RL` stream into `align`-byte units.
///
/// `align` is the pixel size: 4 for an RGBA texture, 1 for a paletted one.
pub fn decode_rle(data: &[u8], align: usize) -> Result<Vec<u8>, DecodeError> {
    debug_assert!(align > 0);
    let mut out = Vec::with_capacity(data.len().saturating_mul(2));
    let mut cursor = 0usize;
    while cursor < data.len() {
        let control = data[cursor];
        cursor += 1;
        if control & 0x80 != 0 {
            let count = (control & 0x7f) as usize + 3;
            let needed = align;
            let available = data.len() - cursor;
            if available < needed {
                return Err(DecodeError::Truncated { needed, available });
            }
            let unit = &data[cursor..cursor + align];
            cursor += align;
            for _ in 0..count {
                out.extend_from_slice(unit);
            }
        } else {
            let count = (control as usize + 1) * align;
            let available = data.len() - cursor;
            if available < count {
                return Err(DecodeError::Truncated {
                    needed: count,
                    available,
                });
            }
            out.extend_from_slice(&data[cursor..cursor + count]);
            cursor += count;
        }
    }
    Ok(out)
}

/// Expands one icon's resource bytes into an RGBA texture.
///
/// `palette` is the icon's `pal` resource when it has one: then `pixels` holds
/// one index byte per pixel and the palette is applied. `compress` is the
/// icon's `compress` tag: `"RL"` (PARQUET's spelling) runs the RL decoder,
/// anything else — including the absent tag — is read as raw pixels.
pub fn decode_icon(
    pixels: &[u8],
    palette: Option<&[u8]>,
    width: u32,
    height: u32,
    compress: Option<&str>,
) -> Result<DecodedTexture, DecodeError> {
    if width == 0 || height == 0 {
        return Err(DecodeError::EmptyTexture { width, height });
    }
    let bytes_per_pixel = if palette.is_some() { 1 } else { 4 };
    let expected = width as usize * height as usize * bytes_per_pixel;

    let decoded = match compress {
        Some("RL") => decode_rle(pixels, bytes_per_pixel)?,
        Some(other) => return Err(DecodeError::UnsupportedCompression(other.to_owned())),
        None => pixels.to_vec(),
    };
    if decoded.len() != expected {
        return Err(DecodeError::SizeMismatch {
            expected,
            actual: decoded.len(),
            width,
            height,
            bytes_per_pixel,
        });
    }

    let rgba = match palette {
        Some(palette) => expand_palette(&decoded, palette)?,
        None => decoded,
    };
    Ok(DecodedTexture {
        width,
        height,
        rgba,
    })
}

/// Applies a 256-entry little-endian `0xAARRGGBB` palette to index pixels.
fn expand_palette(indices: &[u8], palette: &[u8]) -> Result<Vec<u8>, DecodeError> {
    if palette.len() < 1024 {
        return Err(DecodeError::PaletteTooShort {
            actual: palette.len(),
        });
    }
    let mut rgba = Vec::with_capacity(indices.len() * 4);
    for &index in indices {
        let entry = index as usize * 4;
        let argb = u32::from_le_bytes([
            palette[entry],
            palette[entry + 1],
            palette[entry + 2],
            palette[entry + 3],
        ]);
        rgba.push(((argb >> 16) & 0xff) as u8);
        rgba.push(((argb >> 8) & 0xff) as u8);
        rgba.push((argb & 0xff) as u8);
        rgba.push(((argb >> 24) & 0xff) as u8);
    }
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two encodings of one control byte, as FreeMote's decoder reads
    /// them: a `0x80` bit is a run of `(c & 0x7f) + 3` units, otherwise a
    /// literal of `(c + 1) * align` bytes.
    #[test]
    fn rle_run_and_literal() {
        // 0x80 = run of 3 units of value 7; 0x01 = literal of 2 units.
        let data = [0x80u8, 7, 7, 7, 7, 0x01, 1, 2, 3, 4, 5, 6, 7, 8];
        let out = decode_rle(&data, 4).expect("decodes");
        assert_eq!(out.len(), 3 * 4 + 2 * 4);
        assert!(out[..12].iter().all(|&byte| byte == 7));
        assert_eq!(&out[12..], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn rle_is_alignment_aware() {
        // One `0xff` run (127 + 3 = 130 units of 5) plus one literal `0x01`
        // (2 units of 9) — the same stream at 1 byte per pixel and at 4.
        let narrow = [0xffu8, 5, 0x01, 9, 9];
        assert_eq!(
            decode_rle(&narrow, 1).unwrap(),
            [vec![5u8; 130], vec![9; 2]].concat()
        );
        let wide = decode_rle(&[0xff, 1, 2, 3, 4, 0x01, 9, 9, 9, 9, 9, 9, 9, 9], 4).unwrap();
        assert_eq!(wide.len(), (130 + 2) * 4);
        assert_eq!(&wide[..4], &[1, 2, 3, 4]);
        assert_eq!(&wide[wide.len() - 4..], &[9, 9, 9, 9]);
    }

    #[test]
    fn truncated_streams_are_errors() {
        assert!(matches!(
            decode_rle(&[0x80, 1, 2], 4),
            Err(DecodeError::Truncated { .. })
        ));
        assert!(matches!(
            decode_rle(&[0x01, 1, 2, 3], 4),
            Err(DecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn raw_icons_need_exactly_their_size() {
        let pixels = vec![9u8; 3 * 2 * 4];
        let texture = decode_icon(&pixels, None, 3, 2, None).expect("raw RGBA");
        assert_eq!(texture.rgba.len(), 24);
        assert_eq!(texture.pixel_clamped(2, 1), [9, 9, 9, 9]);
        assert_eq!(texture.offset(3, 0), None);
        assert!(matches!(
            decode_icon(&pixels[..23], None, 3, 2, None),
            Err(DecodeError::SizeMismatch {
                expected: 24,
                actual: 23,
                ..
            })
        ));
    }

    #[test]
    fn paletted_icons_expand_through_the_palette() {
        // 2x1 indices 0 and 1; palette: red 0xffff0000, translucent blue.
        let mut palette = vec![0u8; 1024];
        palette[..4].copy_from_slice(&0xffff_0000u32.to_le_bytes());
        palette[4..8].copy_from_slice(&0x8000_00ffu32.to_le_bytes());
        let texture = decode_icon(&[0, 1], Some(&palette), 2, 1, None).expect("paletted");
        assert_eq!(texture.rgba, [255, 0, 0, 255, 0, 0, 255, 128]);
    }

    #[test]
    fn unknown_compression_is_reported() {
        let error = decode_icon(&[0; 16], None, 2, 2, Some("LZ4")).expect_err("LZ4");
        assert_eq!(error, DecodeError::UnsupportedCompression("LZ4".to_owned()));
    }
}
