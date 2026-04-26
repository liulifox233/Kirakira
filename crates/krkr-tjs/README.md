# krkr-tjs

`krkr-tjs` is the planned pure Rust TJS2 runtime crate for `krkr-ruri`.
It must stay platform-neutral and must not depend on GPU, native UI, or desktop
APIs. The crate currently contains a deliberately small public API plus a test
baseline for future runtime work.

## Current Scope

- Minimal `Value` model with numeric/string addition helpers.
- Minimal object property storage.
- Minimal bytecode VM smoke path for `Push`, `Add`, `Return`, and uncaught
  `Throw`.
- Minimal call frame stack for depth tracking and overflow reporting.
- Ignored TJS2 conformance fixtures for semantics that are not implemented yet.
- Real KAG3 `startup.tjs` and `system/Initialize.tjs` fixtures copied from
  `krkrz/kag3`, currently used to pin the unsupported boot boundary and future
  conformance target.
- Enabled boot-plan scanning tests for `Scripts.execStorage`, `KAGLoadScript`,
  and `kag.process` references in real KAG3 system scripts.
- Ignored host/JIT/performance conformance fixtures for `System`, `Storages`,
  `Scripts`, `Debug`, `Plugins`, interpreter/JIT parity, deopt, fallback, and
  hot-loop workloads.
- A stable `cargo bench` scaffold that can later move to a real benchmark
  harness.

## Run

```sh
cargo test -p krkr-tjs
cargo test --workspace
cargo check -p krkr-tjs --features jit --all-targets
cargo bench -p krkr-tjs
```

Ignored conformance fixtures can be inspected or run explicitly while
implementing a slice:

```sh
cargo test -p krkr-tjs -- --ignored
```

Those ignored tests are expected to fail until the related parser, evaluator,
object model, call frame, and exception semantics are implemented.
