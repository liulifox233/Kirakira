# vendored eluna

This directory is a vendored copy of the local eluna fork, consumed by the
`krkr-emote` adaptation crate (`crates/krkr-emote`) as a path dependency.

## Provenance

| Field | Value |
| ----- | ----- |
| Upstream URL | `https://github.com/xmoezzz/eluna` (`git@github.com:xmoezzz/eluna.git`) |
| Local fork | `/Users/ruri/repo/eluna` (single-commit fork of upstream) |
| Pinned commit | `d172fefd29b10e99844028d2e0e499f1c55b166a` ("initial release", 2026-05-14) |
| Vendored on | 2026-09-12 |
| Vendored by | Kirakira mission M50 (`crates/krkr-emote`) |

The vendored tree is byte-identical to the pinned fork for every file that was
copied; `diff -r --exclude=eluna_player /Users/ruri/repo/eluna vendor/eluna`
reports only this file, the added licence texts and the pruned directory.

## What is vendored

- `Cargo.toml`, `Cargo.lock`, `README.md`, `.gitignore` — upstream workspace root.
- `crates/eluna` — the library crate (PSB parser, Emote schema/runtime, vertex
  and shader helpers). This is the crate `krkr-emote` depends on.
- `crates/psb_extract` — the standalone PSB unpacker/schema dumper. Vendored so
  the upstream workspace still builds in place; nothing in Kirakira depends on
  it, but it is the tool used to produce the evidence behind the
  `krkr-emote` normalisation.

## What is pruned

- `crates/eluna_player/**` — upstream's winit + wgpu + egui preview player.
  It is not needed by the library and pulls a 7.5 MB `default.ttf` plus the
  whole GUI stack that would otherwise appear in our tree and lockfile. Pruned
  in full; the upstream workspace `members = ["./crates/*"]` glob simply no
  longer matches it. Re-adding it means copying the directory back from the
  fork and adding nothing else.

## Licence

Upstream ships **no licence files** — only the SPDX `license` field in each
crate manifest. To keep the vendored tree distributable we added the licence
texts that those manifests declare:

| Path | Licence | Applies to |
| ---- | ------- | ---------- |
| `crates/eluna/LICENSE-MPL-2.0` | MPL-2.0 (full text) | `crates/eluna` (`license = "MPL-2.0"`) |
| `crates/psb_extract/LICENSE-MIT` | MIT | `crates/psb_extract` (`license = "MIT OR Apache-2.0"`) |
| `crates/psb_extract/LICENSE-APACHE-2.0` | Apache-2.0 | `crates/psb_extract` (the `OR` alternative) |

The crate manifests keep their original SPDX declarations untouched.

Obligations for Kirakira (AGPL-3.0-or-later):

- eluna files stay under MPL-2.0; do not relicense them, keep their notices.
- Our modifications to MPL-covered files, if any, are published with the
  repository (which is public) — MPL-2.0 is file-level copyleft and is
  compatible with AGPL through its §3.3 secondary-licence clause.
- Files added by Kirakira in this directory (`UPSTREAM.md`, the licence texts)
  are metadata; everything we write ourselves (`crates/krkr-emote`) is
  AGPL-3.0-or-later like the rest of the repository.

## Local patches

None. The pinned commit is vendored verbatim; the difference in behaviour that
PARQUET's `.mtn` models need (source-table shape, `src/<source>/<icon>` layer
content) is implemented entirely in `crates/krkr-emote` by rewriting the parsed
`eluna::PsbFile` tree before handing it to eluna's schema/scene builder. See
`crates/krkr-emote/src/normalize.rs`.

Any future patch to a vendored file must be listed here with file, reason and
a diff summary, and should be upstreamed to
`git@github.com:xmoezzz/eluna.git` when possible.

## Updating the vendored copy

1. `git -C /Users/ruri/repo/eluna pull` (or check out the new pin).
2. Re-copy the files listed under "What is vendored" with the same prune.
3. Update the pin and date in this file.
4. Re-run `cargo test -p krkr-emote`; the PARQUET tests exercise the parser
   against real PSB v3/v4 game data.
