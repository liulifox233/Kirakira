# krkr-ruri

`krkr-ruri` is an early Rust workspace for a Kirikiri/KAG-style runtime. The current build is a macOS-first desktop skeleton: it opens a `winit` window, initializes `wgpu` on Metal, renders a GPU-drawn launcher/settings/runtime shell, and boots a pure Rust KRKR engine layer that can execute `startup.tjs`.

## Run

```sh
cargo run -p krkr-desktop
```

The launcher is fully self-drawn. Use **Open Project** to select a directory and execute its `startup.tjs`. Press `Esc` in the runtime shell to return to the launcher. The **Start** region launches only after a project directory has been selected.

## Current Capabilities

- Rust workspace with `krkr-tjs2`, `krkr-engine`, `krkr-core`, `krkr-render`, `krkr-xp3`, `krkr-audio`, `krkr-platform`, and `krkr-desktop`.
- TJS2 frontend/compiler/VM with core runtime object helpers.
- KRKR engine layer that owns the TJS runtime, registers TVP globals/builtins, reads project storage, opens XP3 archives, and supports pure Rust plugin registration.
- `winit` application lifecycle with resize, scale factor, mouse, keyboard, redraw, and close handling.
- `wgpu` renderer with rectangle rendering, surface reconfiguration, scissor clipping support, viewport reporting, and a reserved texture pipeline for the next image-rendering stage.
- Core UI state for launcher/settings interactions and an empty runtime shell draw list.
- Filesystem resource provider abstraction and native folder picker bridge.
- XP3 archive parser, file-backed XP3 resource provider, and raw/zlib entry streaming.
- Basic status/error reporting through stderr logs, window titles, and native error dialogs for startup/selection failures.

## Non-Goals For This Stage

- No KAG parser.
- No image decoding or sprite rendering.
- No text renderer.
- No audio playback.
- No C++ plugin ABI compatibility yet.
- No platform-native UI beyond file/error dialogs.

## Verification

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
