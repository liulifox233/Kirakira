//! `wutcwf.dll` — the TCWF (`.tcw`) compressed-wave decoder.
//!
//! Reference: krkrz SamplePlugin `wutcwf/WUMainUnit.cpp` (579 lines) with its
//! MIDL surface `tvpsnd.h` / `tvpsnd.c`; the Kirikiroid2 port
//! (`src/plugins/wutcwf.cpp`, 377 lines) re-hosts the same decoder and is used
//! below as the cross-check for how the engine consumes it. Line numbers in
//! this module are `WUMainUnit.cpp`'s unless noted.
//!
//! # What the DLL is
//!
//! **No TJS surface.** The module exports `GetModuleInstance(ITSSModule**,
//! ITSSStorageProvider*, IStream*, HWND)` (`:570-577`); the plugin manager
//! loads it by `GetProcAddress`. It answers `GetSupportExts` with `.tcw` /
//! "TCWF ファイル" (`:206-213`) and hands `GetMediaInstance` an
//! `ITSSWaveDecoder` (`:225-239`). Kirikiroid2's port registers the same
//! decoder through `TVPRegisterWaveDecoderCreator` (`wutcwf.cpp:99-116,
//! 371-377`), i.e. the wave decoder registry — the codec is selected by the
//! media module, never by a script call.
//!
//! # The container
//!
//! All structures are `#pragma pack(push,1)` (`:15-41`):
//!
//! * **File header**, 24 bytes (`TTCWFHeader`): `mark[6] = "TCWF0\x1a"`,
//!   `channels`, `reserved`, then three i32s — `frequency` (sample rate),
//!   `numblocks`, `bytesperblock`, `samplesperblock`.
//! * **Block header**, 32 bytes (`TTCWBlockHeader`): `ms_sample0`,
//!   `ms_sample1`, `ms_idelta` (i16), `ms_bpred`, `ima_stepindex` (u8), then
//!   six `{u16 pos; i16 revise;}` peak revisions.
//! * **Block data**: `bytesperblock - 32` bytes, each carrying two nibbles.
//!
//! A "superblock" is `channels` consecutive per-channel blocks
//! (`:32` — "ステレオの場合はブロックが右・左の順に２つ続く"), so channel `c`'s
//! frame `k` of block `b` lives at
//! `data_start + (b*channels + c)*bytesperblock` and the decoded samples are
//! interleaved at stride `channels` (`Samples[k*numchans + chan]`, `:543`).
//!
//! # What one block decodes to
//!
//! Two ADPCM layers are summed, then the peaks patch the result:
//!
//! * **MS ADPCM** on the **low** nibble (`:483-506`): `predict =
//!   (s[k-1]*AdaptCoeff1[bpred] + s[k-2]*AdaptCoeff2[bpred]) >> 8`,
//!   `idelta` adapted by `AdaptationTable[code]`, floored at 16, and
//!   `current = code*idelta_save + predict` clamped to i16. Codes 8-15 are
//!   negative (two's-complement fold, `:493`). `bpred >= 7` means "probably
//!   lost sync" and fails the block (`:478`).
//! * **IMA ADPCM** on the **high** nibble (`:508-548`), accumulated on top of
//!   the MS result (`n = Samples[k] + current`, `:543-547`): the step from
//!   `ima_step_size[stepindex]`, the usual 1/8-bit differential, and the
//!   index clamped to `[0, 88]`. Its predictor starts at **0**, not at
//!   `ms_sample1` (`:511`).
//! * **Peak revisions** (`:550-563`): six `{pos, revise}` entries, each
//!   subtracting `revise` from `Samples[pos]` — the encoder's escape hatch for
//!   overshoots the ADPCM layers cannot represent.
//!
//! # Streaming and format
//!
//! `GetFormat` answers 16-bit, `dwSeekable = 2`, and leaves
//! `ui64TotalSamples` / `dwTotalTime` at 0 (`:298-303, 427-433`; `numblocks` is
//! carried but never used by the decoder). `Render(buf, bufsamplelen, rendered,
//! status)` (`:305-340`) decodes `bufsamplelen` interleaved sample *frames*:
//! `rendered` is what it wrote and `status` is 1 when the request was filled,
//! 0 when a block read ended the stream first. `SetPosition(samplepos)`
//! (`:342-385`) seeks to a frame: block `samplepos/samplesperblock`, then
//! `remnant` frames into it.
//!
//! # How this port maps it
//!
//! * [`TcwfDecoder`] is that decoder over an in-memory stream: same header and
//!   block layouts, same tables, same two-layer accumulation, same peak
//!   pass, same `Render`/`SetPosition` semantics. Header and block reads that
//!   the reference performs with `IStream::Read` become slice bounds checks.
//! * The reference's failures are the port's errors: a missing mark or a short
//!   header fails `open` (`:413-415`), `bpred >= 7` fails the block (`:478`),
//!   and a short read fails it too (`:466-471`). Where the reference has no
//!   bounds at all the port adds one and names it: a peak `pos` outside the
//!   block writes out of bounds in the reference (`:552-563`); a zero channel
//!   count spins its `Render` loop forever (`:311-333`); a `bytesperblock`
//!   smaller than the 32-byte block header makes its size computation
//!   underflow (`:469-471`); and a block data area shorter than the
//!   `samplesperblock - 2` nibble bytes the loops consume reads past the
//!   buffer (`:486-548`).
//! * The stream's end is a *status*, not an error: a render that runs out of
//!   complete blocks reports `status = 0` (or fills what it can), exactly like
//!   the reference. Bytes left over after the last complete superblock are a
//!   truncated block and are named as such instead of silently skipped — the
//!   reference cannot tell the two apart because its reads just come up short.
//! * **Not wired**: nothing routes a `.tcw` storage to this decoder yet. The
//!   reference is a TVP Sound System media module, i.e. it plugs in *before*
//!   the audio loader; this engine's loader is `krkr-audio`'s kira path
//!   (`crates/krkr-audio/src/lib.rs`, `load_static_sound`), which has no
//!   extension→decoder registry and is outside this module's scope. Until
//!   that seam exists `WaveSoundBuffer.open("...tcw")` still fails in kira —
//!   the decoder below is complete and tested, but unreachable from a game.

// Nothing in a production build calls the decoder yet — the audio-loader seam
// the module header describes is a follow-up — so its items are dead code
// until the wiring lands. `dead_code` stays on in test builds.
#![cfg_attr(not(test), allow(dead_code))]

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "TCWF (*.tcw) wave decoder (TSS media module, no TJS surface)",
    notes: "The decoder is implemented in full and tested against hand-computed fixtures (24-byte TCWF0 header, MS-ADPCM low nibbles + additive IMA high nibbles + the six peak revisions, Render/SetPosition semantics, named malformed-input errors). It is not reachable from a game yet: the reference plugs in as a wave-decoder media module, and this engine's audio loader (krkr-audio/kira) has no extension-to-decoder seam. A .tcw open still fails there.",
    install: |engine| engine.register_plugin(WutcwfPlugin),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what a game's
/// `Plugins.link("wutcwf.dll")` resolves through [`crate::catalog`].
pub(crate) const NAME: &str = "wutcwf.dll";

/// `GetSupportExts`' answer (`WUMainUnit.cpp:206-213`): one entry only —
/// `index >= 1` is `S_FALSE`.
pub(crate) struct MediaSupport {
    /// The extension the module answers for, `.` included.
    pub(crate) extension: &'static str,
    /// `GetSupportExts`' media short name, verbatim.
    pub(crate) media_type: &'static str,
    /// `GetModuleDescription`'s text (`:200-204`).
    pub(crate) description: &'static str,
}

pub(crate) const MEDIA_SUPPORT: MediaSupport = MediaSupport {
    extension: ".tcw",
    media_type: "TCWF ファイル",
    description: "TVP's Compressed Wave Format decoder (*.tcw)",
};

pub struct WutcwfPlugin;

impl KrkrPlugin for WutcwfPlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The reference registers no TJS surface at all: it is a TSS media
        // module answering for `.tcw`. There is nowhere to register it in this
        // engine yet, so the module reports the decoder it carries instead of
        // pretending the extension plays.
        runtime.host_mut().log(&format!(
            "wutcwf: {} — the TCWF decoder ({}, 16-bit PCM, seekable) is implemented and tested \
             but not registered with the audio loader: krkr-audio's kira path has no \
             extension-to-decoder seam, so `.tcw` playback still fails there.",
            MEDIA_SUPPORT.description, MEDIA_SUPPORT.extension,
        ));
        Ok(())
    }
}

// ------------------------------------------------------------------ layout

/// `sizeof(TTCWFHeader)` with the reference's 1-byte packing (`:15-26`).
const HEADER_LEN: usize = 24;
/// `sizeof(TTCWBlockHeader)` with the same packing (`:32-40`).
const BLOCK_HEADER_LEN: usize = 32;
/// `Header.mark` (`:19`).
const MARK: &[u8; 6] = b"TCWF0\x1a";
/// `TTCWBlockHeader::peaks[6]` (`:39`).
const PEAK_COUNT: usize = 6;

/// `AdaptationTable` (`:44-48`).
const ADAPTATION_TABLE: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];
/// `AdaptCoeff1` (`:50-52`).
const ADAPT_COEFF1: [i32; 7] = [256, 512, 0, 192, 240, 460, 392];
/// `AdaptCoeff2` (`:54-56`).
const ADAPT_COEFF2: [i32; 7] = [0, -256, 0, 64, 0, -208, -232];
/// `ima_index_adjust` (`:58-63`).
const IMA_INDEX_ADJUST: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
/// `ima_step_size` (`:65-73`).
const IMA_STEP_SIZE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// Everything the decoder reads out of `TTCWFHeader` (`:17-26`, `:427-433`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TcwfInfo {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    /// Always 16 (`:430`).
    pub(crate) bits_per_sample: u16,
    /// Always 2 (`:431`).
    pub(crate) seekable: u16,
    /// `numblocks`, carried as metadata: the reference decoder never reads it.
    pub(crate) num_blocks: u32,
    pub(crate) bytes_per_block: u32,
    pub(crate) samples_per_block: u32,
}

/// Why a TCWF stream was refused or failed mid-decode.
///
/// The first two are the reference's own `E_FAIL` paths; the block-level ones
/// are the failures its `ReadBlock` reports as `false`; the last four name
/// malformations the reference has no check for and would handle by reading or
/// writing memory it does not own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TcwfError {
    /// The 6-byte `TCWF0\x1a` mark is missing (`:415`).
    BadMark,
    /// `Read(&Header, sizeof(Header))` came up short (`:413-414`).
    TruncatedHeader,
    /// `channels == 0`: the reference's `Render` channel loops never advance
    /// and it reads blocks until the stream ends (`:311-333`).
    NoChannels,
    /// Fewer than two samples per block: `Samples[0]`/`Samples[1]` are the
    /// MS-ADPCM seeds and `Samples[1*channels]` already writes past the
    /// block's allocation (`:474-475`).
    SamplesPerBlockTooSmall { samples_per_block: u32 },
    /// `bytesperblock` cannot hold the 32-byte block header plus the
    /// `samplesperblock - 2` nibble bytes the two decode loops consume
    /// (`:469-471`, `:486-548`).
    BytesPerBlockTooSmall {
        bytes_per_block: u32,
        samples_per_block: u32,
    },
    /// `ms_bpred >= 7`: the reference's "probably lost sync" (`:478`).
    SyncLost { block: u64 },
    /// A block (or the tail of the last one) is not fully present (`:466-471`).
    TruncatedBlock { block: u64 },
    /// A peak revision's `pos` lies outside the block (`:552-563`); the
    /// reference writes `Samples[pos*numchans]` unchecked.
    PeakOutOfRange { block: u64, pos: u32 },
}

impl std::fmt::Display for TcwfError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMark => write!(formatter, "TCWF0 mark missing"),
            Self::TruncatedHeader => write!(formatter, "truncated TCWF header"),
            Self::NoChannels => write!(formatter, "TCWF header declares zero channels"),
            Self::SamplesPerBlockTooSmall { samples_per_block } => write!(
                formatter,
                "TCWF header declares {samples_per_block} samples per block (needs >= 2)"
            ),
            Self::BytesPerBlockTooSmall {
                bytes_per_block,
                samples_per_block,
            } => write!(
                formatter,
                "TCWF header declares {bytes_per_block} bytes per block, too small for a 32-byte \
                 block header and {} nibble bytes",
                samples_per_block.saturating_sub(2)
            ),
            Self::SyncLost { block } => {
                write!(formatter, "TCWF block {block} lost sync (bpred >= 7)")
            }
            Self::TruncatedBlock { block } => write!(formatter, "TCWF block {block} is truncated"),
            Self::PeakOutOfRange { block, pos } => write!(
                formatter,
                "TCWF block {block} has a peak revision at sample {pos}, outside the block"
            ),
        }
    }
}

/// One `ITSSWaveDecoder::Render` outcome (`WUMainUnit.cpp:305-340`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TcwfRender {
    /// Frames written (`*rendered`).
    pub(crate) rendered: usize,
    /// The reference's `*status`: 1 when the request was filled, 0 when the
    /// stream ended first.
    pub(crate) status: u16,
}

// ----------------------------------------------------------------- decoder

/// The reference's `TCWFDecoder` (`WUMainUnit.cpp:110-144`) over a borrowed
/// in-memory stream.
pub(crate) struct TcwfDecoder<'a> {
    /// The whole stream, header included; `StreamPos` is an offset into it.
    data: &'a [u8],
    info: TcwfInfo,
    /// `StreamPos`, where the next block read starts.
    stream_pos: usize,
    /// `Pos`, the frame the next `render` starts at.
    position: u64,
    /// `Samples`, the last decoded superblock (`samplesperblock * channels`),
    /// interleaved per frame.
    samples: Vec<i16>,
    /// `SamplePos`, the next frame inside `samples`.
    sample_pos: usize,
    /// `BufferRemain`, frames left in `samples`.
    buffer_remain: usize,
    /// The block the decoded superblock came from, for error reporting.
    block_index: u64,
}

impl<'a> TcwfDecoder<'a> {
    /// `TCWFDecoder::Open` (`:387-436`): the mark check and the format.
    ///
    /// The four guards with no reference counterpart are the module's
    /// documented malformations ([`TcwfError`]).
    pub(crate) fn open(data: &'a [u8]) -> std::result::Result<Self, TcwfError> {
        if data.len() < HEADER_LEN {
            return Err(TcwfError::TruncatedHeader);
        }
        if &data[..6] != MARK {
            return Err(TcwfError::BadMark);
        }
        let channels = data[6] as u16;
        let sample_rate = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let num_blocks = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let bytes_per_block = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        let samples_per_block = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);

        if channels == 0 {
            return Err(TcwfError::NoChannels);
        }
        if samples_per_block < 2 {
            return Err(TcwfError::SamplesPerBlockTooSmall { samples_per_block });
        }
        let nibble_bytes = samples_per_block - 2;
        let header_len = BLOCK_HEADER_LEN as u32;
        if bytes_per_block < header_len || bytes_per_block - header_len < nibble_bytes {
            return Err(TcwfError::BytesPerBlockTooSmall {
                bytes_per_block,
                samples_per_block,
            });
        }

        Ok(Self {
            data,
            info: TcwfInfo {
                sample_rate,
                channels,
                // `TSSFormat.dwBitsPerSample = 16` (`:430`).
                bits_per_sample: 16,
                // `TSSFormat.dwSeekable = 2` (`:431`).
                seekable: 2,
                num_blocks,
                bytes_per_block,
                samples_per_block,
            },
            stream_pos: HEADER_LEN,
            position: 0,
            samples: Vec::new(),
            sample_pos: 0,
            buffer_remain: 0,
            block_index: 0,
        })
    }

    /// `GetFormat`'s `TSSWaveFormat` (`:298-303, 427-433`).
    pub(crate) fn info(&self) -> TcwfInfo {
        self.info
    }

    /// `Pos`: the frame the next rendered sample comes from (`:119`).
    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    fn channels(&self) -> usize {
        self.info.channels as usize
    }

    fn samples_per_block(&self) -> usize {
        self.info.samples_per_block as usize
    }

    /// Byte offset of block `index` (`newbytepos = block*bytesperblock*channels`,
    /// `:359`, with `DataStart` = the end of the 24-byte header, `:421`).
    /// `None` when the arithmetic would leave the address space — a seek far
    /// past the stream the reference would send to `IStream::Seek`.
    fn block_offset(&self, index: u64) -> Option<usize> {
        let block = index as usize;
        let offset = block
            .checked_mul(self.info.bytes_per_block as usize)?
            .checked_mul(self.channels())?
            .checked_add(HEADER_LEN)?;
        Some(offset)
    }

    /// `TCWFDecoder::Render` (`:305-340`).
    ///
    /// `out` is the caller's interleaved sample buffer: `out.len() / channels`
    /// frames are requested, and a trailing partial frame is not written. A
    /// stream that ends at a block boundary stops the render with
    /// `status = 0`; a block that is truncated, out of sync or carries a
    /// out-of-range peak is an error, the port's naming of the reference's
    /// silent `ReadBlock` failures.
    pub(crate) fn render(&mut self, out: &mut [i16]) -> std::result::Result<TcwfRender, TcwfError> {
        let channels = self.channels();
        let frames = out.len() / channels;
        let mut rendered = 0;
        let mut status = 1;
        while rendered < frames {
            if self.buffer_remain == 0 {
                if !self.read_next_super_block()? {
                    status = 0;
                    break;
                }
                self.sample_pos = 0;
                self.buffer_remain = self.samples_per_block();
            }
            let frame = &mut out[rendered * channels..(rendered + 1) * channels];
            for (channel, sample) in frame.iter_mut().enumerate() {
                *sample = self.samples[self.sample_pos * channels + channel];
            }
            self.sample_pos += 1;
            self.buffer_remain -= 1;
            rendered += 1;
        }
        self.position += rendered as u64;
        Ok(TcwfRender { rendered, status })
    }

    /// `TCWFDecoder::SetPosition` (`:342-385`): seek to frame `samplepos`.
    ///
    /// The reference ignores a failed block read here and leaves stale samples
    /// in the buffer; the port reports the malformation instead.
    pub(crate) fn set_position(&mut self, samplepos: u64) -> std::result::Result<(), TcwfError> {
        let samples_per_block = self.samples_per_block() as u64;
        let block = samplepos / samples_per_block;
        let remnant = (samplepos % samples_per_block) as usize;
        let offset = self
            .block_offset(block)
            .ok_or(TcwfError::TruncatedBlock { block })?;
        self.decode_super_block(block, offset)?;
        // `StreamPos = DataStart + newbytepos` (`:373`) plus the block just
        // read: the invariant every block read relies on is
        // `stream_pos == block_offset(block_index)`.
        let super_len = self.info.bytes_per_block as usize * self.channels();
        self.stream_pos = offset + super_len;
        self.block_index = block + 1;
        self.position = samplepos;
        self.sample_pos = remnant;
        self.buffer_remain = samples_per_block as usize - remnant;
        Ok(())
    }

    /// The `Render` block advance (`:313-330`): `channels` block reads, or the
    /// stream's clean end. Bytes that remain but do not fill a whole
    /// superblock are a truncated block.
    fn read_next_super_block(&mut self) -> std::result::Result<bool, TcwfError> {
        let super_len = self.info.bytes_per_block as usize * self.channels();
        if self.stream_pos + super_len > self.data.len() {
            if self.stream_pos >= self.data.len() {
                return Ok(false);
            }
            return Err(TcwfError::TruncatedBlock {
                block: self.block_index,
            });
        }
        let offset = self.stream_pos;
        self.decode_super_block(self.block_index, offset)?;
        self.stream_pos += super_len;
        self.block_index += 1;
        Ok(true)
    }

    /// The reference's `for(i = 0; i < Header.channels; i++) ReadBlock(...)`
    /// (`:316-319`): one decoded superblock into `Samples`.
    fn decode_super_block(
        &mut self,
        block_index: u64,
        offset: usize,
    ) -> std::result::Result<(), TcwfError> {
        let layout = BlockLayout {
            channels: self.channels(),
            bytes_per_block: self.info.bytes_per_block as usize,
            samples_per_block: self.samples_per_block(),
            block_index,
        };
        let super_len = layout.bytes_per_block * layout.channels;
        let region = self
            .data
            .get(offset..offset + super_len)
            .ok_or(TcwfError::TruncatedBlock { block: block_index })?;
        self.samples
            .resize(layout.samples_per_block * layout.channels, 0);
        for channel in 0..layout.channels {
            let block =
                &region[channel * layout.bytes_per_block..(channel + 1) * layout.bytes_per_block];
            decode_channel_block(&layout, block, &mut self.samples, channel)?;
        }
        Ok(())
    }
}

/// The per-block geometry one channel's decode needs.
#[derive(Clone, Copy)]
struct BlockLayout {
    channels: usize,
    bytes_per_block: usize,
    samples_per_block: usize,
    block_index: u64,
}

/// `TCWFDecoder::ReadBlock` (`WUMainUnit.cpp:438-566`) for one channel.
///
/// `out` is the whole interleaved superblock (`samples_per_block * channels`
/// values); this channel's samples are at stride `channels`.
fn decode_channel_block(
    layout: &BlockLayout,
    block: &[u8],
    out: &mut [i16],
    channel: usize,
) -> std::result::Result<(), TcwfError> {
    let header = &block[..BLOCK_HEADER_LEN];
    let data = &block[BLOCK_HEADER_LEN..];
    let read_i16 = |offset: usize| i16::from_le_bytes([header[offset], header[offset + 1]]);
    let ms_sample0 = read_i16(0);
    let ms_sample1 = read_i16(2);
    let mut idelta = read_i16(4) as i32;
    let bpred = header[6] as i32;
    let ima_stepindex = header[7] as i32;
    let peaks = std::array::from_fn::<_, PEAK_COUNT, _>(|index| {
        let offset = 8 + index * 4;
        (
            u16::from_le_bytes([header[offset], header[offset + 1]]),
            i16::from_le_bytes([header[offset + 2], header[offset + 3]]),
        )
    });

    // `if(bpred>=7) return false; // おそらく同期がとれていない` (`:478`).
    if bpred >= ADAPT_COEFF1.len() as i32 {
        return Err(TcwfError::SyncLost {
            block: layout.block_index,
        });
    }

    let stride = layout.channels;
    let samples_per_block = layout.samples_per_block;
    let slot = |index: usize| index * stride + channel;

    // `Samples[0*numchans] = bheader.ms_sample0; Samples[1*numchans] = …`
    // (`:474-475`).
    out[slot(0)] = ms_sample0;
    out[slot(1)] = ms_sample1;

    // MS ADPCM on the low nibbles (`:483-506`).
    for k in 2..samples_per_block {
        let mut bytecode = (data[k - 2] & 0x0F) as i32;
        let idelta_save = idelta;
        idelta = (ADAPTATION_TABLE[bytecode as usize] * idelta) >> 8;
        if idelta < 16 {
            idelta = 16;
        }
        if bytecode & 0x8 != 0 {
            bytecode -= 0x10;
        }
        let predict = (out[slot(k - 1)] as i32 * ADAPT_COEFF1[bpred as usize]
            + out[slot(k - 2)] as i32 * ADAPT_COEFF2[bpred as usize])
            >> 8;
        let current = bytecode * idelta_save + predict;
        out[slot(k)] = clamp_i16(current);
    }

    // IMA ADPCM on the high nibbles, accumulated on top (`:508-548`).
    let mut stepindex = ima_stepindex;
    let mut prev = 0i32;
    for k in 2..samples_per_block {
        let bytecode = ((data[k - 2] >> 4) & 0x0F) as i32;
        let step = IMA_STEP_SIZE[stepindex as usize];
        let mut diff = step >> 3;
        if bytecode & 1 != 0 {
            diff += step >> 2;
        }
        if bytecode & 2 != 0 {
            diff += step >> 1;
        }
        if bytecode & 4 != 0 {
            diff += step;
        }
        if bytecode & 8 != 0 {
            diff = -diff;
        }
        let current = (prev + diff).clamp(-32768, 32767);
        // `if (stepindex < 0) … else if (stepindex > 88) …` (`:538-539`).
        stepindex = (stepindex + IMA_INDEX_ADJUST[bytecode as usize]).clamp(0, 88);
        prev = current;

        let summed = out[slot(k)] as i32 + current;
        out[slot(k)] = clamp_i16(summed);
    }

    // The six peak revisions (`:550-563`).
    for (pos, revise) in peaks {
        if revise == 0 {
            continue;
        }
        let index = pos as usize;
        if index >= samples_per_block {
            return Err(TcwfError::PeakOutOfRange {
                block: layout.block_index,
                pos: pos as u32,
            });
        }
        let revised = out[slot(index)] as i32 - revise as i32;
        out[slot(index)] = clamp_i16(revised);
    }

    Ok(())
}

/// The reference's `if (current > 32767) … else if (current < -32768) …`
/// clamps (`:500-505`, `:533-534`, `:545-546`, `:559-560`).
fn clamp_i16(value: i32) -> i16 {
    value.clamp(-32768, 32767) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    /// One channel block for the fixture builder below.
    struct BlockSpec {
        ms_sample0: i16,
        ms_sample1: i16,
        ms_idelta: i16,
        ms_bpred: u8,
        ima_stepindex: u8,
        peaks: [(u16, i16); PEAK_COUNT],
        /// The nibble bytes (`bytesperblock - 32` of them).
        data: Vec<u8>,
    }

    impl BlockSpec {
        /// A block whose two ADPCM layers decode to the constant
        /// `ms_sample1`/`0`: every low nibble is 0 (`AdaptationTable[0]` keeps
        /// `idelta` at its floor and `predict` repeats the last sample) and
        /// every high nibble is 0 with `stepindex = 0` (`7 >> 3 == 0`).
        fn constant(sample0: i16, sample1: i16, samples_per_block: usize) -> Self {
            Self {
                ms_sample0: sample0,
                ms_sample1: sample1,
                ms_idelta: 16,
                ms_bpred: 0,
                ima_stepindex: 0,
                peaks: [(0, 0); PEAK_COUNT],
                data: vec![0; samples_per_block.saturating_sub(2)],
            }
        }

        fn encode(&self, bytes_per_block: usize) -> Vec<u8> {
            let mut block = Vec::with_capacity(bytes_per_block);
            block.extend_from_slice(&self.ms_sample0.to_le_bytes());
            block.extend_from_slice(&self.ms_sample1.to_le_bytes());
            block.extend_from_slice(&self.ms_idelta.to_le_bytes());
            block.push(self.ms_bpred);
            block.push(self.ima_stepindex);
            for (pos, revise) in self.peaks {
                block.extend_from_slice(&pos.to_le_bytes());
                block.extend_from_slice(&revise.to_le_bytes());
            }
            debug_assert_eq!(block.len(), BLOCK_HEADER_LEN);
            block.extend_from_slice(&self.data);
            block.resize(bytes_per_block, 0);
            block
        }
    }

    /// Builds a `.tcw` stream from `superblocks` (each entry is one
    /// superblock's per-channel blocks), in the reference's layout.
    fn build_tcw(
        channels: u8,
        frequency: u32,
        samples_per_block: u32,
        superblocks: &[Vec<BlockSpec>],
    ) -> Vec<u8> {
        let bytes_per_block = BLOCK_HEADER_LEN + (samples_per_block as usize).saturating_sub(2);
        let mut stream = Vec::new();
        stream.extend_from_slice(MARK);
        stream.push(channels);
        stream.push(0);
        stream.extend_from_slice(&frequency.to_le_bytes());
        stream.extend_from_slice(&(superblocks.len() as u32).to_le_bytes());
        stream.extend_from_slice(&(bytes_per_block as u32).to_le_bytes());
        stream.extend_from_slice(&samples_per_block.to_le_bytes());
        for superblock in superblocks {
            assert_eq!(superblock.len(), channels as usize);
            for block in superblock {
                stream.extend_from_slice(&block.encode(bytes_per_block));
            }
        }
        stream
    }

    fn mono_constant_stream(samples_per_block: u32, blocks: u32, value: i16) -> Vec<u8> {
        let superblocks: Vec<Vec<BlockSpec>> = (0..blocks)
            .map(|_| {
                vec![BlockSpec::constant(
                    value,
                    value,
                    samples_per_block as usize,
                )]
            })
            .collect();
        build_tcw(1, 44_100, samples_per_block, &superblocks)
    }

    /// Assembles a stream from already-encoded blocks (the encoder path
    /// below), in stream order.
    fn assemble_tcw(
        channels: u8,
        frequency: u32,
        samples_per_block: u32,
        blocks: &[Vec<u8>],
    ) -> Vec<u8> {
        let bytes_per_block = BLOCK_HEADER_LEN + (samples_per_block as usize).saturating_sub(2);
        let mut stream = Vec::new();
        stream.extend_from_slice(MARK);
        stream.push(channels);
        stream.push(0);
        stream.extend_from_slice(&frequency.to_le_bytes());
        stream.extend_from_slice(&((blocks.len() / channels as usize) as u32).to_le_bytes());
        stream.extend_from_slice(&(bytes_per_block as u32).to_le_bytes());
        stream.extend_from_slice(&samples_per_block.to_le_bytes());
        for block in blocks {
            assert_eq!(block.len(), bytes_per_block, "block size");
            stream.extend_from_slice(block);
        }
        stream
    }

    /// `choose_predictor` of the official compressor
    /// (`krkr2 trunk src/tools/generic/tcwfcomp/src/comp/Main.cpp:112-145`),
    /// the predictor/idelta choice the format's blocks are built with.
    fn choose_predictor(data: &[i16]) -> (usize, i32) {
        let count = data.len().saturating_sub(2);
        if count == 0 {
            return (0, 16);
        }
        let mut best_bpred = 0usize;
        let mut best_idelta = 0u32;
        for bpred in 0..ADAPT_COEFF1.len() {
            let mut idelta_sum = 0u32;
            for k in 2..2 + count {
                let predicted = (data[k - 1] as i32 * ADAPT_COEFF1[bpred]
                    + data[k - 2] as i32 * ADAPT_COEFF2[bpred])
                    >> 8;
                idelta_sum += (data[k] as i32 - predicted).unsigned_abs();
            }
            idelta_sum /= (4 * count) as u32;
            if bpred == 0 || idelta_sum < best_idelta {
                best_bpred = bpred;
                best_idelta = idelta_sum;
            }
            if idelta_sum == 0 {
                best_bpred = bpred;
                best_idelta = 16;
                break;
            }
        }
        (best_bpred, best_idelta.max(16) as i32)
    }

    /// `encode_block` of the official compressor (`:148-295`), transcribed with
    /// the decoder's own tables. Returns the block bytes (its header included)
    /// and the PCM the block decodes to: the encoder's reconstruction plus its
    /// six peak revisions, which is exactly what the reference decoder
    /// reproduces.
    ///
    /// `stepindex` is the compressor's `static` IMA index: it carries from
    /// block to block (`:159`).
    fn encode_block(samples: &[i16], stepindex: &mut i32) -> (Vec<u8>, Vec<i16>) {
        let (bpred, idelta0) = choose_predictor(samples);
        let incoming_stepindex = *stepindex;
        let mut idelta = idelta0;
        let mut ms = samples.to_vec();
        let mut base = samples.to_vec();
        let mut temp = vec![0i32; samples.len()];
        let mut out = Vec::new();
        let mut prev = 0i32;
        for k in 2..samples.len() {
            let predict = (ms[k - 1] as i32 * ADAPT_COEFF1[bpred]
                + ms[k - 2] as i32 * ADAPT_COEFF2[bpred])
                >> 8;
            let mut errordelta = (samples[k] as i32 - predict) / idelta;
            errordelta = errordelta.clamp(-8, 7);
            let newsamp = clamp_i16(predict + idelta * errordelta);
            let table = if errordelta < 0 {
                (errordelta + 0x10) as usize
            } else {
                errordelta as usize
            };
            let ms_nibble = (table & 0x0F) as u8;
            idelta = (idelta * ADAPTATION_TABLE[table]) >> 8;
            if idelta < 16 {
                idelta = 16;
            }
            let ms_diff = samples[k] as i32 - newsamp as i32;
            ms[k] = newsamp;

            let mut diff = ms_diff - prev;
            let mut ima_nibble = 0u8;
            let mut step = IMA_STEP_SIZE[*stepindex as usize];
            let mut vpdiff = step >> 3;
            if diff < 0 {
                ima_nibble = 8;
                diff = -diff;
            }
            let mut mask = 4;
            while mask != 0 {
                if diff >= step {
                    ima_nibble |= mask;
                    diff -= step;
                    vpdiff += step;
                }
                step >>= 1;
                mask >>= 1;
            }
            if ima_nibble & 8 != 0 {
                prev -= vpdiff;
            } else {
                prev += vpdiff;
            }
            prev = prev.clamp(-32768, 32767);
            base[k] = clamp_i16(newsamp as i32 + prev);
            temp[k] = samples[k] as i32 - (newsamp as i32 + prev);
            *stepindex = (*stepindex + IMA_INDEX_ADJUST[ima_nibble as usize]).clamp(0, 88);
            out.push(ms_nibble | ((ima_nibble & 0x0F) << 4));
        }

        let mut peaks = [(0u16, 0i16); PEAK_COUNT];
        for entry in &mut peaks {
            let mut max = 0i32;
            let mut max_pos = 0usize;
            for (k, value) in temp.iter().enumerate().skip(2) {
                if *value < 0 {
                    if -*value > max {
                        max = -*value;
                        max_pos = k;
                    }
                } else if *value > max {
                    max = *value;
                    max_pos = k;
                }
            }
            *entry = (max_pos as u16, (-temp[max_pos]) as i16);
            temp[max_pos] += entry.1 as i32;
        }

        // What the reference decoder produces: the two layers summed, then the
        // peak revisions subtracted.
        let mut expected = base;
        for (pos, revise) in peaks {
            expected[pos as usize] = clamp_i16(expected[pos as usize] as i32 - revise as i32);
        }

        let mut block = Vec::with_capacity(BLOCK_HEADER_LEN + out.len());
        block.extend_from_slice(&samples[0].to_le_bytes());
        block.extend_from_slice(&samples[1].to_le_bytes());
        block.extend_from_slice(&(idelta0 as i16).to_le_bytes());
        block.push(bpred as u8);
        block.push(incoming_stepindex as u8);
        for (pos, revise) in peaks {
            block.extend_from_slice(&pos.to_le_bytes());
            block.extend_from_slice(&revise.to_le_bytes());
        }
        block.extend_from_slice(&out);
        (block, expected)
    }

    fn decode_all(decoder: &mut TcwfDecoder<'_>) -> Vec<i16> {
        let mut out = vec![0i16; 4096];
        let render = decoder.render(&mut out).expect("render");
        assert_eq!(render.status, 0, "the fixture must end the stream");
        out.truncate(render.rendered * decoder.info().channels as usize);
        out
    }

    #[test]
    fn the_header_is_read_into_the_tss_format() {
        let stream = mono_constant_stream(8, 3, 42);
        let decoder = TcwfDecoder::open(&stream).expect("open");
        assert_eq!(
            decoder.info(),
            TcwfInfo {
                sample_rate: 44_100,
                channels: 1,
                // `TSSFormat.dwBitsPerSample = 16` / `dwSeekable = 2`.
                bits_per_sample: 16,
                seekable: 2,
                num_blocks: 3,
                bytes_per_block: 38,
                samples_per_block: 8,
            }
        );
        assert_eq!(decoder.info().num_blocks, 3);
        assert_eq!(decoder.position(), 0);
    }

    /// The mission's hand-computed case: low nibbles 1..6 with `bpred = 0`
    /// (coeffs 256/0, so `predict = s[k-1]`), `idelta = 16`, and the peak
    /// revisions 35 at sample 3 and 100 at sample 0. The peak pass runs after
    /// both ADPCM layers, so the MS prediction still sees the unrevised 948.
    ///
    /// | k | nibble | `idelta_save` | `AdaptationTable[code]` | `s[k] = code*idelta_save + s[k-1]` |
    /// |---|---|---|---|---|
    /// | 2 | 1 | 16 | 230 → floor 16 | `16 + 900 = 916` |
    /// | 3 | 2 | 16 | 230 → 16 | `32 + 916 = 948` |
    /// | 4 | 3 | 16 | 230 → 16 | `48 + 948 = 996` |
    /// | 5 | 4 | 16 | 307 → 19 | `64 + 996 = 1060` |
    /// | 6 | 5 | 19 | 409 → 30 | `95 + 1060 = 1155` |
    /// | 7 | 6 | 30 | 512 → 60 | `180 + 1155 = 1335` |
    ///
    /// The high nibbles are 0 with `ima_stepindex = 0` (`7 >> 3 == 0`), so the
    /// IMA layer adds nothing; the peak at sample 0 replaces the first seed.
    #[test]
    fn a_mono_block_decodes_to_the_hand_computed_samples() {
        let mut spec = BlockSpec {
            ms_sample0: 1000,
            ms_sample1: 900,
            ms_idelta: 16,
            ms_bpred: 0,
            ima_stepindex: 0,
            peaks: [(0, 0); PEAK_COUNT],
            data: vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
        };
        spec.peaks[0] = (3, 35);
        spec.peaks[1] = (0, 100);
        let stream = build_tcw(1, 44_100, 8, &[vec![spec]]);
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        assert_eq!(
            decode_all(&mut decoder),
            vec![900, 900, 916, 913, 996, 1060, 1155, 1335],
        );
    }

    /// The IMA layer's own residual, hand-computed. `ms_idelta = 16` and low
    /// nibbles 0 keep the MS layer at its seeds (`s[2] = s[3] = 0`), and
    /// `ima_stepindex = 20` (`ima_step_size[20] = 50`):
    ///
    /// | k | high nibble | step | diff | `current` | stepindex |
    /// |---|---|---|---|---|---|
    /// | 2 | 4 | 50 | `50>>3 + 50 = 56` | `0 + 56 = 56` | `20+2 = 22` |
    /// | 3 | 8 | 60 | `60>>3 = 7`, negated by bit 3 | `56 - 7 = 49` | `22-1 = 21` |
    #[test]
    fn the_ima_layer_adds_its_residual_on_top_of_the_ms_layer() {
        let spec = BlockSpec {
            ms_sample0: 0,
            ms_sample1: 0,
            ms_idelta: 16,
            ms_bpred: 0,
            ima_stepindex: 20,
            peaks: [(0, 0); PEAK_COUNT],
            data: vec![0x40, 0x80],
        };
        let stream = build_tcw(1, 8_000, 4, &[vec![spec]]);
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        assert_eq!(decode_all(&mut decoder), vec![0, 0, 56, 49]);
    }

    #[test]
    fn a_stereo_stream_interleaves_the_channel_blocks() {
        // `:32`: the two blocks of a superblock are the right and left channel.
        let left = BlockSpec::constant(100, 200, 4);
        let right = BlockSpec::constant(-100, -200, 4);
        let stream = build_tcw(2, 22_050, 4, &[vec![left, right]]);
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        assert_eq!(decoder.info().channels, 2);
        assert_eq!(
            decode_all(&mut decoder),
            vec![100, -100, 200, -200, 200, -200, 200, -200],
        );
    }

    #[test]
    fn rendering_is_chunk_size_independent_and_reports_the_stream_end() {
        let stream = mono_constant_stream(4, 3, 7);
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        let mut whole = vec![0i16; 12];
        let render = decoder.render(&mut whole).expect("render");
        assert_eq!(
            render,
            TcwfRender {
                rendered: 12,
                status: 1
            }
        );
        // The request was filled exactly at the stream's end: the next call
        // discovers it (`status = 0`, nothing rendered).
        let mut extra = [0i16; 4];
        let render = decoder.render(&mut extra).expect("render");
        assert_eq!(
            render,
            TcwfRender {
                rendered: 0,
                status: 0
            }
        );
        assert_eq!(decoder.position(), 12);

        // The same stream in two-frame chunks decodes identically.
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        let mut chunked = Vec::new();
        loop {
            let mut chunk = [0i16; 2];
            let render = decoder.render(&mut chunk).expect("render");
            chunked.extend_from_slice(&chunk[..render.rendered]);
            if render.status == 0 {
                break;
            }
        }
        assert_eq!(chunked, whole);

        // A request that runs past the end fills what exists and stops.
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        let mut long = vec![0i16; 20];
        let render = decoder.render(&mut long).expect("render");
        assert_eq!(
            render,
            TcwfRender {
                rendered: 12,
                status: 0
            }
        );
    }

    #[test]
    fn set_position_seeks_to_a_frame() {
        let stream = mono_constant_stream(4, 3, 9);
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        let all = decode_all(&mut decoder);
        assert_eq!(all.len(), 12);

        for target in [5u64, 8, 11] {
            let mut decoder = TcwfDecoder::open(&stream).expect("open");
            decoder.set_position(target).expect("seek");
            assert_eq!(decoder.position(), target);
            let mut out = vec![0i16; 4096];
            let render = decoder.render(&mut out).expect("render");
            out.truncate(render.rendered);
            assert_eq!(
                out,
                all[target as usize..],
                "seek to {target} must continue the stream",
            );
        }

        // A seek into the middle of the first block keeps the previous
        // samples around it (`SamplePos = Samples + remnant*channels`,
        // `BufferRemain = samplesperblock - remnant`).
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        decoder.set_position(2).expect("seek");
        let mut out = vec![0i16; 4096];
        let render = decoder.render(&mut out).expect("render");
        out.truncate(render.rendered);
        assert_eq!(out, all[2..]);
    }

    /// The decoder against the official compressor's shape: three blocks
    /// (a ramp, a fast sine and silence) encoded by the transcription above
    /// must decode to exactly what the encoder says they decode to — the
    /// reconstruction plus its peak revisions.
    #[test]
    fn the_official_encoders_blocks_decode_to_the_encoders_intent() {
        use std::f64::consts::PI;

        const SAMPLES_PER_BLOCK: usize = 64;
        let mut stepindex = 0i32;
        let mut blocks = Vec::new();
        let mut expected = Vec::new();
        for block in 0..3u32 {
            let samples: Vec<i16> = (0..SAMPLES_PER_BLOCK)
                .map(|k| {
                    let k = k as f64;
                    let value = match block {
                        0 => k * 40.0 - 1_000.0,
                        1 => (2.0 * PI * k / 16.0).sin() * 8_000.0 + k * 12.0,
                        _ => 0.0,
                    };
                    value as i16
                })
                .collect();
            let (bytes, want) = encode_block(&samples, &mut stepindex);
            blocks.push(bytes);
            expected.extend_from_slice(&want);
        }

        let stream = assemble_tcw(1, 44_100, SAMPLES_PER_BLOCK as u32, &blocks);
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        assert_eq!(decode_all(&mut decoder), expected);
        assert!(
            expected.iter().any(|value| value.abs() > 4_000),
            "the stream must carry the sine, not silence",
        );
    }

    /// A constant block is representable exactly: every prediction error and
    /// every peak revision is zero (`temp[k] == 0`), so the encoder's intent is
    /// the source PCM and the decoder must return it sample for sample.
    #[test]
    fn a_constant_signal_round_trips_exactly_through_the_encoder() {
        let samples = vec![1_234i16; 24];
        let mut stepindex = 0i32;
        let (block, expected) = encode_block(&samples, &mut stepindex);
        assert_eq!(
            expected, samples,
            "a constant block is representable exactly",
        );
        let stream = assemble_tcw(1, 44_100, 24, &[block]);
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        assert_eq!(decode_all(&mut decoder), samples);
    }

    #[test]
    fn a_bad_mark_fails_to_open() {
        let mut stream = mono_constant_stream(4, 1, 0);
        stream[5] = b'X';
        assert_eq!(TcwfDecoder::open(&stream).err(), Some(TcwfError::BadMark));
    }

    #[test]
    fn a_short_header_fails_to_open() {
        let stream = mono_constant_stream(4, 1, 0);
        assert_eq!(
            TcwfDecoder::open(&stream[..HEADER_LEN - 1]).err(),
            Some(TcwfError::TruncatedHeader),
        );
    }

    #[test]
    fn a_zero_channel_header_is_refused() {
        let mut stream = mono_constant_stream(4, 1, 0);
        stream[6] = 0;
        assert_eq!(
            TcwfDecoder::open(&stream).err(),
            Some(TcwfError::NoChannels)
        );
    }

    #[test]
    fn a_block_that_cannot_hold_its_samples_is_refused() {
        let mut stream = mono_constant_stream(4, 1, 0);
        // samplesperblock 2048 with `bytesperblock` still 34: the loops would
        // read 2046 nibble bytes out of a 2-byte area.
        stream[20..24].copy_from_slice(&2048u32.to_le_bytes());
        assert_eq!(
            TcwfDecoder::open(&stream).err(),
            Some(TcwfError::BytesPerBlockTooSmall {
                bytes_per_block: 34,
                samples_per_block: 2048,
            }),
        );
        // `samplesperblock < 2` cannot seed `Samples[1]` either.
        let mut stream = mono_constant_stream(4, 1, 0);
        stream[20..24].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            TcwfDecoder::open(&stream).err(),
            Some(TcwfError::SamplesPerBlockTooSmall {
                samples_per_block: 1,
            }),
        );
    }

    /// `bpred >= 7` is the reference's "probably lost sync" (`:478`): its
    /// `ReadBlock` returns false, so `Render` stops the stream short.
    #[test]
    fn a_block_with_bpred_seven_loses_sync() {
        let mut spec = BlockSpec::constant(0, 0, 4);
        spec.ms_bpred = 7;
        let stream = build_tcw(1, 44_100, 4, &[vec![spec]]);
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        let mut out = [0i16; 4];
        assert_eq!(
            decoder.render(&mut out).err(),
            Some(TcwfError::SyncLost { block: 0 }),
        );
    }

    #[test]
    fn a_truncated_block_is_named() {
        let stream = mono_constant_stream(4, 2, 0);
        let cut = stream.len() - 6;
        let mut decoder = TcwfDecoder::open(&stream[..cut]).expect("open");
        let mut out = [0i16; 8];
        assert_eq!(
            decoder.render(&mut out).err(),
            Some(TcwfError::TruncatedBlock { block: 1 }),
        );
        // A tail that cannot even start a superblock is the block that would
        // have come next.
        let mut decoder = TcwfDecoder::open(&stream[..HEADER_LEN + 4]).expect("open");
        assert_eq!(
            decoder.render(&mut out).err(),
            Some(TcwfError::TruncatedBlock { block: 0 }),
        );
    }

    /// A peak position outside the block writes out of bounds in the
    /// reference (`Samples[pos*numchans]`, `:552-563`); the port names it.
    #[test]
    fn a_peak_outside_the_block_is_malformed() {
        let mut spec = BlockSpec::constant(0, 0, 4);
        spec.peaks[2] = (9, 5);
        let stream = build_tcw(1, 44_100, 4, &[vec![spec]]);
        let mut decoder = TcwfDecoder::open(&stream).expect("open");
        let mut out = [0i16; 4];
        assert_eq!(
            decoder.render(&mut out).err(),
            Some(TcwfError::PeakOutOfRange { block: 0, pos: 9 }),
        );
    }

    #[test]
    fn the_errors_read_as_messages() {
        assert_eq!(TcwfError::BadMark.to_string(), "TCWF0 mark missing");
        assert!(
            TcwfError::SyncLost { block: 3 }
                .to_string()
                .contains("block 3"),
        );
        assert!(
            TcwfError::BytesPerBlockTooSmall {
                bytes_per_block: 20,
                samples_per_block: 8,
            }
            .to_string()
            .contains("20 bytes per block"),
        );
    }

    #[test]
    fn registers_under_the_catalog_name_and_reports_the_decoder() {
        use krkr_engine::{EngineConfig, KrkrEngine};

        let engine = {
            let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
            engine.register_plugin(WutcwfPlugin).expect("plugin");
            engine
        };
        assert_eq!(catalog::canonical_name(NAME), Some(NAME));
        assert!(
            engine.host().linked_plugins().any(|linked| linked == NAME),
            "the plugin does not register the name the catalog resolves",
        );
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains(MEDIA_SUPPORT.extension)
                    && line.contains("not registered")),
            "no registration line naming the extension and the wiring gap",
        );
        assert_eq!(MEDIA_SUPPORT.extension, ".tcw");
        assert_eq!(MEDIA_SUPPORT.media_type, "TCWF ファイル");
    }
}
