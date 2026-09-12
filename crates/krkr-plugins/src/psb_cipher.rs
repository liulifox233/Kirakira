//! PSB cipher layer: the E-mote XOR scheme and the key store behind
//! `psb_file`'s key-required diagnosis.
//!
//! A PSB document's encryption flags describe what its toolchain did, not what
//! its bytes prove (M45: M2 sets the header bit on plaintext `.pimg`
//! documents), so the reader in `psb_file` parses first and asks for a key
//! only when the structure cannot defend itself and the flags still explain
//! the damage.  This module answers that question: it repairs a private copy
//! of the document with every key the configuration supplies, and hands each
//! repair back to the reader, whose own admission — v3 Adler-32 checksum,
//! then the structural parse — decides which repair, if any, was real.
//!
//! # The scheme
//!
//! Every E-mote key shares three fixed seed words and varies only in a fourth,
//! the title's private key DWORD: `0x075BCD15, 0x159A55E5, 0x1F123BB5, key, 0, 0`
//! (`PsbDecryptionKey::emote_key`, `vendor/eluna/crates/eluna/src/psb/mod.rs:268-299`;
//! FreeMote's `PsbHeader.Load(br, key)` and GARbro's `PsbReader.Parse(uint key)`
//! seed the same six words).  From that seed a byte-oriented XOR stream is
//! generated (`…/psb/mod.rs:891-919`): whenever the reservoir word runs out of
//! bytes the state shifts and the fourth word becomes the next keystream
//! dword.  The stream is *continuous across a document's ranges* — the header
//! bytes are consumed first, then the body, in a single pass — so the two
//! ranges below cannot be decrypted with independent streams.
//!
//! # Ranges
//!
//! * The header bit `0x0001` ciphers bytes `0x08..0x08+0x20` for v2 and
//!   `0x08..0x08+0x24` for v3 — the v3 range includes the Adler-32 word at
//!   `0x28`, which must be decrypted together with the header it protects
//!   (`…/psb/mod.rs:43-44, 853-867`; the same length table is in FreeMote
//!   `PsbHeader.cs:166-185` and in `xp3-brute` `src/encoder/psb.rs:1004-1013`).
//!   v4 widens the header to `0x30` bytes (FreeMote `PsbHeader.cs:180-185`,
//!   GARbro `ArcPSB.cs:361-372`).
//! * The body bit `0x0002` — or a root byte that is not `0x21`, the
//!   reference's implicit case for old v2 documents (`…/psb/mod.rs:869-870`;
//!   GARbro forces the same for v2 at `ArcPSB.cs:358-359`) — ciphers bytes
//!   `names_offset..chunk_offsets_offset`: the names trie, the root value and
//!   the string tables (`…/psb/mod.rs:872-881`; `xp3-brute`
//!   `src/encoder/psb.rs:1014-1019`; FreeMote `PsbFile.cs:342-353`; GARbro
//!   `ArcPSB.cs:403-404`).  The resource offset/length tables and the resource
//!   data behind them are never ciphered — which is why the reference writer
//!   lays the root value out between the names trie and the string tables
//!   (`emote-psb` `src/psb/write.rs:100-119`).  The implicit case is honoured
//!   only while some cipher bit is claimed: a document that flags nothing is
//!   never run through a cipher, even one whose old v2 body really is ciphered
//!   (a deliberate narrowing of the reference, kept here so the contract holds
//!   at both layers).
//!
//! The magic, version, flags and the resource payloads stay in the clear; the
//! MDF/LZ4 wrappers are outside this module (Phase 1 loads raw PSB bytes).
//!
//! # Keys
//!
//! Keys are configuration, never discovery: [`PsbConfig`] carries what a game
//! profile (or, until the engine has such a field, the `KRKR_PSB_KEYS`
//! environment variable) supplied, and [`PsbKeyStore`] tries the candidates in
//! order, keeping the first whose repair the reader accepts — the same
//! try-candidates-and-validate model as `xp3-brute`'s cached-key reuse
//! (`src/decoder/psb.rs:39-44, 286-293, 397-440`) and `eluna`'s full-candidate
//! check (`…/psb/mod.rs:752-758`).  Recovering a key that is not known (brute
//! force, executable mining) is deliberately not part of this module.

use std::{fmt, ops::Range};

/// Header-encryption flag bit, as eluna names it (`…/psb/mod.rs:40`).
pub(crate) const PSB_FLAG_HEADER_ENCRYPTED: u16 = 0x0001;
/// Body-encryption flag bit (`…/psb/mod.rs:41`).
pub(crate) const PSB_FLAG_BODY_ENCRYPTED: u16 = 0x0002;
/// Either bit means a key may be required to decode the document.
pub(crate) const PSB_FLAG_ENCRYPTION_HINT: u16 =
    PSB_FLAG_HEADER_ENCRYPTED | PSB_FLAG_BODY_ENCRYPTED;

/// Where the plugin reads keys until the engine profile carries them.
pub(crate) const PSB_KEYS_ENV_VAR: &str = "KRKR_PSB_KEYS";

/// The scheme's fixed seed words; only the fourth word is keyed
/// (`…/psb/mod.rs:268-271, 290-299`).
const EMOTE_SEED_WORDS: [u32; 3] = [0x075B_CD15, 0x159A_55E5, 0x1F12_3BB5];

/// PSB's object marker, the byte a v2 root must carry for the body to be
/// provably plaintext (`…/psb/mod.rs:887-889`).
const PSB_ROOT_OBJECT: u8 = 0x21;

/// Header fields the cipher has to read: the names and resource-table offsets
/// and the root offset, in the fixed v2/v3 header (`PsbHeader::read`).
const HEADER_NAMES_FIELD: usize = 0x0c;
const HEADER_CHUNK_OFFSETS_FIELD: usize = 0x18;
const HEADER_ROOT_FIELD: usize = 0x24;

/// One candidate key of the E-mote scheme: the title's private key DWORD.
///
/// The store's selection is only as good as the configuration handed to it;
/// this type is the unit of that configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PsbKey {
    word: u32,
}

impl PsbKey {
    /// A key of the canonical scheme (eluna's `emote_key`,
    /// `…/psb/mod.rs:290-299`).
    pub const fn emote(word: u32) -> Self {
        Self { word }
    }

    /// Reads a key from configuration: `0x`-prefixed hexadecimal or plain
    /// decimal.
    pub fn parse(token: &str) -> Result<Self, PsbKeyError> {
        let token = token.trim();
        let (digits, radix) = match token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
        {
            Some(hexadecimal) => (hexadecimal, 16),
            None => (token, 10),
        };
        u32::from_str_radix(digits, radix)
            .map(Self::emote)
            .map_err(|_| PsbKeyError::InvalidToken(token.to_string()))
    }
}

impl fmt::Display for PsbKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:08x}", self.word)
    }
}

/// Why a configured key was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PsbKeyError {
    /// Not a `0x`-prefixed hexadecimal or decimal `u32`.
    InvalidToken(String),
    /// The configuration is not valid Unicode.
    NotUnicode,
}

impl fmt::Display for PsbKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken(token) => write!(
                formatter,
                "`{token}` is not a PSB key (use 0x-prefixed hex or decimal)"
            ),
            Self::NotUnicode => write!(formatter, "the PSB key configuration is not valid Unicode"),
        }
    }
}

impl std::error::Error for PsbKeyError {}

/// The cipher layer's configuration, as an engine game profile would carry it.
///
/// Keys are ordered: the store tries them in registration order, so a title
/// with several known candidates can prefer the most likely one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PsbConfig {
    keys: Vec<PsbKey>,
}

impl PsbConfig {
    /// No configured keys: every keyed document keeps the key-required
    /// diagnosis.
    pub const fn new() -> Self {
        Self { keys: Vec::new() }
    }

    /// Adds one candidate key.
    #[must_use]
    pub fn with_key(mut self, key: PsbKey) -> Self {
        self.keys.push(key);
        self
    }

    /// Parses a key list: comma- or whitespace-separated tokens, each either
    /// `0x`-prefixed hexadecimal or decimal.
    pub fn parse(text: &str) -> Result<Self, PsbKeyError> {
        let mut config = Self::new();
        for token in text
            .split(|character: char| character == ',' || character.is_whitespace())
            .filter(|token| !token.is_empty())
        {
            config = config.with_key(PsbKey::parse(token)?);
        }
        Ok(config)
    }

    /// The interim runtime source: the `KRKR_PSB_KEYS` environment variable.
    ///
    /// An unset or empty variable means no configured keys; malformed content
    /// is reported so the caller can log it.  This exists only until the engine
    /// profile carries a PSB configuration of its own.
    pub fn from_environment() -> Result<Self, PsbKeyError> {
        match std::env::var(PSB_KEYS_ENV_VAR) {
            Ok(text) => Self::parse(&text),
            Err(std::env::VarError::NotPresent) => Ok(Self::new()),
            Err(std::env::VarError::NotUnicode(_)) => Err(PsbKeyError::NotUnicode),
        }
    }
}

/// The runtime key store: an ordered list of candidate keys a session may use.
///
/// The store never discovers keys and never decides on its own whether a
/// document decoded: it offers one repair per candidate, and the reader's
/// admission (the same one a plaintext document must pass) accepts or rejects
/// each.  A wrong key therefore costs one failed parse and cannot turn a
/// damaged document into a false success.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PsbKeyStore {
    keys: Vec<PsbKey>,
}

impl PsbKeyStore {
    /// A store with no keys: the cipher path is never entered.
    pub const fn new() -> Self {
        Self { keys: Vec::new() }
    }

    /// The store for a configuration.
    pub fn from_config(config: &PsbConfig) -> Self {
        Self {
            keys: config.keys.clone(),
        }
    }

    /// Yields one decrypted copy of `bytes` per candidate key, in registration
    /// order; candidates whose repair cannot be positioned (a header the cipher
    /// cannot read, a body range outside the document) are skipped.
    ///
    /// The results are proposals, not facts: the caller must run each through
    /// the same admission a plaintext document faces.
    pub fn decrypt_candidates<'a>(
        &'a self,
        flags: u16,
        bytes: &'a [u8],
    ) -> impl Iterator<Item = Vec<u8>> + 'a {
        self.keys
            .iter()
            .filter_map(move |key| PsbCipher::new(*key).decrypt_document(flags, bytes).ok())
    }
}

/// The E-mote cipher for one key: a fresh XOR stream per document, applied
/// over the ranges the document's flags select.
#[derive(Clone, Copy, Debug)]
pub struct PsbCipher {
    key: PsbKey,
}

impl PsbCipher {
    pub const fn new(key: PsbKey) -> Self {
        Self { key }
    }

    /// Repairs `bytes` the way the reference decoders do for `flags`.
    ///
    /// The header range is applied first when its bit is set; the body range is
    /// then computed from the *repaired* header, because while the header bit
    /// is in effect the stored offsets are themselves ciphertext
    /// (`…/psb/mod.rs:853-881`).
    pub fn decrypt_document(&self, flags: u16, bytes: &[u8]) -> Result<Vec<u8>, PsbCipherError> {
        let version = document_version(bytes)?;
        let mut plain = bytes.to_vec();
        let mut stream = EmoteStream::new(self.key);
        if flags & PSB_FLAG_HEADER_ENCRYPTED != 0 {
            let range = header_cipher_range(version);
            let Some(head) = plain.get_mut(range.clone()) else {
                return Err(PsbCipherError::Truncated);
            };
            stream.apply(head);
        }
        if body_is_ciphered(flags, &plain) {
            let range = body_cipher_range(&plain)?;
            stream.apply(&mut plain[range]);
        }
        Ok(plain)
    }

    /// The inverse of [`PsbCipher::decrypt_document`], for tests and tooling:
    /// the same steps with the body range read from the plaintext header
    /// (`xp3-brute`'s `encrypt_psb`, `src/encoder/psb.rs:989-1021`).
    #[allow(dead_code, reason = "cipher scheme inverse, exercised by tests")]
    pub fn encrypt_document(&self, flags: u16, bytes: &[u8]) -> Result<Vec<u8>, PsbCipherError> {
        let version = document_version(bytes)?;
        let header = flags & PSB_FLAG_HEADER_ENCRYPTED != 0;
        let mut cipher = bytes.to_vec();
        if header && cipher.get(header_cipher_range(version)).is_none() {
            return Err(PsbCipherError::Truncated);
        }
        // The body range must be read from the plaintext header: once the
        // header is ciphered its offset fields are ciphertext.
        let body = if body_is_ciphered(flags, bytes) {
            Some(body_cipher_range(bytes)?)
        } else {
            None
        };
        let mut stream = EmoteStream::new(self.key);
        if header {
            stream.apply(&mut cipher[header_cipher_range(version)]);
        }
        if let Some(range) = body {
            stream.apply(&mut cipher[range]);
        }
        Ok(cipher)
    }
}

/// Why a document could not be run through the cipher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PsbCipherError {
    /// The buffer is too short for the header bytes its version and flags
    /// select.
    Truncated,
    /// The repaired header's body range is not inside the document, so the
    /// stream cannot be positioned over it (`…/psb/mod.rs:875-880`).
    InvalidBodyRange {
        names: usize,
        chunk_offsets: usize,
        len: usize,
    },
}

impl fmt::Display for PsbCipherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(formatter, "PSB is too short for its encrypted header"),
            Self::InvalidBodyRange {
                names,
                chunk_offsets,
                len,
            } => write!(
                formatter,
                "PSB body range {names:#x}..{chunk_offsets:#x} is outside the {len}-byte document"
            ),
        }
    }
}

impl std::error::Error for PsbCipherError {}

/// The scheme's byte-oriented XOR stream: six words of state, one keystream
/// byte at a time, continuing across calls.
///
/// Faithful port of eluna's `PsbDecryptor` (`…/psb/mod.rs:891-919`, itself
/// cross-checked against GARbro's `ArcPSB.cs:688-706` and FreeMote's stream
/// context): when the reservoir word `state[4]` runs out of bytes the state
/// shifts, `state[3]` becomes the next dword and the reservoir is refilled
/// with it.  `state[5]` is never read in this direction.
struct EmoteStream {
    state: [u32; 6],
}

impl EmoteStream {
    fn new(key: PsbKey) -> Self {
        Self {
            state: [
                EMOTE_SEED_WORDS[0],
                EMOTE_SEED_WORDS[1],
                EMOTE_SEED_WORDS[2],
                key.word,
                0,
                0,
            ],
        }
    }

    fn apply(&mut self, bytes: &mut [u8]) {
        for byte in bytes {
            if self.state[4] == 0 {
                let previous = self.state[3];
                let mixed = self.state[0] ^ self.state[0].wrapping_shl(11);
                self.state[0] = self.state[1];
                self.state[1] = self.state[2];
                let next = mixed ^ previous ^ ((mixed ^ (previous >> 11)) >> 8);
                self.state[2] = previous;
                self.state[3] = next;
                self.state[4] = next;
            }
            *byte ^= self.state[4] as u8;
            self.state[4] >>= 8;
        }
    }
}

/// The header bytes the scheme ciphers for a version.
///
/// v2 protects its eight offset fields (`0x20` bytes); v3 adds the Adler-32
/// word, which must be decrypted with the header it protects or the reader's
/// checksum gate could never pass a repaired header, and v4 its three extra
/// resource fields (FreeMote `PsbHeader.cs:180-185`).
fn header_cipher_range(version: u16) -> Range<usize> {
    let length = match version {
        ..=2 => 0x20,
        3 => 0x24,
        _ => 0x30,
    };
    0x08..0x08 + length
}

/// Is the body stream needed for this document?
///
/// The body bit asks for it; a root byte that is not the object marker asks
/// for it too — the reference's implicit case for old v2 documents
/// (`…/psb/mod.rs:869-870`, GARbro forces the same at `ArcPSB.cs:358-359`).
/// Unlike the reference, the implicit case is honoured only while a cipher bit
/// is claimed somewhere: Phase 1 never runs an unflagged document through a
/// cipher, and this layer keeps that contract even when called directly.
/// `bytes` must be the buffer whose header is in effect — the repaired one
/// when decrypting.
fn body_is_ciphered(flags: u16, bytes: &[u8]) -> bool {
    flags & PSB_FLAG_ENCRYPTION_HINT != 0
        && (flags & PSB_FLAG_BODY_ENCRYPTED != 0 || root_code(bytes) != Some(PSB_ROOT_OBJECT))
}

/// The body range: names trie through the string tables, ending where the
/// resource tables begin (`…/psb/mod.rs:872-881`).
fn body_cipher_range(bytes: &[u8]) -> Result<Range<usize>, PsbCipherError> {
    let names = read_u32(bytes, HEADER_NAMES_FIELD).ok_or(PsbCipherError::Truncated)? as usize;
    let chunk_offsets =
        read_u32(bytes, HEADER_CHUNK_OFFSETS_FIELD).ok_or(PsbCipherError::Truncated)? as usize;
    if names > chunk_offsets || chunk_offsets > bytes.len() {
        return Err(PsbCipherError::InvalidBodyRange {
            names,
            chunk_offsets,
            len: bytes.len(),
        });
    }
    Ok(names..chunk_offsets)
}

/// The version lives in the first eight bytes the cipher never touches.
fn document_version(bytes: &[u8]) -> Result<u16, PsbCipherError> {
    read_u16(bytes, 4).ok_or(PsbCipherError::Truncated)
}

/// The root value's marker byte, if the document is large enough to have one.
/// `None` — an offset outside the document — counts as "not a plain object",
/// which is also how the reference treats it (`…/psb/mod.rs:887-889`).
fn root_code(bytes: &[u8]) -> Option<u8> {
    let root = read_u32(bytes, HEADER_ROOT_FIELD)? as usize;
    bytes.get(root).copied()
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keystream the reference's algorithm produces for these keys, taken
    /// from a verbatim transcription of GARbro's `Decrypt`
    /// (`ArcFormats/Emote/ArcPSB.cs:688-706`) and cross-checked against
    /// `eluna`'s `PsbDecryptor` by round-tripping a document through
    /// `PsbFile::parse_normalized` (see the mission record; eluna is not a
    /// dependency of this crate).
    const REFERENCE_KEYSTREAM: &[(u32, &str)] = &[
        (
            0x39D4_4B15,
            "5f1a3ee0c04ecc271f168da9d86c9a3236646622f4949967f64b65a64e0f784743942e564694bbfdfc4a0d71ec3988f6f22726d494801ef5616e5fee1f6daf59",
        ),
        (
            0x0102_0304,
            "5455e8d8d1061a1f14595b9118725080b3d5881abd746bd5fd04b39e8165709dd11e09c1d154494f2925db49fe1d99574a7eefde9a5605dbbed2674bc1408ed4",
        ),
        (
            0xDEAD_BEEF,
            "4af347073abbb5c00afff44efd6a1dfdb70bf8c55914f9a8e3a21c41bf624057537ee0528af3db3288447496639975c298753693d7839c7e88d8f84a35ee2824",
        ),
    ];

    /// A buffer shaped like a PSB header the cipher can read: the fields it
    /// consults are filled in, and a root object marker is placed at
    /// `root_offset` when that is inside the buffer.
    fn shaped_document(
        version: u16,
        flags: u16,
        names: u32,
        chunk_offsets: u32,
        root: u32,
    ) -> Vec<u8> {
        let mut bytes = vec![0_u8; 0x60];
        bytes[..4].copy_from_slice(b"PSB\0");
        bytes[4..6].copy_from_slice(&version.to_le_bytes());
        bytes[6..8].copy_from_slice(&flags.to_le_bytes());
        bytes[HEADER_NAMES_FIELD..HEADER_NAMES_FIELD + 4].copy_from_slice(&names.to_le_bytes());
        bytes[HEADER_CHUNK_OFFSETS_FIELD..HEADER_CHUNK_OFFSETS_FIELD + 4]
            .copy_from_slice(&chunk_offsets.to_le_bytes());
        bytes[HEADER_ROOT_FIELD..HEADER_ROOT_FIELD + 4].copy_from_slice(&root.to_le_bytes());
        if let Some(marker) = bytes.get_mut(root as usize) {
            *marker = PSB_ROOT_OBJECT;
        }
        bytes
    }

    fn hex_digits(text: &str) -> Vec<u8> {
        (0..text.len() / 2)
            .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn emote_stream_matches_the_reference_keystream() {
        for (word, expected) in REFERENCE_KEYSTREAM {
            let mut keystream = vec![0_u8; expected.len() / 2];
            EmoteStream::new(PsbKey::emote(*word)).apply(&mut keystream);
            assert_eq!(keystream, hex_digits(expected), "key {word:#010x}");
        }
    }

    #[test]
    fn header_cipher_range_matches_the_reference_lengths() {
        assert_eq!(header_cipher_range(2), 0x08..0x28);
        assert_eq!(header_cipher_range(3), 0x08..0x2c);
        assert_eq!(header_cipher_range(4), 0x08..0x38);
    }

    #[test]
    fn body_range_is_names_through_chunk_offsets() {
        let bytes = shaped_document(3, PSB_FLAG_BODY_ENCRYPTED, 0x30, 0x40, 0x50);
        assert_eq!(body_cipher_range(&bytes), Ok(0x30..0x40));
        // An empty range is legal (no names/strings to protect).
        let empty = shaped_document(3, PSB_FLAG_BODY_ENCRYPTED, 0x30, 0x30, 0x50);
        assert_eq!(body_cipher_range(&empty), Ok(0x30..0x30));
    }

    #[test]
    fn invalid_body_ranges_are_rejected() {
        let inverted = shaped_document(3, PSB_FLAG_BODY_ENCRYPTED, 0x50, 0x40, 0x20);
        assert_eq!(
            body_cipher_range(&inverted),
            Err(PsbCipherError::InvalidBodyRange {
                names: 0x50,
                chunk_offsets: 0x40,
                len: 0x60,
            })
        );
        let past_end = shaped_document(3, PSB_FLAG_BODY_ENCRYPTED, 0x30, 0x70, 0x40);
        assert_eq!(
            body_cipher_range(&past_end),
            Err(PsbCipherError::InvalidBodyRange {
                names: 0x30,
                chunk_offsets: 0x70,
                len: 0x60,
            })
        );
    }

    #[test]
    fn header_encryption_covers_exactly_the_reference_range() {
        let key = PsbKey::emote(0x0102_0304);
        let cipher = PsbCipher::new(key);
        for (version, end) in [(2_u16, 0x28_usize), (3, 0x2c)] {
            // The body is not ciphered: the body bit is clear and the root
            // byte at 0x50 is a plain object marker.
            let plain = shaped_document(version, PSB_FLAG_HEADER_ENCRYPTED, 0x30, 0x40, 0x50);
            let ciphered = cipher
                .encrypt_document(PSB_FLAG_HEADER_ENCRYPTED, &plain)
                .expect("shaped header encrypts");
            let mut expected = plain.clone();
            EmoteStream::new(key).apply(&mut expected[0x08..end]);
            assert_eq!(ciphered, expected, "version {version}");
            assert_eq!(
                ciphered[..0x08],
                plain[..0x08],
                "magic, version, flags stay clear"
            );
            assert_eq!(
                ciphered[end..],
                plain[end..],
                "nothing behind the header moves"
            );
            assert_eq!(
                cipher
                    .decrypt_document(PSB_FLAG_HEADER_ENCRYPTED, &ciphered)
                    .expect("shaped header decrypts"),
                plain,
                "version {version}"
            );
        }
    }

    #[test]
    fn body_encryption_covers_exactly_the_reference_range() {
        let key = PsbKey::emote(0x0102_0304);
        let cipher = PsbCipher::new(key);
        let plain = shaped_document(3, PSB_FLAG_BODY_ENCRYPTED, 0x30, 0x40, 0x50);
        // No header bit: the stream begins at the body, exactly as if the
        // encrypted document had no header to consume first.
        let ciphered = cipher
            .encrypt_document(PSB_FLAG_BODY_ENCRYPTED, &plain)
            .expect("shaped body encrypts");
        let mut expected = plain.clone();
        EmoteStream::new(key).apply(&mut expected[0x30..0x40]);
        assert_eq!(ciphered, expected);
        assert_eq!(ciphered[..0x30], plain[..0x30]);
        assert_eq!(ciphered[0x40..], plain[0x40..]);
        assert_eq!(
            cipher
                .decrypt_document(PSB_FLAG_BODY_ENCRYPTED, &ciphered)
                .expect("shaped body decrypts"),
            plain
        );
    }

    #[test]
    fn implicit_body_encryption_follows_a_non_object_root() {
        // The reference also repairs the body when a cipher-claiming document's
        // root byte is not an object marker, even with the body bit clear — the
        // old v2 shape.  The stream must continue across the two ranges.
        let key = PsbKey::emote(0x0102_0304);
        let cipher = PsbCipher::new(key);
        let mut plain = shaped_document(3, PSB_FLAG_HEADER_ENCRYPTED, 0x30, 0x40, 0x50);
        plain[0x50] = 0x00;
        let ciphered = cipher
            .encrypt_document(PSB_FLAG_HEADER_ENCRYPTED, &plain)
            .expect("shaped document encrypts");
        let mut expected = plain.clone();
        let mut stream = EmoteStream::new(key);
        stream.apply(&mut expected[0x08..0x2c]);
        stream.apply(&mut expected[0x30..0x40]);
        assert_eq!(ciphered, expected);
        assert_eq!(
            cipher
                .decrypt_document(PSB_FLAG_HEADER_ENCRYPTED, &ciphered)
                .expect("shaped document decrypts"),
            plain
        );
    }

    #[test]
    fn encrypt_and_decrypt_are_inverse_for_each_flag_shape() {
        let cipher = PsbCipher::new(PsbKey::emote(0x0102_0304));
        let plain = shaped_document(3, 0, 0x30, 0x40, 0x50);
        // No flags means no cipher: the document passes through untouched.
        assert_eq!(
            cipher.decrypt_document(0, &plain).unwrap(),
            plain,
            "an unflagged document is never ciphered"
        );
        assert_eq!(cipher.encrypt_document(0, &plain).unwrap(), plain);
        for flags in [
            PSB_FLAG_HEADER_ENCRYPTED,
            PSB_FLAG_BODY_ENCRYPTED,
            PSB_FLAG_HEADER_ENCRYPTED | PSB_FLAG_BODY_ENCRYPTED,
        ] {
            let ciphered = cipher
                .encrypt_document(flags, &plain)
                .expect("shaped document encrypts");
            assert_ne!(ciphered, plain, "flags {flags:#06x}");
            assert_eq!(
                cipher.decrypt_document(flags, &ciphered).unwrap(),
                plain,
                "flags {flags:#06x}"
            );
        }
    }

    #[test]
    fn short_documents_are_rejected() {
        let cipher = PsbCipher::new(PsbKey::emote(0x0102_0304));
        // Too short even for the version word.
        assert_eq!(
            cipher.decrypt_document(0, &[0_u8; 4]),
            Err(PsbCipherError::Truncated)
        );
        // Long enough to be read, too short for the header bytes the bit
        // selects.
        let plain = shaped_document(3, PSB_FLAG_HEADER_ENCRYPTED, 0x30, 0x40, 0x50);
        assert_eq!(
            cipher.encrypt_document(PSB_FLAG_HEADER_ENCRYPTED, &plain[..0x20]),
            Err(PsbCipherError::Truncated)
        );
        assert_eq!(
            cipher.decrypt_document(PSB_FLAG_HEADER_ENCRYPTED, &plain[..0x20]),
            Err(PsbCipherError::Truncated)
        );
    }

    #[test]
    fn configured_keys_are_tried_in_registration_order() {
        let first = PsbKey::emote(0x0000_0001);
        let second = PsbKey::emote(0xffff_fffe);
        // A body-only ciphertext: every configured key can be streamed over the
        // body range, so the store's order is observable in the candidates.
        let plain = shaped_document(3, PSB_FLAG_BODY_ENCRYPTED, 0x30, 0x40, 0x50);
        let ciphered = PsbCipher::new(first)
            .encrypt_document(PSB_FLAG_BODY_ENCRYPTED, &plain)
            .expect("shaped body encrypts");

        assert_eq!(
            PsbKeyStore::new()
                .decrypt_candidates(PSB_FLAG_BODY_ENCRYPTED, &ciphered)
                .count(),
            0
        );
        let store = PsbKeyStore::from_config(&PsbConfig::new().with_key(first).with_key(second));
        let candidates: Vec<_> = store
            .decrypt_candidates(PSB_FLAG_BODY_ENCRYPTED, &ciphered)
            .collect();
        assert_eq!(
            candidates,
            vec![
                PsbCipher::new(first)
                    .decrypt_document(PSB_FLAG_BODY_ENCRYPTED, &ciphered)
                    .unwrap(),
                PsbCipher::new(second)
                    .decrypt_document(PSB_FLAG_BODY_ENCRYPTED, &ciphered)
                    .unwrap(),
            ]
        );
        // The right key's candidate is the plaintext document; a wrong key's is
        // only what the wrong stream produced — accepting it is the reader's
        // call, not the store's.
        assert_eq!(candidates[0], plain);
        assert_ne!(candidates[1], plain);
    }

    #[test]
    fn candidates_that_cannot_be_positioned_are_skipped() {
        let store = PsbKeyStore::from_config(
            &PsbConfig::new()
                .with_key(PsbKey::emote(0x0102_0304))
                .with_key(PsbKey::emote(0x0506_0708)),
        );
        // The body range lies outside the document, so neither key can be
        // streamed over it.
        let broken = shaped_document(3, PSB_FLAG_BODY_ENCRYPTED, 0x30, 0x70, 0x40);
        assert_eq!(
            store
                .decrypt_candidates(PSB_FLAG_BODY_ENCRYPTED, &broken)
                .count(),
            0
        );
    }

    #[test]
    fn config_parses_hexadecimal_and_decimal_lists() {
        let config = PsbConfig::parse("0x12ab, 0xFFFFFFFF\n250\t").unwrap();
        assert_eq!(
            config.keys,
            vec![
                PsbKey::emote(0x12ab),
                PsbKey::emote(u32::MAX),
                PsbKey::emote(250)
            ]
        );
        assert_eq!(PsbConfig::parse("").unwrap(), PsbConfig::new());
        assert_eq!(
            PsbConfig::parse("0xzz").unwrap_err(),
            PsbKeyError::InvalidToken("0xzz".to_string())
        );
        assert_eq!(
            PsbConfig::parse("0x1234 0x100000000").unwrap_err(),
            PsbKeyError::InvalidToken("0x100000000".to_string())
        );
        assert_eq!(PsbKey::emote(0xab).to_string(), "0x000000ab");
    }
}
