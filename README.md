# krkr-ruri

`krkr-ruri` is an early Rust workspace for a Kirikiri/KAG-style runtime. The current build is a macOS-first desktop skeleton: it opens a `winit` window, initializes `wgpu` on Metal, and renders a GPU-drawn launcher/settings/runtime shell.

## Run

```sh
cargo run -p krkr-desktop
```

The launcher is fully self-drawn. Use **Open Project** to select a directory and enter the empty runtime shell. Press `Esc` in the runtime shell to return to the launcher. The **Start** region enters the runtime shell only after a project directory has been selected.

## Current Capabilities

- Rust workspace with `krkr-core`, `krkr-render`, `krkr-audio`, `krkr-platform`, and `krkr-desktop`.
- `winit` application lifecycle with resize, scale factor, mouse, keyboard, redraw, and close handling.
- `wgpu` renderer with rectangle rendering, surface reconfiguration, scissor clipping support, viewport reporting, and a reserved texture pipeline for the next image-rendering stage.
- Core UI state for launcher/settings interactions and an empty runtime shell draw list.
- Filesystem resource provider abstraction and native folder picker bridge.
- Basic status/error reporting through stderr logs, window titles, and native error dialogs for startup/selection failures.

## Non-Goals For This Stage

- No TJS interpreter.
- No KAG parser.
- No XP3 archive reader.
- No image decoding or sprite rendering.
- No text renderer.
- No audio playback.
- No platform-native UI beyond file/error dialogs.

## Verification

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
