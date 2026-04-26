# krkr-kag

`krkr-kag` is the pure Rust KAG scenario layer scaffold. It has no platform,
native UI, or GPU dependencies.

The first enabled tests parse and step through the real `first.ks` fixture from
`krkrz/kag3`. The crate intentionally implements only a tiny vertical slice:

- UTF-8 and Shift_JIS scenario decoding.
- Labels, character lines, command tags, inline tags, and text events.
- A minimal runner that turns `wait`, `cm`, and text into runtime actions.
- Parser edge tests for comments, quoted params, labels, characters, inline
  tags, malformed tags, unknown tags, and decoder failures.
- Ignored boot conformance for future KAG3 startup/TJS integration.
- Ignored runtime conformance fixtures for macro expansion, conditionals,
  `iscript`, jump/call/return, storage loading, and message flow.

## Run

```sh
cargo test -p krkr-kag
cargo test --workspace
```

Ignored boot conformance tests are expected to fail until KAG3 boot
orchestration and TJS script execution are implemented:

```sh
cargo test -p krkr-kag -- --ignored
```
