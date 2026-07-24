# AGENTS.md

## Project Philosophy

`Kirakira` is a lightweight, high-performance, modern KRKR game emulator. It targets KRKR2/KRKRZ compatibility and full cross-platform reach across Linux, macOS, Windows, Android, iOS, and Web without C++ glue code.

## Architecture

- `krkr-tjs2`: TJS2 language frontend, compiler, bytecode VM, object model, and language builtins. Includes an interactive debugger core (`debug` module: source-line breakpoints, stepping, exception breaks, pause/inspect context) driven by a `DebugUi` implementation; MIR emits per-statement `SourceMark` instructions so bytecode carries statement-level source positions.
- `krkr-kag`: KAG parser and scenario control flow. It handles labels, tags, macros, conditionals, script blocks, jump/call/return, parser snapshots, and host callbacks.
- `krkr-engine`: KRKR/TVP engine runtime. It owns the TJS runtime and KAG parser, registers TVP globals/native objects, coordinates project storage, XP3 access, layers, transitions, timers/events, audio commands, and pure Rust plugin registration. Checks KAG line/label breakpoints before each scenario tag via the tjs2 debugger. Implements the native `VideoOverlay` object (`native/video.rs`): movies decode on a per-overlay thread through krkr-video (video frames and soundtrack chunks are pumped together with bounded-channel back-pressure), playback runs on the host clock, the soundtrack streams to krkr-audio as live PCM (`PlayPcmStream`), and the engine tick fires the TJS status events (`ready`/`play`/`pause`/`stop`/`unload`, `onPeriod`) KAG movie conductors wait on; visible overlays present as top-most texture quads in the frame output.
- `krkr-plugins`: pure Rust plugin registration.
- `krkr-core`: pure Rust shell/runtime state, input events, view models, draw lists, layer tree, message layer model, image upload metadata, and audio command model. No platform UI or GPU APIs.
- `krkr-render`: `wgpu` rendering, surface/device/pipeline management, rectangles, uploaded textures, text texture presentation, clipping, content sizing, and transitions.
- `krkr-font`: font discovery, metrics, glyph rasterization, and RGBA text image generation used before GPU texture upload.
- `krkr-platform`: filesystem and minimal platform bridges. Keep native UI out of core and avoid platform UI components for engine surfaces.
- `krkr-audio`: Kira/CPAL audio backend for static/streaming playback, buses, volume, fade, pause/resume, and stop commands. Also plays live PCM streams (`AudioCommand::PlayPcmStream`) fed by external decoders — movie soundtracks arrive this way; video containers never go through the audio file loaders.
- `krkr-xp3`: XP3 archive parsing and resource streaming.
- `krkr-video`: video decoding behind a `VideoDecoder` trait with per-platform pluggable backends (the same philosophy as krkr2/krkrz using DirectShow/Media Foundation system decoders, never a bundled decoder). Currently only the macOS backend exists (AVFoundation/VideoToolbox, feature `macos-avfoundation`); other platform backends are added per-OS later. Delivers RGBA frames with PTS to the engine's VideoOverlay, and decodes the soundtrack (`audio_spec`/`next_audio_chunk`) into interleaved f32 LPCM that the engine streams to krkr-audio.
- `apps/debugger`: `krkr-debug` headless CLI combining an LLDB-style interactive script debugger (stdin REPL over the tjs2 debugger core; TJS2 + KAG breakpoints, stepping, exception breaks, expression eval) with batch probing (screenshot/pixel/layer dumps, at-frame script injection, synthetic clicks, virtual clock/audio).
- `apps/disasm`: `krkr-disasm` standalone TJS2 bytecode disassembler. Dumps full structured disassembly (data pool, object headers, data slots, instructions) of official `TJS2100\0` bytecode from loose files, XP3 members, or game directories (engine archive priority), and of krkr's own compiler output from TJS source.
- `apps/desktop`: `winit` app lifecycle, input mapping, launcher/settings/runtime state transitions, renderer/audio integration, project selection, and fullscreen/window sizing.
