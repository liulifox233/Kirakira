//! `wuvorbis.dll` — Ogg Vorbis (`*.ogg`) codec registration.
//!
//! The reference plugin (krkrz `src/plugins/win32/wuvorbis/`) is a TVP Sound
//! System media module, not a TJS surface: `V2Link` reads `-vorbis_gain`,
//! `-vorbis_pcm_format` and `-vorbis_rg` from the command line, and the module
//! answers `GetSupportExts` with `.ogg` / "Ogg Stream Format" so every
//! `WaveSoundBuffer` open of an `.ogg` storage gets a decoder.
//!
//! This module is that registration: the name, the extension and the three
//! options. Decoding itself already lives in `krkr-audio` (kira's `ogg` +
//! `vorbis` features over Symphonia), so an `.ogg` storage — loose file or XP3
//! member — plays with no TJS surface installed here.
//!
//! Option semantics follow `WuVorbisMainUnit.cpp:562`: `-vorbis_gain` is read
//! through a TJS2 real conversion, `-vorbis_pcm_format` recognises only the
//! exact string `f32` (the default is `i16`), and `-vorbis_rg` accepts
//! `none`/`no`, `track` (the default) and `album`, silently ignoring anything
//! else. The values are parsed and reported, but the native pipeline is always
//! f32 and applies neither gain nor ReplayGain — see [`META`]'s notes.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Ogg Vorbis (*.ogg) codec registration (TSS media module, no TJS surface) and the -vorbis_gain / -vorbis_pcm_format / -vorbis_rg options",
    notes: "`.ogg` playback is real through the native loader (krkr-audio: kira ogg+vorbis). Registration and the three command-line options are real too — reference defaults, and unknown values ignored exactly as the reference ignores them — but the option effects are mapped only: the native pipeline always decodes to f32 and applies no gain and no ReplayGain.",
    install: |engine| engine.register_plugin(WuVorbisPlugin),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what a game's
/// `Plugins.link("wuvorbis.dll")` resolves through [`crate::catalog`].
pub(crate) const NAME: &str = "wuvorbis.dll";

pub struct WuVorbisPlugin;

impl KrkrPlugin for WuVorbisPlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The reference reads its options in `V2Link`, before the engine hands
        // the module any storage. The process command line is the same source
        // `KrkrHost` itself parses for `System.getArgument("-vorbis_gain")`.
        let options = VorbisOptions::from_arguments(std::env::args().skip(1));
        for line in options.registration_log() {
            runtime.host_mut().log(&line);
        }
        Ok(())
    }
}

/// One entry of the reference's `GetSupportExts` answer, mapped to the loader
/// that decodes it in this engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MediaSupport {
    /// Extension the module answers for, `.` included.
    pub(crate) extension: &'static str,
    /// The reference's media short name (`GetSupportExts` out parameter).
    pub(crate) media_type: &'static str,
    /// Which loader in this engine decodes the extension.
    pub(crate) loader: &'static str,
    /// `krkr-audio` feature the loader needs; `None` when it is always built.
    pub(crate) loader_feature: Option<&'static str>,
}

pub(crate) const MEDIA_SUPPORT: &[MediaSupport] = &[MediaSupport {
    extension: ".ogg",
    media_type: "Ogg Stream Format",
    loader: "krkr-audio (kira ogg+vorbis)",
    loader_feature: None,
}];

/// `-vorbis_pcm_format`: the reference extracts i16 samples unless the option
/// value is exactly `f32` (`WuVorbisMainUnit.cpp:603`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PcmFormat {
    I16,
    F32,
}

impl PcmFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::I16 => "i16",
            Self::F32 => "f32",
        }
    }
}

/// `-vorbis_rg`: the reference looks up ReplayGain on by default and switches
/// to album gain only for the exact value `album` (`WuVorbisMainUnit.cpp:615`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReplayGain {
    Track,
    Album,
    None,
}

impl ReplayGain {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::None => "none",
        }
    }
}

/// The option set `V2Link` ends up with, with the reference's defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VorbisOptions {
    /// `-vorbis_gain` in dB; 0 when the option is absent.
    pub(crate) gain_db: f64,
    pub(crate) pcm_format: PcmFormat,
    pub(crate) replay_gain: ReplayGain,
}

impl Default for VorbisOptions {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            pcm_format: PcmFormat::I16,
            replay_gain: ReplayGain::Track,
        }
    }
}

impl VorbisOptions {
    /// Reads the three options out of a command line, the way `V2Link` does:
    /// the first `-name`/`-name=value` token wins (`TVPGetCommandLine` walks
    /// `TVPProgramArguments` in order), a bare flag reads as `"yes"`, and every
    /// unknown value leaves the option at its default.
    pub(crate) fn from_arguments(arguments: impl IntoIterator<Item = String>) -> Self {
        let arguments: Vec<String> = arguments.into_iter().collect();
        let mut options = Self::default();
        if let Some(value) = first_option_value(&arguments, "-vorbis_gain") {
            options.gain_db = tjs_real(&value);
        }
        if let Some(value) = first_option_value(&arguments, "-vorbis_pcm_format")
            && value == "f32"
        {
            options.pcm_format = PcmFormat::F32;
        }
        if let Some(value) = first_option_value(&arguments, "-vorbis_rg") {
            match value.as_str() {
                "none" | "no" => options.replay_gain = ReplayGain::None,
                "track" => options.replay_gain = ReplayGain::Track,
                "album" => options.replay_gain = ReplayGain::Album,
                _ => {}
            }
        }
        options
    }

    /// What `register` reports to the engine log: the media support this module
    /// answers for and the loader behind it, then the option set this build
    /// ended up with.
    pub(crate) fn registration_log(self) -> [String; 2] {
        let support = MEDIA_SUPPORT[0];
        let feature = match support.loader_feature {
            Some(feature) => format!(", requires krkr-audio's \"{feature}\" feature"),
            None => String::new(),
        };
        [
            format!(
                "wuvorbis: Ogg Vorbis (*.ogg) decoder registered (TSS media module, no TJS \
                 surface): \"{}\" ({}) -> {}{}.",
                support.extension, support.media_type, support.loader, feature,
            ),
            format!(
                "wuvorbis: options -vorbis_gain={}dB -vorbis_pcm_format={} -vorbis_rg={} \
                 (reference defaults; the native pipeline is f32-only and applies no gain or \
                 ReplayGain).",
                self.gain_db,
                self.pcm_format.as_str(),
                self.replay_gain.as_str(),
            ),
        ]
    }
}

/// The value of a `-name` / `-name=value` token, `"yes"` for the bare form —
/// `TVPGetCommandLine`'s contract (`SysInitImpl.cpp:1626`). A token that only
/// starts with the name (`-vorbis_gainx=1`) is not the option.
fn option_value(argument: &str, name: &str) -> Option<String> {
    match argument.strip_prefix(name)? {
        "" => Some("yes".to_string()),
        rest => rest.strip_prefix('=').map(str::to_string),
    }
}

/// The first of `name`'s values on the command line, the one
/// `TVPGetCommandLine` answers with.
fn first_option_value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .iter()
        .find_map(|argument| option_value(argument, name))
}

/// `(tTVReal)` of a command-line value, as TJS2 converts it
/// (`TJSStringToReal` -> `TJSParseNumber`): a sign, then the longest numeric
/// prefix, with trailing text ignored. A value without digits reads as 0,
/// which is what a bare `-vorbis_gain` ("yes") becomes — the reference logs
/// 0 dB for it. The word forms TJS2 also accepts (`true`, `NaN`, …) and the
/// radix forms (`0x10`) are not modelled: neither is a meaningful gain.
fn tjs_real(value: &str) -> f64 {
    let (text, sign) = match value.trim_start().as_bytes().first() {
        Some(b'-') => (&value.trim_start()[1..], -1.0),
        Some(b'+') => (&value.trim_start()[1..], 1.0),
        _ => (value.trim_start(), 1.0),
    };
    sign * decimal_prefix(text)
}

/// The numeric prefix of `text` (digits, at most one dot, one well-formed
/// exponent), or 0 when there are no digits.
fn decimal_prefix(text: &str) -> f64 {
    let bytes = text.as_bytes();
    let mut end = 0;
    let mut seen_dot = false;
    while let Some(byte) = bytes.get(end) {
        match byte {
            b'0'..=b'9' => end += 1,
            b'.' if !seen_dot => {
                seen_dot = true;
                end += 1;
            }
            _ => break,
        }
    }
    if let Some(b'e' | b'E') = bytes.get(end) {
        let mut exponent = end + 1;
        if let Some(b'+' | b'-') = bytes.get(exponent) {
            exponent += 1;
        }
        if bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
            while bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
                exponent += 1;
            }
            end = exponent;
        }
    }
    text[..end].parse().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use krkr_audio::AudioSystem;
    use krkr_core::{
        AudioBus, AudioCommand, AudioEvent, AudioInstanceId, AudioLoadPolicy, AudioSourceRef,
        ResourceStream, StoragePort,
    };
    use krkr_engine::{EngineConfig, KrkrEngine};

    use super::*;
    use crate::catalog;

    /// A real Ogg Vorbis stream: a 440 Hz stereo sine, 44.1 kHz, 30 ms, written
    /// by ffmpeg's Vorbis encoder —
    /// `ffmpeg -f lavfi -i "sine=frequency=440:duration=0.03" -ar 44100 -ac 2
    /// -c:a vorbis -strict -2 -q:a 0 smoke.ogg`.
    const VORBIS_FIXTURE_HEX: &str = "\
        4f67675300020000000000000000fa1d9c9c00000000455b6857011e01766f72626973000000000244ac0000000000000000000000000000bb014f67\
        675300000000000000000000fa1d9c9c01000000347c871b0e3dffffffffffffffffffffffffbb03766f726269730d0000004c61766636322e31322e\
        313031010000001c000000656e636f6465723d4c61766336322e32382e31303120766f726269730105766f726269731c424356020010000084749a59\
        aa0122cc408681d0909500000200006084220c312034642500001000002086928368426bce37e738689683a6526c4e0727526d9ee4a6626ece39e79c\
        73b239678c73ce39a72867168366426bce39273168968266426bce39e7496c1eb4a64a6bce39679c733a18678471ce39a7496b1ea466636dce396741\
        6b9aa3e6526cce3927526e9ed4e6526dce39e79c73ce39e79c73ce39a77a713a07e78473ce39276a6faee5267471ce39e79371ba37278473ce39e79c\
        73ce39e79c73ce3927080d5909000001001084616318770a82f4391a88518498864c7ad03d3a4c82c620a7907a343a1a29a50e4249659c94d2094243\
        56020080000010424821851452482185145248218518628821869c72ca29a8a0924a2aaa28a3cc32cb2cb3cc32cb2cb30e3bebacc30e430c31c4d04a\
        2bb1d4545b8d35d69a7bceb9e620ad95d65a6bad94524a29a59482d09095000008000081904106196414524821851862ca29a79c820a2a2034642500\
        000600c02167a081061a68a081061a68a071c6198820820822a8a4924c3a0a29b5d86acc31d75e830e3af79e7befb9f81c84524a29a594524a29a594\
        524a2925080d59090080000000082184105248218514528a31c61c730e3a09250442435602006000000c31c41864904148218518628a29c71c730c3a\
        082194525268a1855c6a882596565a89a5a5986a8bb1d65873ed31d6de7befbdf7de7befbdf7de7bce81d090950040040000830c228820828c310621\
        048486ac040040000010628831c6208410528821a79c824c32e9a4a39002a1212b01002700008411472471041267a081082aa920a3cc422cb1b5d65a\
        6badb5d65a6badb5d65a6badb5d65a6badb5d65a6badb5d65a6b2d101ab21200880000609041061944104104196480d090950000080000238c400419\
        a514638e39e61874d041271d85165a2034642500e004004020a18832cc30041155545146155514524729a594524a29a594524a29a594524a29a59452\
        4a29a594524a29a5544a29a504424356020064000090a29452292d458222a518a4184b461573505a8aa8720c52cda952ce20e6249688318494935432\
        e614420c42ea1c754c29062d951842c618a4d8724ba1730e080d592100846600381c07902c0b902c0b00000000000000244d0334cf032ccd03000000\
        0000000049d300cbd300cdf3000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000090340dd03c0fd03c0f0000000000000034cf033c4f043c510400000000000000cbf3004df4004f14\
        010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000090340dd03c0fd03c0f000000000000002ccf033c5104344f0400000000000000cbf3004f14014ff4000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        0000000400000a3800000a58088586ac0800e204001c920449822441f300926541d3a069304d806459d034681a4c130000000000000000000049d3a0\
        69d034882240d234681a340da20800000000000000000080a469d034681a441120691a340d9a065104000000000000000000c0334d88224411a609f0\
        4c13a2085184690200000000000000000000000000000000000000000000020000091c0000054c28038586ac0800e204001c8e62590000e0388e6501\
        0080e338960500009665892200005896268a000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        0000000000000000000000000000000000000000000000020000091c0000054c28038586ac0400a200001c8a6259c0712c0b388e650149b22c806501\
        340fa069005104000200000d1c0000056cd094581ca0d09095004014008041712c4bd3449124699ae6892249d234cf13459ae6799e679af03ccf334d\
        88a2289a264451144d13a6699aaa0a4c53550500001a3800000ad8a029b13840a1212b018090000087a25896a6799ee789a269aa2649d234cf134551\
        344dd3545592a4699e278aa2689aa6a9aa2c4bd33c4f1445d134555555a1699e278aa2689aaaaabaf03ccf134551344d55755d789ee789a2289aa6aa\
        ba2e4451144dd3345553555d1788a2699aa6aaaaaaeb02d11345d35455d7755de079a2689aaaeaaaae0b44d3345555555d579601a6699aaaeabab20c\
        505555755dd7956580aaaaaaebbaae2c0354d5755d5796651980ebbaae2ccbb2000040040700400123e824a3ca226c34e1c2035068c88a00200a0000\
        308629c594328c490829848631092185904949a9b4942a08a994544a052195924ac928a5945a4a1584544a2aa582904a49a51400008be000008b6021\
        141ab21200c80300208c518a31c69c930829c59873ce49849462cc39e7a4528c39e79c73524ac61c73ce3929a573ce39e79c949239e79c734e4ae99c\
        73ce3927a594d239e79c93524a09a173d04929a574ce39e70400801a3800000ad828b239c14850a1212b018054000083e35896a6799e289aa625499a\
        e6799e278aa6a94992a6799ee789a26af23ccf134551344d55e5799e278aa2689aaaca7545d1344d535555972c8ba2699aa6aaba2e4cd33455d5755d\
        17a6699aaaeabaae0bdb565555755d59866dabaaaabaae2c03d7755d59b66520cbae2bbbb62c00005ec10100d4c086d5114e8ac6020b0d5909006400\
        0010c620a4104248198490420821a51442020000091c0000054c28038586ac04005201000063acb5d65a6bad35d0596badb5d65a2b20b3d65a6badb5\
        d65a6badb5d65a6badb5d45a6badb5d65a6badb5d65a6badb5d65a6badb5d65a6badb5d65a6badb5d65a6badb5d65a6badb5d65a6badb5d65a6badb5\
        d65a4b29a594524a29a594524a29a594524a29a594520140bf160e00ff1036ac8e705234165868c84a00201c0000304629c61c83504a2915428c3927\
        1d95d662ac1062cc390929b5165bf19c73104a48a5b5188be79c83504a4ab1d558540aa19494528b2dd6a252e8a8a494526b3516634c2aa9b5d662ab\
        b11863520a2db5d6628cc5085b536a2db6da6a2cc6d89a4a0b2dc6186331c217195b8ba9b65a8331c2c8164b4bb5d61a8c3146f7d662a9ade6628c0f\
        beb6144b8c351700e0eee00080a860e30c2b496785a3c18586ac0400420200088494628c31c69c73ce39a914638e39e79c8310422895628c31e79c83\
        10420825638c39e71c84104208a1949231e71c8410420821a4943ae71c841042082184524ae79c831042082184504ae920841042082184124a292985\
        104208218410422a29a5104208a194104a4825a51442082184504a0929a5944208a19410422821a594524a2184104229a5a494524aa984524209a184\
        544a4a2985124208a59492524a2995524228a184524a4929a59452082184524a010080080e00800246d049469545d868c28507200000000400200891\
        192251b0000c0e540042c214005058609003000d0e0f691717d065800bbab8eb4008410842108b032820010727dcf0c41b9e708313748a4a0d080000\
        00000019007c0000240f4044443473101112131415161718191a1b1c000080000200001000000000000800000000104f676753000440050000000000\
        00fa1d9c9c02000000d303a2ca031e1e1efeff0700000000fcff010000000000000000000000000000000000000000feff0700000000fcff01000000\
        0000000000000000000000000000000000feff0700000000fcff010000000000000000000000000000000000000000";

    #[test]
    fn defaults_match_the_reference_option_descriptor() {
        let options = VorbisOptions::from_arguments(Vec::<String>::new());
        assert_eq!(options.gain_db, 0.0);
        assert_eq!(options.pcm_format, PcmFormat::I16);
        assert_eq!(options.replay_gain, ReplayGain::Track);
    }

    #[test]
    fn gain_is_read_through_a_tjs_real_conversion() {
        let gain = |arguments: &[&str]| {
            VorbisOptions::from_arguments(arguments.iter().map(|argument| argument.to_string()))
                .gain_db
        };
        assert_eq!(gain(&["-vorbis_gain=-10"]), -10.0);
        assert_eq!(gain(&["-vorbis_gain=-0.5"]), -0.5);
        assert_eq!(gain(&["-vorbis_gain=+3"]), 3.0);
        // The bare flag is answered as "yes", and TJS2 reads "yes" as 0.
        assert_eq!(gain(&["-vorbis_gain"]), 0.0);
        assert_eq!(gain(&["-vorbis_gain=loud"]), 0.0);
        // TJS2 keeps the numeric prefix and ignores what follows it.
        assert_eq!(gain(&["-vorbis_gain=-10dB"]), -10.0);
        assert_eq!(gain(&["-vorbis_gain=1e1"]), 10.0);
        // A token that merely starts with the option name is a different one.
        assert_eq!(gain(&["-vorbis_gainx=-10"]), 0.0);
        assert_eq!(gain(&["-vorbis_pcm_format=f32"]), 0.0);
    }

    #[test]
    fn pcm_format_only_recognises_the_exact_value_f32() {
        let format = |arguments: &[&str]| {
            VorbisOptions::from_arguments(arguments.iter().map(|argument| argument.to_string()))
                .pcm_format
        };
        assert_eq!(format(&["-vorbis_pcm_format=f32"]), PcmFormat::F32);
        assert_eq!(format(&["-vorbis_pcm_format=i16"]), PcmFormat::I16);
        assert_eq!(format(&["-vorbis_pcm_format=F32"]), PcmFormat::I16);
        assert_eq!(format(&["-vorbis_pcm_format=float"]), PcmFormat::I16);
        assert_eq!(format(&["-vorbis_pcm_format"]), PcmFormat::I16);
    }

    #[test]
    fn replay_gain_accepts_none_no_track_and_album_and_ignores_the_rest() {
        let replay_gain = |arguments: &[&str]| {
            VorbisOptions::from_arguments(arguments.iter().map(|argument| argument.to_string()))
                .replay_gain
        };
        assert_eq!(replay_gain(&["-vorbis_rg=album"]), ReplayGain::Album);
        assert_eq!(replay_gain(&["-vorbis_rg=track"]), ReplayGain::Track);
        assert_eq!(replay_gain(&["-vorbis_rg=none"]), ReplayGain::None);
        assert_eq!(replay_gain(&["-vorbis_rg=no"]), ReplayGain::None);
        // An unknown value leaves the option alone, as the reference does.
        assert_eq!(replay_gain(&["-vorbis_rg=off"]), ReplayGain::Track);
        // The first occurrence wins, like `TVPGetCommandLine`.
        assert_eq!(
            replay_gain(&["-vorbis_rg=album", "-vorbis_rg=none"]),
            ReplayGain::Album
        );
    }

    #[test]
    fn registers_under_the_catalog_name_and_reports_its_options() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(WuVorbisPlugin).expect("register");

        assert_eq!(catalog::canonical_name(NAME), Some(NAME));
        assert!(
            engine.host().linked_plugins().any(|linked| linked == NAME),
            "the plugin does not register the name the catalog resolves",
        );
        let support = MEDIA_SUPPORT[0];
        let logs = engine.host().logs();
        assert!(
            logs.iter().any(|line| line.contains("Ogg Vorbis (*.ogg)")
                && line.contains(support.extension)
                && line.contains(support.loader)),
            "no registration line with the media mapping: {logs:?}",
        );
        assert!(
            logs.iter()
                .any(|line| line.contains("-vorbis_pcm_format=i16")
                    && line.contains("-vorbis_rg=track")),
            "no option line: {logs:?}",
        );
    }

    /// The extension the module answers for is the one the native loader
    /// actually decodes: the smoke plays a real Ogg Vorbis stream under that
    /// extension, through the production `AudioSystem`.
    #[test]
    fn the_reference_extension_is_the_one_this_engine_decodes() {
        let [support] = MEDIA_SUPPORT else {
            panic!("wuvorbis registers exactly one extension");
        };
        assert_eq!(support.extension, ".ogg");
        assert_eq!(support.media_type, "Ogg Stream Format");
        let storage = format!("wuvorbis-smoke{}", support.extension);
        match decode_smoke(&storage, fixture_bytes(VORBIS_FIXTURE_HEX)) {
            DecodeSmoke::Decoded => {}
            DecodeSmoke::Skipped(reason) => {
                eprintln!("skipping the Ogg Vorbis decode smoke: {reason}");
            }
        }
    }

    /// M31: the workspace `kira`/`symphonia` feature list dropped `pcm` (a
    /// codec feature the `wav` feature does not imply, although kira's own
    /// default set carries it), which left every ordinary PCM `.wav`
    /// undecodable. `crates/krkr-audio`'s tests are outside this mission's
    /// scope, so the guard lives here, next to the codec registration that
    /// shares the same loader and feature set.
    #[test]
    fn decodes_a_pcm_wav_through_the_native_loader() {
        match decode_smoke("wuvorbis-smoke.wav", pcm_wav()) {
            DecodeSmoke::Decoded => {}
            DecodeSmoke::Skipped(reason) => {
                eprintln!("skipping the PCM WAV decode smoke: {reason}");
            }
        }
    }

    /// A PCM WAV: 16-bit mono, 8 kHz, 50 ms of a sine.
    fn pcm_wav() -> Vec<u8> {
        const SAMPLE_RATE: u32 = 8_000;
        const SAMPLES: usize = 400;
        let data_len = (SAMPLES * 2) as u32;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for index in 0..SAMPLES {
            let sample = ((index as f32 * 0.1).sin() * 8_000.0) as i16;
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }

    /// The bytes a fixture hex literal stands for.
    fn fixture_bytes(hex: &str) -> Vec<u8> {
        let bytes = hex.as_bytes();
        assert_eq!(bytes.len() % 2, 0, "fixture hex has an odd length");
        bytes
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| (byte as char).to_digit(16).expect("hex digit") as u8;
                (digit(pair[0]) << 4) | digit(pair[1])
            })
            .collect()
    }

    enum DecodeSmoke {
        Decoded,
        Skipped(String),
    }

    /// Plays `bytes` as the real `storage` through the production
    /// `AudioSystem` — the path a `WaveSoundBuffer` play command takes — and
    /// reports how the load went. `StaticUncached` keeps the load on the static
    /// loader, the path a `WaveSoundBuffer` open takes. An environment without
    /// an audio device skips instead of failing; anything else that keeps the
    /// registered format from decoding is a failure.
    fn decode_smoke(storage: &str, bytes: Vec<u8>) -> DecodeSmoke {
        let mut audio = AudioSystem::new();
        let provider = Arc::new(MemoryStorage {
            storage: storage.to_string(),
            bytes,
        });
        if let Err(error) = audio
            .prepare()
            .and_then(|()| audio.set_resource_provider(Some(provider)))
        {
            return DecodeSmoke::Skipped(format!("audio worker unavailable: {error}"));
        }
        let command = AudioCommand::Play {
            id: AudioInstanceId(1),
            bus: AudioBus::SoundEffect,
            source: AudioSourceRef::new(storage),
            load_policy: AudioLoadPolicy::StaticUncached,
            looping: false,
            volume: 0.0,
        };
        if let Err(error) = audio.submit_commands([command]) {
            return DecodeSmoke::Skipped(format!("audio worker unavailable: {error}"));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            for event in audio.drain_events() {
                match event {
                    AudioEvent::PlaybackStopped { .. } => return DecodeSmoke::Decoded,
                    AudioEvent::Status(status) => {
                        // A missing output device is the only skip here: the
                        // Opus-disabled marker also appears in the fallback part
                        // of every failed static load, so treating it as a skip
                        // would hide a real decode failure (a missing `pcm`
                        // feature, for one).
                        if status.message.contains("audio backend is unavailable") {
                            return DecodeSmoke::Skipped("no audio output device".to_string());
                        }
                        panic!("`{storage}` did not decode: {}", status.message);
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "no audio event for `{storage}` within 5s"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// In-memory storage exposing one named stream, like a loose game file or
    /// an XP3 member seen through the engine's storage port.
    struct MemoryStorage {
        storage: String,
        bytes: Vec<u8>,
    }

    impl StoragePort for MemoryStorage {
        fn open(&self, path: &str) -> io::Result<Box<dyn ResourceStream>> {
            if !path.eq_ignore_ascii_case(&self.storage) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no storage named {path}"),
                ));
            }
            Ok(Box::new(io::Cursor::new(self.bytes.clone())))
        }

        fn exists(&self, path: &str) -> bool {
            path.eq_ignore_ascii_case(&self.storage)
        }
    }
}
