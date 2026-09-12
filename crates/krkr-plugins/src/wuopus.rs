//! `wuopus.dll` — Opus (`*.opus`) codec registration.
//!
//! The reference plugin (krkrz `src/plugins/win32/opus/`, the "kropus"
//! plugin) is a TVP Sound System media module, not a TJS surface: `V2Link`
//! reads `-opus_gain` and `-opus_pcm_format` from the command line, and the
//! module answers `GetSupportExts` with `.opus` / "Opus Stream Format" so
//! every `WaveSoundBuffer` open of an `.opus` storage gets a decoder. The
//! decoder itself hard-codes 48 kHz and takes the channel count from the
//! OpusHead.
//!
//! This module is that registration: the name, the extension and the two
//! options. Opus decoding lives in `krkr-audio` behind its optional `opus`
//! feature (Symphonia's Ogg demuxer probes the stream, audiopus decodes the
//! packets into f32 frames); `krkr-desktop` and `krkr-debug` enable that
//! feature, and a build without it reports that Opus decoding is disabled.
//!
//! Option semantics follow `OpusMainUnit.cpp:493`: `-opus_gain` is read
//! through a TJS2 real conversion for its log line only — the reference never
//! applies it, its source notes `op_set_gain_offset` "should" be called — and
//! `-opus_pcm_format` recognises only the exact string `f32` (the default is
//! i16). The pipeline here always decodes to f32, so both values are parsed
//! and reported rather than applied.
//!
//! The option helpers below mirror [`crate::wuvorbis`]'s: the two reference
//! DLLs each carry their own copy of the same command-line code, and keeping
//! every plugin module self-contained is what lets one be implemented by
//! editing a single file.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Opus (*.opus) codec registration (TSS media module, no TJS surface) and the -opus_gain / -opus_pcm_format options",
    notes: "`.opus` playback is real whenever krkr-audio is built with its `opus` feature (krkr-desktop and krkr-debug enable it; else the loader reports that Opus decoding is disabled). Registration and both options are real too — `-opus_gain` stays log-only here exactly as the reference leaves it — but a requested pcm format is mapped, not applied: the native pipeline always decodes to f32.",
    install: |engine| engine.register_plugin(WuOpusPlugin),
};

/// Canonical DLL name: what `KrkrPlugin::name` reports and what a game's
/// `Plugins.link("wuopus.dll")` resolves through [`crate::catalog`].
pub(crate) const NAME: &str = "wuopus.dll";

pub struct WuOpusPlugin;

impl KrkrPlugin for WuOpusPlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // The reference reads its options in `V2Link`, before the engine hands
        // the module any storage. The process command line is the same source
        // `KrkrHost` itself parses for `System.getArgument("-opus_gain")`.
        let options = OpusOptions::from_arguments(std::env::args().skip(1));
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
    extension: ".opus",
    media_type: "Opus Stream Format",
    loader: "krkr-audio (symphonia ogg probe + audiopus)",
    loader_feature: Some("opus"),
}];

/// `-opus_pcm_format`: the reference extracts i16 samples unless the option
/// value is exactly `f32` (`OpusMainUnit.cpp:515`).
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

/// The option set `V2Link` ends up with, with the reference's defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OpusOptions {
    /// `-opus_gain` in dB; 0 when the option is absent.
    pub(crate) gain_db: f64,
    pub(crate) pcm_format: PcmFormat,
}

impl Default for OpusOptions {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            pcm_format: PcmFormat::I16,
        }
    }
}

impl OpusOptions {
    /// Reads the two options out of a command line, the way `V2Link` does: the
    /// first `-name`/`-name=value` token wins (`TVPGetCommandLine` walks
    /// `TVPProgramArguments` in order), a bare flag reads as `"yes"`, and an
    /// unknown value leaves the option at its default.
    pub(crate) fn from_arguments(arguments: impl IntoIterator<Item = String>) -> Self {
        let arguments: Vec<String> = arguments.into_iter().collect();
        let mut options = Self::default();
        if let Some(value) = first_option_value(&arguments, "-opus_gain") {
            options.gain_db = tjs_real(&value);
        }
        if let Some(value) = first_option_value(&arguments, "-opus_pcm_format")
            && value == "f32"
        {
            options.pcm_format = PcmFormat::F32;
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
                "wuopus: Opus (*.opus) decoder registered (TSS media module, no TJS surface): \
                 \"{}\" ({}) -> {}{}.",
                support.extension, support.media_type, support.loader, feature,
            ),
            format!(
                "wuopus: options -opus_gain={}dB -opus_pcm_format={} (gain is log-only, as in \
                 the reference; the native pipeline is f32-only).",
                self.gain_db,
                self.pcm_format.as_str(),
            ),
        ]
    }
}

/// The value of a `-name` / `-name=value` token, `"yes"` for the bare form —
/// `TVPGetCommandLine`'s contract (`SysInitImpl.cpp:1626`). A token that only
/// starts with the name (`-opus_gainx=1`) is not the option.
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
/// which is what a bare `-opus_gain` ("yes") becomes — the reference logs
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

    /// A real Ogg Opus stream: a 440 Hz mono sine, 48 kHz, 60 ms, 6 kbit/s
    /// VOIP mode, written by libopus —
    /// `ffmpeg -f lavfi -i "sine=frequency=440:duration=0.06" -ar 48000 -ac 1
    /// -c:a libopus -b:a 6k -application voip smoke.opus`.
    const OPUS_FIXTURE_HEX: &str = "\
        4f67675300020000000000000000c2e8c30a00000000a0341bc801134f707573486561640101380180bb00000000004f676753000000000000000000\
        00c2e8c30a010000000a835e0f013e4f707573546167730d0000004c61766636322e31322e313031010000001d000000656e636f6465723d4c617663\
        36322e32382e313031206c69626f7075734f6767530004780c000000000000c2e8c30a020000001d728c210411180f0f08836d82d01cfdedc4ece7f3\
        8fa492479808a71a5d853c55c01ed92e6ae8a99293a15b04164a40163008a1218f0c89dbd4ed409b46d5142208066d35300aec7d40b3729ecaefe0";

    #[test]
    fn defaults_match_the_reference_option_descriptor() {
        let options = OpusOptions::from_arguments(Vec::<String>::new());
        assert_eq!(options.gain_db, 0.0);
        assert_eq!(options.pcm_format, PcmFormat::I16);
    }

    #[test]
    fn gain_is_read_through_a_tjs_real_conversion() {
        let gain = |arguments: &[&str]| {
            OpusOptions::from_arguments(arguments.iter().map(|argument| argument.to_string()))
                .gain_db
        };
        assert_eq!(gain(&["-opus_gain=-10"]), -10.0);
        assert_eq!(gain(&["-opus_gain=-0.5"]), -0.5);
        assert_eq!(gain(&["-opus_gain=+3"]), 3.0);
        // The bare flag is answered as "yes", and TJS2 reads "yes" as 0.
        assert_eq!(gain(&["-opus_gain"]), 0.0);
        assert_eq!(gain(&["-opus_gain=loud"]), 0.0);
        // TJS2 keeps the numeric prefix and ignores what follows it.
        assert_eq!(gain(&["-opus_gain=-10dB"]), -10.0);
        assert_eq!(gain(&["-opus_gain=1e1"]), 10.0);
        // A token that merely starts with the option name is a different one.
        assert_eq!(gain(&["-opus_gainx=-10"]), 0.0);
        assert_eq!(gain(&["-opus_pcm_format=f32"]), 0.0);
    }

    #[test]
    fn pcm_format_only_recognises_the_exact_value_f32() {
        let format = |arguments: &[&str]| {
            OpusOptions::from_arguments(arguments.iter().map(|argument| argument.to_string()))
                .pcm_format
        };
        assert_eq!(format(&["-opus_pcm_format=f32"]), PcmFormat::F32);
        assert_eq!(format(&["-opus_pcm_format=i16"]), PcmFormat::I16);
        assert_eq!(format(&["-opus_pcm_format=F32"]), PcmFormat::I16);
        assert_eq!(format(&["-opus_pcm_format=float"]), PcmFormat::I16);
        assert_eq!(format(&["-opus_pcm_format"]), PcmFormat::I16);
    }

    #[test]
    fn registers_under_the_catalog_name_and_reports_its_options() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(WuOpusPlugin).expect("register");

        assert_eq!(catalog::canonical_name(NAME), Some(NAME));
        assert!(
            engine.host().linked_plugins().any(|linked| linked == NAME),
            "the plugin does not register the name the catalog resolves",
        );
        let support = MEDIA_SUPPORT[0];
        let logs = engine.host().logs();
        assert!(
            logs.iter().any(|line| line.contains("Opus (*.opus)")
                && line.contains(support.extension)
                && line.contains(support.loader)),
            "no registration line with the media mapping: {logs:?}",
        );
        assert!(
            logs.iter()
                .any(|line| line.contains("-opus_pcm_format=i16")),
            "no option line: {logs:?}",
        );
    }

    /// The extension the module answers for is the one the native loader
    /// decodes: the smoke plays a real Ogg Opus stream under that extension,
    /// through the production `AudioSystem`. A build without krkr-audio's
    /// `opus` feature skips instead of failing.
    #[test]
    fn the_reference_extension_is_the_one_this_engine_decodes() {
        let [support] = MEDIA_SUPPORT else {
            panic!("wuopus registers exactly one extension");
        };
        assert_eq!(support.extension, ".opus");
        assert_eq!(support.media_type, "Opus Stream Format");
        assert_eq!(support.loader_feature, Some("opus"));
        let storage = format!("wuopus-smoke{}", support.extension);
        match decode_smoke(&storage, fixture_bytes(OPUS_FIXTURE_HEX)) {
            DecodeSmoke::Decoded => {}
            DecodeSmoke::Skipped(reason) => {
                eprintln!("skipping the Opus decode smoke: {reason}");
            }
        }
    }

    /// M23: `krkr-debug` did not enable `krkr-audio/opus`, so the harness could
    /// not decode PARQUET's `.opus` BGM while `krkr-desktop` could. The smoke
    /// above covers the codec when the feature is on; this pins the feature at
    /// the two shells that must enable it.
    #[test]
    fn both_native_shells_enable_the_opus_feature() {
        for manifest in ["apps/debugger/Cargo.toml", "apps/desktop/Cargo.toml"] {
            let path = format!("{}/../../{manifest}", env!("CARGO_MANIFEST_DIR"));
            let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!("{path} is not readable: {error}");
            });
            let line = text
                .lines()
                .find(|line| line.trim_start().starts_with("krkr-audio"))
                .unwrap_or_else(|| panic!("{path} has no krkr-audio dependency"));
            let features = line
                .split_once("features = [")
                .and_then(|(_, rest)| rest.split_once(']'))
                .map(|(list, _)| list)
                .unwrap_or_else(|| panic!("{path} enables no krkr-audio features: {line}"));
            assert!(
                features
                    .split(',')
                    .any(|feature| feature.trim().trim_matches('"') == "opus"),
                "{path} does not enable the opus feature: {line}",
            );
        }
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
    /// loader, the one that falls back to the Opus decoder and says so when the
    /// codec was compiled out. Environments without an audio device and builds
    /// without the codec feature skip instead of failing.
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
                        for (marker, reason) in [
                            ("audio backend is unavailable", "no audio output device"),
                            (
                                "Opus decoding is disabled",
                                "the opus feature is compiled out",
                            ),
                        ] {
                            if status.message.contains(marker) {
                                return DecodeSmoke::Skipped(reason.to_string());
                            }
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
