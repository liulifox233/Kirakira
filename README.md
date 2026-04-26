# krkr-ruri

`krkr-ruri` is an early Rust workspace for a Kirikiri/KAG-style runtime. The current build is a macOS-first desktop skeleton: it opens a `winit` window, initializes `wgpu` on Metal, and renders a GPU-drawn launcher/settings/runtime shell.

## Run

```sh
cargo run -p krkr-desktop
```

The launcher is fully self-drawn. Use **Open Project** to select a directory and enter the empty runtime shell. Press `Esc` in the runtime shell to return to the launcher. The **Start** region enters the runtime shell only after a project directory has been selected.

## Runtime Layers

TJS2 is the scripting language/runtime. KAG3 is a visual novel framework mostly written in TJS2, and `.ks` files are KAG3 scenario scripts consumed by that framework.

`krkr-tjs` is the future high-performance TJS2 runtime, including interpreter/bytecode work and optional JIT support. `krkr-kag` is a separate `.ks` scenario-layer scaffold used for parsing tests, real KAG3 fixtures, and future integration with `krkr-tjs`; it must not become a replacement implementation of the whole TJS-written KAG3 system.

## Current Capabilities

- Rust workspace with `krkr-core`, `krkr-render`, `krkr-audio`, `krkr-platform`, `krkr-xp3`, `krkr-tjs`, `krkr-kag`, and `krkr-desktop`.
- `winit` application lifecycle with resize, scale factor, mouse, keyboard, redraw, and close handling.
- `wgpu` renderer with rectangle rendering, surface reconfiguration, scissor clipping support, viewport reporting, and a reserved texture pipeline for the next image-rendering stage.
- Core UI state for launcher/settings interactions and an empty runtime shell draw list.
- Filesystem resource provider abstraction and native folder picker bridge.
- XP3 archive parser, file-backed XP3 resource provider, and raw/zlib entry streaming.
- Basic status/error reporting through stderr logs, window titles, and native error dialogs for startup/selection failures.
- `krkr-tjs` crate scaffold with value, object, bytecode VM, call frame, exception, and benchmark tests for future TJS2 runtime work.
- `krkr-kag` crate scaffold with UTF-8/Shift_JIS scenario decoding, KAG tag/text parsing, and a minimal runner tested against real `krkrz/kag3` fixtures.

## Non-Goals For This Stage

- No TJS interpreter yet; `krkr-tjs` currently provides only minimal APIs and ignored conformance fixtures.
- No full KAG3 interpreter yet; `krkr-kag` currently supports only a small parser/runner slice.
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

TJS runtime scaffold:

```sh
cargo test -p krkr-tjs
cargo check -p krkr-tjs --features jit --all-targets
cargo bench -p krkr-tjs
```

KAG3 scenario scaffold:

```sh
cargo test -p krkr-kag
```
