# AGENTS.md

## Project Philosophy

`krkr-ruri` is a high-performance, GPU-first, pure Rust runtime. It targets full cross-platform reach without C++ glue code.

Do not use Flutter, ANGLE, Cocos2d-x, native UI toolkits, or any general UI framework. The engine is the UI: it renders its own launcher, settings, runtime surfaces, and overlays, similar in spirit to RetroArch.

All pixel composition, transitions, effects, image presentation, and text rendering should be pushed onto the GPU. The CPU should focus on script execution, resource orchestration, and logic scheduling.

Build through small, runnable vertical slices, but keep every slice aligned with the GPU-first architecture.

## Architecture

- `krkr-core`: pure Rust state, resource traits, input events, view models, draw lists. No platform UI or GPU APIs.
- `krkr-render`: `wgpu` rendering, surface/device/pipeline management, GPU resources.
- `krkr-platform`: filesystem and minimal platform bridges. Keep native UI out of core and avoid platform UI components for engine surfaces.
- `krkr-audio`: audio backend shell and future playback.
- `krkr-tjs`: pure Rust TJS2 language runtime crate. Keep it platform/GPU independent and focused on values, parsing, bytecode/VM, host bindings, and optional JIT support.
- `krkr-kag`: KAG3 `.ks` scenario-layer scaffold and tests. KAG3 itself is mostly TJS2 code; this crate exists to parse/step `.ks` fixtures and integrate with `krkr-tjs`, not to reimplement the full TJS-written KAG3 framework independently.
- `apps/desktop`: `winit` app lifecycle, input mapping, state transitions.

## Working Rules

- Follow existing crate boundaries before adding new abstractions.
- Keep changes scoped; avoid unrelated refactors.
- Prefer simple data structures and explicit state machines.
- Add tests for core behavior, resource handling, and parsing logic as those areas grow.
- Do not add text/image/audio/TJS/XP3 claims unless the feature is actually wired and verified.
- Keep TJS2 semantics in `krkr-tjs`; keep `.ks` parsing and KAG scenario tests in `krkr-kag`. Use ignored conformance fixtures for KAG3 behavior that still depends on a real TJS2 runtime.

## Verification

Run before handing off non-trivial code changes:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Manual desktop check when UI/rendering behavior changes:

```sh
cargo run -p krkr-desktop
```
