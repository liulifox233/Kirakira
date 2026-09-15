//! `fftgraph.dll` — the global `drawFFTGraph(...)` spectrum visualiser.
//!
//! Reference: krkrz `fftgraph/Main.cpp` (621 lines, byte-identical in krkr2
//! trunk `src/plugins/win32/fftgraph/`) plus Ooura's `fftsg.c` for the
//! transform. The Kirikiroid2 port is a 10-line stub (`function
//! drawFFTGraph(){}`) and is not a reference. Line numbers in this module are
//! the reference's.
//!
//! # What the DLL registers
//!
//! One global function, installed on the global object in `V2Link`
//! (`Main.cpp:535-541`):
//!
//! ```text
//! drawFFTGraph(layer, soundBuffer, left, top, width, height [, options])
//! ```
//!
//! `numparams < 6` fails with `TJS_E_BADPARAMCOUNT` (`:416`). The layer is
//! duck-typed through `imageWidth` / `imageHeight` / `mainImageBufferForWrite`
//! / `mainImageBufferPitch` (`:430-448`; a failed `PropGet` throws
//! `invoking of Layer.<name> failed.`), the pixels are written directly into
//! that buffer in the layer's native format (`0xAARRGGBB` little-endian), and
//! the call ends with `layer.update(left, top, width, height)` (`:491`).
//!
//! # What it computes
//!
//! * **Samples** (`Main.cpp:36-65`): `soundBuffer.getVisBuffer(ptr, 2048, 1,
//!   0)` — 2048 mono int16 samples at the play cursor. A call that succeeds
//!   with `ret < 2048` zero-fills the buffer; a failed call leaves the previous
//!   contents in place.  The reference lends the engine its own `short*` and
//!   reads it back through the pointer; this engine has no pointer channel, so
//!   the destination is a TJS array the engine fills with the same samples
//!   (`plugin_api::audio` → the audio backend's decoded-PCM tap, installed by
//!   `wf_basic_effect`'s `install_engine_audio_bridge`).
//! * **Window** (`:100-107`): `w[i] = sin(pi*(i+0.5)/len) * (4/32768/len)`,
//!   built once per length, stored as f32. The windowed data interleaves the
//!   halves — `a[i] = s[i]*w[i]`, `a[i+len/2] = s[i+len/2]*w[len/2-1-i]`
//!   (`:110-114`), i.e. the upper half of the buffer sees the mirrored lower
//!   half of the window.
//! * **Transform** (`:117`): Ooura `ddst(len, -1, …)`, the DST case2
//!   `S[k] = sum_j a[j]*sin(pi*(j+0.5)*k/len)` with the output layout
//!   `a[0] = S[len]`, `a[k] = S[k]` (`fftsg.c:196-208`). Then the DC cut
//!   `a[0] = a[1] = 0` (`:120`). Because the basis advances
//!   `2*pi*k/(2*len)` per sample, a tone at DFT bin `f` lands at bin `2*f`.
//! * **Bands** (`:130-209`): `count` log-spaced bands over the bin range,
//!   `start = floor(len^j)`, `end = floor(len^(j+1))` with
//!   `j = i*((cut-1)/cut)/count + 1/cut` — the lowest `1/cut` of the log scale
//!   is hidden. Each band's value is `10*log10(max^2)` of its largest absolute
//!   bin, clamped to `[mindb, 0]`, mapped to `vi = maxval*(1 - db/mindb)`,
//!   then applied with fall-off and a peak-hold timer.
//! * **Drawing**: `type` 0 (default, "Winamp fire") draws one column per band
//!   (`count = width`) with a 16-entry fire gradient and a grey peak pixel
//!   (`:317-372`); `type` 1 ("LCD") draws `division` bar cells
//!   (`division`/`thick`/`oncolor`/`offcolor`/`bgcolor`/`peakcolor` options,
//!   `:373-393`, `DrawBarGraph` `:246-306`). An unhandled `type` draws nothing
//!   (`:317-394`: the switch has no `default`).
//!
//! # How this port maps it
//!
//! * The spectrum computation is the reference's end to end: same window (same
//!   f32 construction and half-interleave), same transform definition and
//!   output layout, same band edges, dB mapping, fall/peak state, and both
//!   drawing modes. The DST is **not** a transcription of Ooura's f32
//!   split-radix work-array code: it evaluates the identical DST-case2
//!   transform with a zero-padded complex FFT and f64 intermediates (narrowed
//!   to f32 at the data boundary), so bins agree with the definition and with
//!   Ooura up to the last bits of each f32 bin. The tests pin the definition
//!   both ways (a closed form for an impulse, direct summation otherwise, and
//!   the band heights of a single tone against an independent scratch
//!   computation).
//! * The layer bitmap is reached through the engine's scoped bitmap view
//!   ([`krkr_engine::plugin_api::layer`]) instead of `mainImageBufferForWrite`:
//!   that member deliberately answers `void` here
//!   (`crates/krkr-engine/src/native/classes.rs`), and the view is the
//!   memory-safe stand-in for the raw pointer. Pixels are stored R,G,B,A by the
//!   engine, so the reference's `0xAARRGGBB` words are split on write. Writes
//!   outside the bitmap (the reference writes through an unchecked pointer and
//!   scribbles whatever follows the image) are dropped, and a call on a layer
//!   with no image fails with the family's `Not drawable layer type` instead of
//!   the reference's `invoking of Layer.imageWidth failed.` wrapper — the
//!   engine's own `imageWidth` accessor already throws that family error.
//! * The reference's file-static analyzer state (`Main.cpp:11-26`, one
//!   spectrum per process) is a process-wide mutex-held [`Analyzer`] here. No
//!   script runs while the lock is held, so a member call that re-enters
//!   `drawFFTGraph` sees the previous state instead of deadlocking.
//! * The sample fetch calls the sound buffer's own `getVisBuffer` — the
//!   reference's script-level contract, so a script that replaced the member
//!   still wins — and the engine answers it from the audio backend's decoded
//!   PCM (`plugin_api::audio`, the tap `krkr-audio` publishes).  A buffer that
//!   is not playing answers 0, which zero-fills the analyzer and renders the
//!   reference's own "not playing" state.

use std::{f64::consts::PI, sync::Mutex, sync::MutexGuard};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::layer::{LayerBitmapError, layer_bitmap_read, layer_bitmap_write, layer_update},
};
use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "global drawFFTGraph(layer, soundBuffer, left, top, width, height [, options])",
    notes: "The surface, the spectrum computation (sine window, 2048-sample DST, log bands, dB \
            with fall-off and peak hold), both drawing modes (type 0 fire, type 1 LCD bars with \
            division/thick/oncolor/offcolor/bgcolor/peakcolor) and the sample source are real and \
            numerically tested: the fetch runs through the sound buffer's own `getVisBuffer`, \
            which the engine answers from the audio backend's decoded-PCM tap, so a playing \
            buffer paints a real spectrum and a silent one keeps the reference's silence state. \
            The reference's raw `short*` destination is a TJS array here (no pointer channel).",
    install: |engine| engine.register_plugin(FftGraphPlugin),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what a game's
/// `Plugins.link("fftgraph.dll")` resolves through [`crate::catalog`].
pub(crate) const NAME: &str = "fftgraph.dll";

/// The function the reference installs as a global (`Main.cpp:537`).
const FUNCTION_NAME: &str = "drawFFTGraph";

/// `numparams < 6` is `TJS_E_BADPARAMCOUNT` (`Main.cpp:416`).
const MIN_PARAMS: usize = 6;

/// `GetVisBuffer(param.sb, 2048, 1, 0)` (`Main.cpp:310`): 2048 mono samples.
const SAMPLE_FRAMES: usize = 2048;

pub struct FftGraphPlugin;

impl KrkrPlugin for FftGraphPlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The reference registers the function on the global object in `V2Link`
        // (`Main.cpp:535-541`); a member a script defines later still wins by
        // ordinary member lookup.
        runtime.register_global_native_with_arg_count(
            FUNCTION_NAME,
            NativeArgCount::AtLeast(MIN_PARAMS),
            draw_fft_graph,
        );
        // The readback `fetch_vis_buffer` reads through the sound buffer's own
        // `getVisBuffer`: the audio backend's decoded-PCM tap, installed
        // through the engine's plugin API (see `wf_basic_effect`'s
        // `install_engine_audio_bridge`).
        crate::wf_basic_effect::install_engine_audio_bridge();
        runtime.host_mut().log(
            "fftgraph: drawFFTGraph(layer, soundBuffer, left, top, width, height [, options]) \
             registered (spectrum + both graph types real; the sound buffer's getVisBuffer is \
             still a silent no-op here, so the graph draws the reference's silence state).",
        );
        Ok(())
    }
}

// ---------------------------------------------------------------- analyzer

/// The reference's file-static analyzer state (`Main.cpp:11-26`): the sample
/// buffer, the transform data, the window and the per-band history. One
/// analyzer per process — showing several spectra at once is explicitly not a
/// goal of the plugin (`:11`).
struct Analyzer {
    /// `SampleBuffer`: the last samples the sound buffer handed over.
    samples: Vec<i16>,
    /// `FFTData`: windowed samples, then the transform output.
    fft: Vec<f32>,
    /// `WindowData`, built once (`Main.cpp:100-107`).
    window: Vec<f32>,
    /// `BandCount` / `BandCut`: the layout the bands were built for. A count of
    /// 0 is the reference's "nothing allocated yet" sentinel.
    band_count: usize,
    band_cut: f32,
    /// One entry per band: `BandData`, `BandPeakData`, `BandPeakCount`,
    /// `BandStart`, `BandEnd`.
    bands: Vec<Band>,
}

#[derive(Clone, Copy, Default)]
struct Band {
    data: i32,
    peak: i32,
    peak_count: i32,
    start: usize,
    end: usize,
}

impl Analyzer {
    const fn new() -> Self {
        Self {
            samples: Vec::new(),
            fft: Vec::new(),
            window: Vec::new(),
            band_count: 0,
            band_cut: 0.0,
            bands: Vec::new(),
        }
    }

    /// What `GetVisBuffer`'s result does to the sample buffer
    /// (`Main.cpp:59-63`): a successful call that reports fewer than
    /// [`SAMPLE_FRAMES`] written samples zero-fills; a failed call leaves the
    /// buffer alone.  The reference's engine wrote the samples into the
    /// plugin's own buffer through the pointer; here the window arrives as the
    /// fetched slice and the same rule turns it into the buffer's contents.
    fn update_samples(&mut self, window: Option<Vec<i16>>) {
        if self.samples.len() != SAMPLE_FRAMES {
            self.samples = vec![0; SAMPLE_FRAMES];
        }
        let Some(window) = window else {
            // A failed call keeps the previous samples (`Main.cpp:62-63`).
            return;
        };
        if window.len() < SAMPLE_FRAMES {
            // `ZeroMemory(SampleBuffer, ...)` — the whole buffer, not just the
            // missing tail (`Main.cpp:59-61`).
            self.samples.fill(0);
        }
        let written = window.len().min(SAMPLE_FRAMES);
        self.samples[..written].copy_from_slice(&window[..written]);
    }

    /// `DoFFT` (`Main.cpp:76-122`): window the samples, transform, cut DC.
    fn do_fft(&mut self) {
        let half = SAMPLE_FRAMES / 2;
        if self.window.len() != SAMPLE_FRAMES {
            self.window = build_window(SAMPLE_FRAMES);
            self.fft = vec![0.0; SAMPLE_FRAMES];
        }

        // `FFTData[i] = SampleBuffer[i]*WindowData[i]` and
        // `FFTData[i+len2] = SampleBuffer[i+len2]*WindowData[len2-i-1]`
        // (`Main.cpp:110-114`).
        for i in 0..half {
            self.fft[i] = self.samples[i] as f32 * self.window[i];
            self.fft[i + half] = self.samples[i + half] as f32 * self.window[half - i - 1];
        }

        dst_ii(&mut self.fft);

        // DC cut (`Main.cpp:120`).
        self.fft[0] = 0.0;
        self.fft[1] = 0.0;
    }

    /// `GetBandSpectrum` (`Main.cpp:130-209`): per-band peak, dB, fall-off and
    /// peak hold. The layout is rebuilt whenever `count`/`cut` change, exactly
    /// like the reference (`:133-166`).
    #[allow(clippy::too_many_arguments)]
    fn get_band_spectrum(
        &mut self,
        count: usize,
        cut: f32,
        mindb: f32,
        maxval: i32,
        falldown: i32,
        peakholdtime: i32,
        peakfalldown: i32,
    ) {
        if count != self.band_count || cut != self.band_cut {
            self.band_count = count;
            self.band_cut = cut;
            self.bands = build_bands(count, cut);
        }

        let max_mindb_1 = (maxval as f64 / mindb as f64) as f32;
        for band in &mut self.bands {
            let mut max = 0.0f32;
            for bin in band.start..band.end.min(self.fft.len()) {
                let value = self.fft[bin].abs();
                if max < value {
                    max = value;
                }
            }

            // `if(max != 0) db = log10(max*max) * 10; else db = -1000000;`
            // (`Main.cpp:182`), then the clamp to `[mindb, 0]` (`:183-184`).
            let mut db = if max != 0.0 {
                ((max * max) as f64).log10() as f32 * 10.0
            } else {
                -1000000.0
            };
            if db > 0.0 {
                db = 0.0;
            }
            if db < mindb {
                db = mindb;
            }

            let vi = (maxval as f32 - max_mindb_1 * db) as i32;

            band.data -= falldown;
            if band.data < 0 {
                band.data = 0;
            }
            if band.data < vi {
                band.data = vi;
            }
            if band.data > maxval {
                band.data = maxval;
            }

            if band.peak_count == peakholdtime {
                band.peak -= peakfalldown;
                if band.peak < 0 {
                    band.peak = 0;
                }
            } else {
                band.peak_count += 1;
            }

            if band.peak < vi {
                band.peak = vi;
                band.peak_count = 0;
            }
        }
    }
}

/// The process-wide analyzer, the reference's file statics (`Main.cpp:11-26`).
static ANALYZER: Mutex<Analyzer> = Mutex::new(Analyzer::new());

/// The analyzer lock, immune to a panic in a previous holder.
fn lock_analyzer() -> MutexGuard<'static, Analyzer> {
    ANALYZER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The sine window, `mul = 4.0/32768.0/len` (`Main.cpp:101-106`). The multiply
/// order mirrors the reference's: the scale is rounded to f32 once, and each
/// value is `sin(...)` in f64 multiplied by that f32 and rounded once.
fn build_window(len: usize) -> Vec<f32> {
    let mul = (4.0f64 / 32768.0 / len as f64) as f32;
    (0..len)
        .map(|i| ((PI * (i as f64 + 0.5) / len as f64).sin() * mul as f64) as f32)
        .collect()
}

/// The band layout `GetBandSpectrum` builds (`Main.cpp:154-165`):
/// `j = i*((cut-1)/cut)/count + 1/cut` in f32 with the reference's promotions,
/// `start = floor(len^j)`, `end = floor(len^(j+1))`, widened to at least one
/// bin and capped at the transform length.
fn build_bands(count: usize, cut: f32) -> Vec<Band> {
    // `(float)(i) * ((cut-1)/cut) / (float)count + (1.0/cut)`: the last term
    // is a double division promoted into the float sum.
    let edge = |index: usize| -> f32 {
        let slope = (index as f32) * ((cut - 1.0) / cut) / (count as f32);
        (slope as f64 + 1.0f64 / cut as f64) as f32
    };
    let mut bands = Vec::with_capacity(count);
    for i in 0..count {
        let start = (SAMPLE_FRAMES as f64).powf(edge(i) as f64) as i64;
        let mut end = (SAMPLE_FRAMES as f64).powf(edge(i + 1) as f64) as i64;
        if start == end {
            end += 1;
        }
        if end > SAMPLE_FRAMES as i64 {
            end = SAMPLE_FRAMES as i64;
        }
        // The reference indexes its arrays unchecked; a band range outside the
        // transform (a pathological `cut`) reads garbage there. Clamp instead.
        bands.push(Band {
            start: start.clamp(0, SAMPLE_FRAMES as i64) as usize,
            end: end.clamp(0, SAMPLE_FRAMES as i64) as usize,
            ..Band::default()
        });
    }
    bands
}

// -------------------------------------------------------------------- DST

/// Ooura's `ddst(n, -1, a, ip, w)` (`fftsg.c:405-458` and the case2 definition
/// at `:196-208`): the DST `S[k] = sum_j a[j]*sin(pi*(j+0.5)*k/n)` for
/// `0 < k <= n`, output with `a[0] = S[n]` and `a[k] = S[k]`.
///
/// The reference calls Ooura's f32 split-radix routine through its work arrays
/// (`Main.cpp:117`); this port computes the identical transform as one complex
/// FFT of length `2n` over the zero-padded input with f64 intermediates: with
/// `X[k] = sum_j a[j]*exp(+2*pi*i*j*k/(2n))`,
/// `S[k] = Im(exp(i*pi*k/(2n)) * X[k])`. The result is narrowed to f32 at the
/// data boundary, so bins agree with the definition and differ from Ooura only
/// in the last bits.
fn dst_ii(a: &mut [f32]) {
    let n = a.len();
    if n < 2 {
        return;
    }
    let len = n * 2;
    let mut re = vec![0.0f64; len];
    let mut im = vec![0.0f64; len];
    for (index, value) in a.iter().enumerate() {
        re[index] = *value as f64;
    }
    fft_positive_sign(&mut re, &mut im);
    let mut out = vec![0.0f32; n];
    for k in 1..=n {
        let theta = PI * k as f64 / len as f64;
        let value = theta.sin() * re[k] + theta.cos() * im[k];
        // Index 0 holds S[n]; index k holds S[k] (`fftsg.c:203-205`).
        out[if k == n { 0 } else { k }] = value as f32;
    }
    a.copy_from_slice(&out);
}

/// In-place radix-2 DFT with the `+` sign convention
/// `X[k] = sum_j x[j]*exp(+2*pi*i*j*k/n)`; `n` must be a power of two. The sign
/// is the one `dst_ii`'s derivation needs.
fn fft_positive_sign(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two(), "radix-2 FFT needs a power of two");
    let mut target = 0usize;
    for index in 1..n {
        let mut bit = n >> 1;
        while target & bit != 0 {
            target ^= bit;
            bit >>= 1;
        }
        target |= bit;
        if index < target {
            re.swap(index, target);
            im.swap(index, target);
        }
    }
    let mut block = 2;
    while block <= n {
        let angle = 2.0 * PI / block as f64;
        let (twiddle_re, twiddle_im) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(block) {
            let (mut step_re, mut step_im) = (1.0f64, 0.0f64);
            for k in 0..block / 2 {
                let (low_re, low_im) = (re[start + k], im[start + k]);
                let (high_re, high_im) = (re[start + k + block / 2], im[start + k + block / 2]);
                let term_re = high_re * step_re - high_im * step_im;
                let term_im = high_re * step_im + high_im * step_re;
                re[start + k] = low_re + term_re;
                im[start + k] = low_im + term_im;
                re[start + k + block / 2] = low_re - term_re;
                im[start + k + block / 2] = low_im - term_im;
                let next_re = step_re * twiddle_re - step_im * twiddle_im;
                step_im = step_re * twiddle_im + step_im * twiddle_re;
                step_re = next_re;
            }
        }
        block <<= 1;
    }
}

// ---------------------------------------------------------------- options

/// The rect `drawFFTGraph` was called with (`FFTGraphParam`, `Main.cpp:218-230`).
struct GraphRequest {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    options: Option<ObjectHandle>,
}

/// The type 1 colours (`Main.cpp:375-380` defaults, `:384-389` reads).
#[derive(Clone, Copy)]
struct BarColors {
    oncolor: u32,
    offcolor: u32,
    bgcolor: u32,
    peakcolor: u32,
}

/// What the call ended up drawing.
enum Graph {
    /// Type 0 (`Main.cpp:319-371`) with one band per column.
    Fire(Vec<Band>),
    /// Type 1 (`Main.cpp:373-393`, `DrawBarGraph` `:246-306`).
    Bars {
        bands: Vec<Band>,
        th: i32,
        thickness: i32,
        colors: BarColors,
    },
    /// An unhandled `type`, or a rect the reference cannot draw: nothing.
    Nothing,
}

/// `GetValueFromOptions` (`Main.cpp:232-244`): with no options object, or a
/// name the object does not carry, the default stands. A name that resolves to
/// something else reads through the TJS integer conversion.
fn option_integer(
    runtime: &Runtime<KrkrHost>,
    options: Option<ObjectHandle>,
    name: &str,
    default: i64,
) -> i64 {
    let Some(options) = options else {
        return default;
    };
    match runtime.object_member(options, name) {
        Variant::Void => default,
        value => value.to_integer().unwrap_or(default),
    }
}

// ----------------------------------------------------------------- drawing

/// One writable layer bitmap in the engine's R,G,B,A order.
struct Surface<'a> {
    pixels: &'a mut [u8],
    width: i32,
    height: i32,
    pitch: usize,
}

impl Surface<'_> {
    /// Writes a reference `0xAARRGGBB` word at layer coordinates `(x, y)`.
    /// The reference writes through an unchecked pointer at
    /// `(top + height - 1 - y)` rows of `pitch` bytes; coordinates outside the
    /// bitmap are dropped here instead of corrupting the memory behind it.
    fn write_argb(&mut self, x: i32, y: i32, argb: u32) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let offset = y as usize * self.pitch + x as usize * 4;
        if offset + 4 > self.pixels.len() {
            return;
        }
        self.pixels[offset] = (argb >> 16) as u8;
        self.pixels[offset + 1] = (argb >> 8) as u8;
        self.pixels[offset + 2] = argb as u8;
        self.pixels[offset + 3] = (argb >> 24) as u8;
    }
}

/// Type 0's palette (`Main.cpp:322-329`).
const FIRE_COLORS: [u32; 16] = [
    0xff20ff00, 0xff40ff00, 0xff60ff00, 0xff80ff00, 0xffa0ff00, 0xffc0ff00, 0xffe0ff00, 0xffffff00,
    0xffffe000, 0xffffc000, 0xffffa000, 0xffff8000, 0xffff6000, 0xffff4000, 0xffff2000, 0xffffff00,
];
const FIRE_PEAK_COLOR: u32 = 0xff808080;

/// `DrawFFTGraph` (`Main.cpp:308-395`): fetch, transform, then one of the two
/// graph types. Its only failure is the layer family's: every layer call here
/// answers [`LayerBitmapError::NotDrawable`] at most.
fn draw_graph(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    sound_buffer: Option<ObjectHandle>,
    request: &GraphRequest,
) -> std::result::Result<(), LayerBitmapError> {
    if request.width <= 0 || request.height <= 0 {
        // The reference allocates its band arrays with these unchecked and
        // walks them; a negative rect is out of contract. Draw nothing.
        return Ok(());
    }

    // The reference reads the layer's image accessors before it touches the
    // sound buffer (`Main.cpp:430-448`), so a layer without an image fails
    // before any spectrum work; the scoped read probe is that check here.
    layer_bitmap_read(runtime, layer, |_| ()).map_err(|_| LayerBitmapError::NotDrawable)?;

    // `GetVisBuffer` runs script (a script-provided member), so it happens
    // before the analyzer lock is taken: a member that re-enters
    // `drawFFTGraph` sees the previous state instead of deadlocking.
    let written = sound_buffer.and_then(|sound_buffer| fetch_vis_buffer(runtime, sound_buffer));
    {
        let mut analyzer = lock_analyzer();
        analyzer.update_samples(written);
        analyzer.do_fft();
    }

    // `type = GetValueFromOptions(...)` (`Main.cpp:313-315`) and the type 1
    // reads (`:384-389`); option members can be script getters, so they run
    // outside the lock too.
    let graph = match option_integer(runtime, request.options, "type", 0) as i32 {
        0 => {
            let mut analyzer = lock_analyzer();
            analyzer.get_band_spectrum(
                request.width as usize,
                3.7,
                -70.0,
                request.height - 1,
                (request.height + 15) / 16,
                30,
                (request.height + 31) / 32,
            );
            Graph::Fire(analyzer.bands.clone())
        }
        1 => {
            let division = option_integer(runtime, request.options, "division", 16) as i32;
            let thickness = option_integer(runtime, request.options, "thick", 2) as i32;
            let colors = BarColors {
                oncolor: option_integer(runtime, request.options, "oncolor", 0xff000000) as u32,
                offcolor: option_integer(runtime, request.options, "offcolor", 0xffb0b0b0) as u32,
                bgcolor: option_integer(runtime, request.options, "bgcolor", 0xffc0c0c0) as u32,
                peakcolor: option_integer(runtime, request.options, "peakcolor", 0xff707070) as u32,
            };
            // `1000 / (height/thickness)` and `width/division` divide by zero
            // on these inputs in the reference; draw nothing.
            if division <= 0 || thickness <= 0 || request.height / thickness == 0 {
                Graph::Nothing
            } else {
                let th = 1000 / (request.height / thickness);
                let mut analyzer = lock_analyzer();
                analyzer.get_band_spectrum(
                    division as usize,
                    8.0,
                    -70.0,
                    1000 - th * 2,
                    40,
                    40,
                    10,
                );
                Graph::Bars {
                    bands: analyzer.bands.clone(),
                    th,
                    thickness,
                    colors,
                }
            }
        }
        _ => Graph::Nothing,
    };

    layer_bitmap_write(runtime, layer, |view| {
        let mut surface = Surface {
            pixels: view.pixels,
            width: view.bitmap.width as i32,
            height: view.bitmap.height as i32,
            pitch: view.bitmap.pitch as usize,
        };
        match &graph {
            Graph::Fire(bands) => draw_fire(&mut surface, request, bands),
            Graph::Bars {
                bands,
                th,
                thickness,
                colors,
            } => draw_bar_graph(&mut surface, request, bands, *th, *thickness, *colors),
            Graph::Nothing => {}
        }
    })
    .map_err(|_| LayerBitmapError::NotDrawable)?;

    // The reference's closing `layer.update(left, top, width, height)`
    // (`Main.cpp:491`), whose own failure its `FuncCall` ignores. The engine's
    // `Layer.update` repaints the layer; the rect is not used.
    let _ = layer_update(runtime, layer);
    Ok(())
}

/// Type 0, "Winamp fire" (`Main.cpp:317-372`): one log band per column, a fire
/// gradient from the bottom, transparent above the bar, and the peak value's
/// grey pixel at its held height.
fn draw_fire(surface: &mut Surface, request: &GraphRequest, bands: &[Band]) {
    let bottom = request.top + request.height - 1;
    for x in 0..request.width {
        let Some(band) = bands.get(x as usize) else {
            break;
        };
        let mut y = 0;
        while y < band.data {
            let color = 15 - band.data + y;
            let color = if color < 0 {
                0xff00ff00
            } else {
                FIRE_COLORS[color as usize]
            };
            surface.write_argb(request.left + x, bottom - y, color);
            y += 1;
        }
        while y < request.height {
            surface.write_argb(request.left + x, bottom - y, 0);
            y += 1;
        }
        // `*(lbstart + x*4 - BandPeakData[x]*pitch) = peak_color` (`:367-369`).
        surface.write_argb(request.left + x, bottom - band.peak, FIRE_PEAK_COLOR);
    }
}

/// Type 1, "LCD" bars (`DrawBarGraph`, `Main.cpp:246-306`): `count` cells of
/// `bw = width/count` columns, each cell drawn in `thickness`-row groups — the
/// group's lowest row all-`bgcolor`, the rows above it column 0 `bgcolor` and
/// the rest in the peak/on/off colour that the running `v = k*th` selects.
fn draw_bar_graph(
    surface: &mut Surface,
    request: &GraphRequest,
    bands: &[Band],
    th: i32,
    thickness: i32,
    colors: BarColors,
) {
    let bottom = request.top + request.height - 1;
    if bands.is_empty() {
        // `width/count` would divide by zero; the caller only builds an empty
        // layout for a zero division, which it already refuses to draw.
        return;
    }
    let bw = request.width / bands.len() as i32;
    for (index, band) in bands.iter().enumerate() {
        let column = request.left + index as i32 * bw;
        let mut y = 0;
        let mut v = 0;
        let mut peakdrawn = false;
        loop {
            for x in 0..bw {
                surface.write_argb(column + x, bottom - y, colors.bgcolor);
            }
            let color = if !peakdrawn && band.peak <= v && colors.peakcolor != colors.offcolor {
                peakdrawn = true;
                colors.peakcolor
            } else if band.data >= v {
                colors.oncolor
            } else {
                colors.offcolor
            };
            for row in (y + 1..).take(thickness.saturating_sub(1) as usize) {
                surface.write_argb(column, bottom - row, colors.bgcolor);
                for x in 1..bw {
                    surface.write_argb(column + x, bottom - row, color);
                }
            }
            y += thickness;
            v += th;
            if y + thickness > request.height {
                break;
            }
        }
    }
}

// --------------------------------------------------------------- the calls

/// The reference's `GetVisBuffer` member call (`Main.cpp:48-57`):
/// [`SAMPLE_FRAMES`] mono samples at the play cursor.  The reference lends the
/// engine its own `short*`; this engine has no pointer channel, so the
/// destination is a TJS array the engine fills with the same samples.  `None`
/// means the call failed (`Main.cpp:59-63` leaves the previous buffer alone);
/// a successful call with a short count is the zero-filled case the caller
/// applies.
fn fetch_vis_buffer(
    runtime: &mut Runtime<KrkrHost>,
    sound_buffer: ObjectHandle,
) -> Option<Vec<i16>> {
    let destination = runtime.alloc_array_object(Vec::new());
    let written = runtime
        .call_object_method(
            sound_buffer,
            "getVisBuffer",
            vec![
                Variant::Object(destination),
                Variant::Integer(SAMPLE_FRAMES as i64),
                Variant::Integer(1),
                Variant::Integer(0),
            ],
        )
        .ok()?
        .to_integer()
        .unwrap_or(0);
    let written = written.clamp(0, SAMPLE_FRAMES as i64) as usize;
    let elements = runtime.array_elements(destination)?;
    Some(
        elements
            .iter()
            .take(written)
            .map(|value| value.to_integer().unwrap_or(0) as i16)
            .collect(),
    )
}

/// `tDrawFFTGraphFunction::FuncCall` (`Main.cpp:404-498`).
fn draw_fft_graph(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // The reference casts `param[0]` without a check (`Main.cpp:418`) and
    // faults on a non-object; the port reports the layer family's error.
    let Some(layer) = args.first().and_then(Variant::object_handle) else {
        return Err(LayerBitmapError::NotDrawable.into());
    };
    let sound_buffer = args.get(1).and_then(Variant::object_handle);

    // `fftparam.left = *param[2]` … — tjs_int truncation of the variants.
    let coordinate = |index: usize| -> Result<i32> {
        Ok(args
            .get(index)
            .map(Variant::to_integer)
            .transpose()?
            .unwrap_or(0) as i32)
    };
    let request = GraphRequest {
        left: coordinate(2)?,
        top: coordinate(3)?,
        width: coordinate(4)?,
        height: coordinate(5)?,
        // `param[6]->AsObjectNoAddRef()`: a non-object option argument reads as
        // "no options", so every option keeps its default (`Main.cpp:473-476`).
        options: args.get(6).and_then(Variant::object_handle),
    };

    draw_graph(runtime, layer, sound_buffer, &request)?;

    // `return TJS_S_OK` (`Main.cpp:496`); the function has no result.
    Ok(Variant::Void)
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::*;
    use crate::catalog;

    /// The assertion tolerance for f32 transform output against f64
    /// expectations: a relative 1e-4 with an absolute floor for exact zeros.
    const DST_TOLERANCE: f64 = 1e-4;

    fn close(got: f32, want: f64) -> bool {
        (got as f64 - want).abs() <= DST_TOLERANCE * want.abs() + 1e-9
    }

    /// A deterministic 16-bit sample vector (a plain LCG, so the test needs no
    /// fixture file and the vector is reproducible).
    fn pseudo_random_samples(seed: u64) -> Vec<i16> {
        let mut state = seed;
        (0..SAMPLE_FRAMES)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 33) as i16
            })
            .collect()
    }

    /// An impulse at sample 0: `x[0] = height`, rest 0.
    fn impulse_samples(height: i16) -> Vec<i16> {
        let mut samples = vec![0i16; SAMPLE_FRAMES];
        samples[0] = height;
        samples
    }

    /// A tone at DFT bin `bin_index`, the shape the visualiser sees.
    fn tone_samples(bin_index: usize, amplitude: f64) -> Vec<i16> {
        (0..SAMPLE_FRAMES)
            .map(|index| {
                let phase = 2.0 * PI * (bin_index as f64 * index as f64 / SAMPLE_FRAMES as f64);
                (phase.sin() * amplitude) as i16
            })
            .collect()
    }

    /// The window interleave of `Main.cpp:110-114` as a plain function.
    fn windowed(samples: &[i16]) -> Vec<f32> {
        let window = build_window(SAMPLE_FRAMES);
        let half = SAMPLE_FRAMES / 2;
        let mut data = vec![0.0f32; SAMPLE_FRAMES];
        for i in 0..half {
            data[i] = samples[i] as f32 * window[i];
            data[i + half] = samples[i + half] as f32 * window[half - i - 1];
        }
        data
    }

    /// The definition of the transform the reference calls
    /// (`fftsg.c:196-208`), evaluated directly in f64: `S[k]` for k = 1..=len,
    /// with `S[len]` at index 0 and `S[k]` at index k.
    fn dst_by_definition(windowed: &[f32]) -> Vec<f64> {
        let n = windowed.len();
        let mut out = vec![0.0f64; n];
        for k in 1..=n {
            let mut sum = 0.0f64;
            for (j, value) in windowed.iter().enumerate() {
                sum += *value as f64 * (PI * (j as f64 + 0.5) * k as f64 / n as f64).sin();
            }
            out[if k == n { 0 } else { k }] = sum;
        }
        out
    }

    #[test]
    fn the_dst_of_an_impulse_matches_the_closed_form() {
        // With `a[0] = h` and everything else zero, `S[k] = a[0]*sin(pi*k/(2*len))`
        // — hand-computable, and the check for the impulse the mission names.
        let samples = impulse_samples(16_384);
        let windowed = windowed(&samples);
        let mut fft = windowed.clone();
        dst_ii(&mut fft);
        let scale = windowed[0] as f64;
        assert!(scale != 0.0, "the windowed impulse must not be zero");
        for (k, value) in fft.iter().enumerate().skip(1) {
            let expected = scale * (PI * k as f64 / (2.0 * SAMPLE_FRAMES as f64)).sin();
            assert!(
                close(*value, expected),
                "bin {k}: {value} vs closed form {expected}",
            );
        }
        // `S[len]` lands at index 0: `sin(pi*len/(2*len)) = 1`.
        assert!(close(fft[0], scale), "S[len] lands at index 0");
    }

    #[test]
    fn the_dst_matches_a_direct_evaluation_of_the_definition() {
        for (name, samples) in [
            ("impulse", impulse_samples(-32_768)),
            ("random", pseudo_random_samples(0x5eed)),
        ] {
            let windowed = windowed(&samples);
            let expected = dst_by_definition(&windowed);
            let mut fft = windowed.clone();
            dst_ii(&mut fft);
            for (index, (got, want)) in fft.iter().zip(expected.iter()).enumerate() {
                assert!(
                    close(*got, *want),
                    "{name}: bin {index} is {got}, direct evaluation says {want}",
                );
            }
        }
    }

    /// A single tone checked against an independent scratch computation: the
    /// pure-Python script (run with `python3 -`) evaluates the definitions on
    /// this page in f64 — window, half-interleave, the DST sum, the band edges
    /// and the dB mapping — and reports
    ///
    /// ```text
    /// tone bin 400 peaks at bin 800 (a tone at DFT bin f lands at DST bin 2f)
    /// 40 bands, cut 3.7, maxval 19: heights = [0]*32 ++ [1, 17] ++ [0]*6
    ///   (band 32 covers bins 673..773, band 33 bins 773..888)
    /// silence, count=16, maxval=800: heights = [0]*16
    /// ```
    ///
    /// The constants below are that output; `th` and the fall are the caller's
    /// (a 64x32 layer at `height = 20`: falldown `(20+15)/16 = 2`, peak hold
    /// `(20+31)/32 = 1`).
    #[test]
    fn the_band_heights_of_a_single_tone_match_the_scratch_computation() {
        let mut analyzer = Analyzer::new();
        analyzer.samples = tone_samples(400, 16_384.0);
        analyzer.do_fft();

        let peak_bin = (1..SAMPLE_FRAMES)
            .max_by(|left, right| {
                analyzer.fft[*left]
                    .abs()
                    .total_cmp(&analyzer.fft[*right].abs())
            })
            .expect("bins");
        assert!(
            peak_bin.abs_diff(800) <= 1,
            "the transform peaks at bin {peak_bin}, not at the tone's 2*400",
        );

        analyzer.get_band_spectrum(40, 3.7, -70.0, 19, 2, 30, 1);
        let mut expected = vec![0i32; 40];
        expected[32] = 1;
        expected[33] = 17;
        let heights: Vec<i32> = analyzer.bands.iter().map(|band| band.data).collect();
        assert_eq!(heights, expected, "band heights");
        // One frame after a zero history the peak equals the height.
        let peaks: Vec<i32> = analyzer.bands.iter().map(|band| band.peak).collect();
        assert_eq!(peaks, expected, "band peaks");
    }

    /// The silence state's band heights are all zero (`vi = maxval*(1 - 1)`),
    /// from the same scratch run.
    #[test]
    fn silence_keeps_every_band_at_zero() {
        let mut analyzer = Analyzer::new();
        analyzer.samples = vec![0; SAMPLE_FRAMES];
        analyzer.do_fft();
        analyzer.get_band_spectrum(16, 8.0, -70.0, 800, 40, 40, 10);
        assert!(analyzer.bands.iter().all(|band| band.data == 0));
    }

    /// An engine with the plugin installed and one 64x32 layer filled opaque
    /// white, plus a sound object whose `getVisBuffer` records its arguments
    /// into globals and reports a short read (the reference's `Main.cpp:48-63`
    /// shape). The four arguments go into separate globals — a dictionary
    /// literal is not usable for this, and unqualified reads inside a method
    /// body do not reach the global object (see the M105 finding).
    fn engine_with_layer() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(FftGraphPlugin).expect("plugin");
        engine
            .execute_script(
                "layer.tjs",
                r#"
                global.layer = new Layer();
                layer.setImageSize(64, 32);
                layer.fillRect(0, 0, 64, 32, 0xffffffff);
                layer.visible = true;

                global.visPtr = -1;
                global.visCount = -1;
                global.visChannels = -1;
                global.visAhead = -1;
                global.sound = %[];
                sound.getVisBuffer = function(dest, numsamples, channels, ahead) {
                    global.visPtr = typeof dest;
                    global.visCount = numsamples;
                    global.visChannels = channels;
                    global.visAhead = ahead;
                    return 0;
                };
                "#,
            )
            .expect("script");
        engine
    }

    fn layer_handle(engine: &KrkrEngine) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member("layer")
            .object_handle()
            .expect("layer handle")
    }

    fn pixel(engine: &mut KrkrEngine, x: u32, y: u32) -> u32 {
        engine
            .execute_expression("pixel.tjs", &format!("layer.getMainPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer") as u32
    }

    fn mask_pixel(engine: &mut KrkrEngine, x: u32, y: u32) -> u32 {
        engine
            .execute_expression("mask.tjs", &format!("layer.getMaskPixel({x}, {y})"))
            .expect("mask pixel")
            .to_integer()
            .expect("integer") as u32
    }

    #[test]
    fn registers_the_global_and_reports_itself() {
        let engine = {
            let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
            engine.register_plugin(FftGraphPlugin).expect("plugin");
            engine
        };
        assert_eq!(catalog::canonical_name(NAME), Some(NAME));
        assert!(
            engine.host().linked_plugins().any(|linked| linked == NAME),
            "the plugin does not register the name the catalog resolves",
        );
        assert!(
            matches!(
                engine.tjs_runtime().global_member(FUNCTION_NAME),
                Variant::Object(_)
            ),
            "drawFFTGraph is not installed as a global",
        );
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains(FUNCTION_NAME)),
            "no registration line",
        );
    }

    #[test]
    fn the_call_shape_is_the_references() {
        let mut engine = engine_with_layer();
        engine
            .execute_script("draw.tjs", "drawFFTGraph(layer, sound, 2, 4, 40, 20);")
            .expect("draw");
        let args: Vec<i64> = ["visCount", "visChannels", "visAhead"]
            .iter()
            .map(|name| {
                engine
                    .execute_expression("arg.tjs", name)
                    .expect("arg")
                    .to_integer()
                    .expect("integer")
            })
            .collect();
        assert_eq!(
            args,
            vec![SAMPLE_FRAMES as i64, 1, 0],
            "getVisBuffer(dest, 2048, 1, 0)",
        );
        let destination = engine
            .execute_expression("arg.tjs", "visPtr")
            .expect("arg")
            .to_tjs_string()
            .expect("string");
        assert_eq!(
            destination, "Object",
            "the destination is the array the engine fills (the reference lends a `short*`)",
        );
    }

    /// Row-24 pin: with the playing buffer's decoded PCM reaching
    /// `GetVisBuffer`, the graph stops drawing the silence state — a tone
    /// paints fire pixels above the rect's bottom row.
    #[test]
    fn a_playing_buffer_paints_a_real_spectrum() {
        use std::sync::Arc;

        use krkr_engine::plugin_api::audio::{
            AudioInstanceId, PcmAudioSpec, WavePcmRequest, WavePcmSource, WavePcmState,
            WavePcmWindow, clear_wave_pcm_source, set_wave_pcm_source,
        };

        struct Tone;

        impl WavePcmSource for Tone {
            fn read_window(
                &self,
                _id: AudioInstanceId,
                request: WavePcmRequest,
            ) -> Option<WavePcmWindow> {
                // A 1 kHz tone at a third of full scale, both channels.
                let samples = (0..request.frames)
                    .flat_map(|index| {
                        let phase = std::f32::consts::TAU * 1_000.0 * index as f32 / 44_100.0;
                        let value = phase.sin() / 3.0;
                        [value, value]
                    })
                    .collect();
                Some(WavePcmWindow {
                    spec: PcmAudioSpec {
                        sample_rate: 44_100,
                        channels: 2,
                    },
                    state: WavePcmState::Playing,
                    samples,
                    available_frames: request.frames,
                })
            }
        }

        let _guard = crate::wf_basic_effect::lock_pcm_source();
        let mut engine = engine_with_layer();
        set_wave_pcm_source(Arc::new(Tone));
        engine
            .execute_script(
                "tone.tjs",
                r#"
                global.buffer = new WaveSoundBuffer();
                buffer.open("tone.ogg");
                buffer.play();
                drawFFTGraph(layer, buffer, 2, 4, 40, 20);
                "#,
            )
            .expect("draw");
        clear_wave_pcm_source();

        // The silence state paints only the bottom row's peak pixels; the tone
        // must paint fire pixels above it.
        let painted = (5..23).any(|y| (2..42).any(|x| pixel(&mut engine, x, y) != 0));
        assert!(
            painted,
            "a playing buffer's spectrum must paint above the silence row"
        );
    }

    #[test]
    fn draws_the_silence_state_of_type_zero() {
        let mut engine = engine_with_layer();
        engine
            .execute_script("draw.tjs", "drawFFTGraph(layer, sound, 2, 4, 40, 20);")
            .expect("draw");

        // All bands sit at `db = mindb`, so `vi = 0`: no fire pixels, and the
        // peak pixel (distance 0 from the bottom row) paints the rect's bottom
        // row grey — the reference's `0xff808080`.
        assert_eq!(pixel(&mut engine, 2, 23), 0x80_8080, "bottom row start");
        assert_eq!(pixel(&mut engine, 41, 23), 0x80_8080, "bottom row end");
        assert_eq!(pixel(&mut engine, 2, 22), 0, "the row above is transparent");
        assert_eq!(
            pixel(&mut engine, 41, 4),
            0,
            "the rect's top row is transparent"
        );
        // The alpha byte is written too: `getMainPixel` reads RGB, the mask
        // reads alpha, and the reference's peak colour is opaque.
        assert_eq!(
            mask_pixel(&mut engine, 2, 23),
            0xff,
            "the peak pixel is opaque"
        );
        assert_eq!(
            mask_pixel(&mut engine, 2, 22),
            0,
            "the transparent row is clear"
        );
        // Outside the rect the layer keeps its fill.
        assert_eq!(pixel(&mut engine, 1, 23), 0xff_ffff, "left of the rect");
        assert_eq!(pixel(&mut engine, 42, 23), 0xff_ffff, "right of the rect");
        assert_eq!(pixel(&mut engine, 2, 24), 0xff_ffff, "below the rect");
        assert_eq!(pixel(&mut engine, 2, 3), 0xff_ffff, "above the rect");

        // The reference's closing `layer.update(...)` (`Main.cpp:491`).
        assert_eq!(
            engine
                .execute_expression("update.tjs", "layer.callOnPaint")
                .expect("callOnPaint")
                .to_integer()
                .expect("integer"),
            1,
        );
    }

    #[test]
    fn draws_the_lcd_pattern_of_type_one() {
        let mut engine = engine_with_layer();
        engine
            .execute_script(
                "draw.tjs",
                "drawFFTGraph(layer, sound, 0, 0, 32, 16, %[type: 1]);",
            )
            .expect("draw");

        // Defaults: division 16, thick 2, so `bw = 32/16 = 2` columns per cell
        // and `th = 1000/(16/2) = 125`. Every band reads 0, so the first group
        // (nearest the bottom) draws the peak colour 0xff707070 (`peak 0 <= v 0`)
        // and every later group the off colour 0xffb0b0b0. Each group is two
        // rows: the lower one all-bgcolor, the upper one column 0 bgcolor and
        // the other column coloured.
        for row in [1, 3, 5, 7, 9, 11, 13, 15] {
            assert_eq!(pixel(&mut engine, 0, row), 0xc0_c0c0, "bg row {row}");
            assert_eq!(pixel(&mut engine, 1, row), 0xc0_c0c0, "bg row {row}");
        }
        for row in [0, 2, 4, 6, 8, 10, 12] {
            assert_eq!(
                pixel(&mut engine, 0, row),
                0xc0_c0c0,
                "column 0 of row {row}"
            );
            assert_eq!(
                pixel(&mut engine, 1, row),
                0xb0_b0b0,
                "off cell of row {row}"
            );
        }
        assert_eq!(pixel(&mut engine, 0, 14), 0xc0_c0c0, "peak row column 0");
        assert_eq!(pixel(&mut engine, 1, 14), 0x70_7070, "the peak cell");
        // The last cell (index 15) starts at column 30.
        assert_eq!(pixel(&mut engine, 30, 14), 0xc0_c0c0, "last cell column 0");
        assert_eq!(pixel(&mut engine, 31, 14), 0x70_7070, "last cell peak");
    }

    #[test]
    fn type_one_options_override_the_defaults() {
        let mut engine = engine_with_layer();
        engine
            .execute_script(
                "draw.tjs",
                "drawFFTGraph(layer, sound, 0, 0, 4, 4, \
                 %[type: 1, division: 2, thick: 2, oncolor: 0xff112233, \
                   offcolor: 0xff445566, bgcolor: 0xff778899, peakcolor: 0xffaabbcc]);",
            )
            .expect("draw");
        // height 4, thick 2 → `th = 1000/(4/2) = 250`; two groups. Bottom group:
        // row 3 all bg, row 2 column 0 bg + peak. Top group: row 1 all bg,
        // row 0 column 0 bg + off.
        assert_eq!(pixel(&mut engine, 0, 3), 0x77_8899, "bg row");
        assert_eq!(pixel(&mut engine, 1, 3), 0x77_8899, "bg row");
        assert_eq!(pixel(&mut engine, 0, 2), 0x77_8899, "column 0");
        assert_eq!(pixel(&mut engine, 1, 2), 0xaa_bbcc, "peak cell");
        assert_eq!(pixel(&mut engine, 1, 1), 0x77_8899, "bg row");
        assert_eq!(pixel(&mut engine, 1, 0), 0x44_5566, "off cell");
        assert_eq!(pixel(&mut engine, 3, 2), 0xaa_bbcc, "second cell peak");
        assert_eq!(pixel(&mut engine, 3, 0), 0x44_5566, "second cell off");
    }

    #[test]
    fn type_one_uses_the_on_color_when_the_peak_color_is_the_off_color() {
        let mut engine = engine_with_layer();
        // `peakcolor != offcolor` is part of the peak condition (`Main.cpp:269`):
        // with the two equal, the first group falls through to `data >= v`
        // (`0 >= 0`), which draws the on colour.
        engine
            .execute_script(
                "draw.tjs",
                "drawFFTGraph(layer, sound, 0, 0, 4, 4, \
                 %[type: 1, division: 2, thick: 2, oncolor: 0xff112233, \
                   offcolor: 0xff445566, peakcolor: 0xff445566]);",
            )
            .expect("draw");
        assert_eq!(
            pixel(&mut engine, 1, 2),
            0x11_2233,
            "on colour in the peak slot"
        );
    }

    #[test]
    fn thickness_one_draws_only_background_rows() {
        let mut engine = engine_with_layer();
        // With thick 1 the inner `for(yy = 1; yy < thickness; yy++)` never runs,
        // so only the all-bg row of each group is written — the reference's own
        // edge case (`Main.cpp:272-296`).
        engine
            .execute_script(
                "draw.tjs",
                "drawFFTGraph(layer, sound, 0, 0, 4, 4, \
                 %[type: 1, division: 2, thick: 1, bgcolor: 0xff778899]);",
            )
            .expect("draw");
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(pixel(&mut engine, x, y), 0x77_8899, "({x}, {y})");
            }
        }
    }

    #[test]
    fn an_unhandled_type_draws_nothing() {
        let mut engine = engine_with_layer();
        engine
            .execute_script(
                "draw.tjs",
                "drawFFTGraph(layer, sound, 0, 0, 8, 8, %[type: 7]);",
            )
            .expect("draw");
        assert_eq!(pixel(&mut engine, 0, 0), 0xff_ffff, "untouched");
    }

    #[test]
    fn fewer_than_six_arguments_is_a_bad_parameter_count() {
        let mut engine = engine_with_layer();
        let error = engine
            .execute_script("draw.tjs", "drawFFTGraph(layer, sound, 0, 0, 8);")
            .expect_err("a short call must fail");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        assert_eq!(error.kind.tjs_error_code(), Some(-1004));
    }

    #[test]
    fn a_freed_layer_image_is_not_drawable() {
        let mut engine = engine_with_layer();
        engine
            .execute_script("free.tjs", "layer.freeImage();")
            .expect("freeImage");
        let error = engine
            .execute_script("draw.tjs", "drawFFTGraph(layer, sound, 0, 0, 8, 8);")
            .expect_err("no image to draw into");
        assert_eq!(error.message, "Not drawable layer type");
    }

    #[test]
    fn writes_outside_the_bitmap_are_dropped() {
        let mut engine = engine_with_layer();
        // A rect that starts at the right edge and runs past it: the in-bounds
        // rows it covers are painted, the out-of-bounds ones are dropped, and
        // nothing panics.
        engine
            .execute_script("draw.tjs", "drawFFTGraph(layer, sound, 60, 28, 16, 8);")
            .expect("draw");
        assert_eq!(
            pixel(&mut engine, 63, 31),
            0,
            "the covered bottom row is painted"
        );
        assert_eq!(
            pixel(&mut engine, 60, 28),
            0,
            "the covered top row is painted"
        );
        assert_eq!(pixel(&mut engine, 59, 31), 0xff_ffff, "left of the rect");
        assert_eq!(pixel(&mut engine, 63, 27), 0xff_ffff, "above the rect");
    }

    #[test]
    fn the_bitmap_view_is_committed() {
        let mut engine = engine_with_layer();
        let layer = layer_handle(&engine);
        let before = layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            view.bitmap.generation
        })
        .expect("generation");
        engine
            .execute_script("draw.tjs", "drawFFTGraph(layer, sound, 2, 4, 40, 20);")
            .expect("draw");
        let after = layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            view.bitmap.generation
        })
        .expect("generation");
        assert_ne!(before, after, "the drawing commits a new image");
    }

    /// The window itself, against the reference's construction
    /// (`Main.cpp:101-106`): `sin(pi*(i+0.5)/len) * mul`, `mul = 4/32768/len`.
    #[test]
    fn the_window_matches_the_reference_construction() {
        let window = build_window(SAMPLE_FRAMES);
        let mul = (4.0f64 / 32768.0 / SAMPLE_FRAMES as f64) as f32;
        for index in [0usize, 1, 511, 1023, 2047] {
            let expected =
                ((PI * (index as f64 + 0.5) / SAMPLE_FRAMES as f64).sin() * mul as f64) as f32;
            assert_eq!(window[index], expected, "window[{index}]");
        }
        // The half-interleave the reference applies at `:110-114` mirrors the
        // lower half of the window over the upper half of the samples.
        let samples: Vec<i16> = (0..SAMPLE_FRAMES).map(|index| index as i16).collect();
        let data = windowed(&samples);
        for index in [0usize, 511, 1023] {
            assert_eq!(data[index], samples[index] as f32 * window[index]);
            assert_eq!(
                data[index + 1024],
                samples[index + 1024] as f32 * window[1023 - index],
            );
        }
    }
}
