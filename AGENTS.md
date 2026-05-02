# AGENTS.md

## Project Philosophy

`Kirakira` is a lightweight, high-performance, modern KRKR game emulator. It targets KRKR2/KRKRZ compatibility and full cross-platform reach across Linux, macOS, Windows, Android, iOS, and Web without C++ glue code.

## Architecture

- `krkr-tjs2`: TJS2 language frontend, compiler, bytecode VM, object model, and language builtins.
- `krkr-kag`: KAG parser and scenario control flow. It handles labels, tags, macros, conditionals, script blocks, jump/call/return, parser snapshots, and host callbacks.
- `krkr-engine`: KRKR/TVP engine runtime. It owns the TJS runtime and KAG parser, registers TVP globals/native objects, coordinates project storage, XP3 access, layers, transitions, timers/events, audio commands, and pure Rust plugin registration.
- `krkr-core`: pure Rust shell/runtime state, input events, view models, draw lists, layer tree, message layer model, image upload metadata, and audio command model. No platform UI or GPU APIs.
- `krkr-render`: `wgpu` rendering, surface/device/pipeline management, rectangles, uploaded textures, text texture presentation, clipping, content sizing, and transitions.
- `krkr-font`: font discovery, metrics, glyph rasterization, and RGBA text image generation used before GPU texture upload.
- `krkr-platform`: filesystem and minimal platform bridges. Keep native UI out of core and avoid platform UI components for engine surfaces.
- `krkr-audio`: Kira/CPAL audio backend for static/streaming playback, buses, volume, fade, pause/resume, and stop commands.
- `krkr-xp3`: XP3 archive parsing and resource streaming.
- `apps/desktop`: `winit` app lifecycle, input mapping, launcher/settings/runtime state transitions, renderer/audio integration, project selection, and fullscreen/window sizing.
