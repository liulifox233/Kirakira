# vendored eluna

This directory is a vendored copy of the local eluna fork, consumed by the
`krkr-emote` adaptation crate (`crates/krkr-emote`) as a path dependency. It is
the fork's own workspace tree, pruned, kept in-repo as a working tree (not a
submodule and not `git`-excluded), and it builds and tests on its own from this
directory.

## Provenance

| Field | Value |
| ----- | ----- |
| Upstream URL | `https://github.com/xmoezzz/eluna` (`git@github.com:xmoezzz/eluna.git`) |
| Local fork | `/home/ruri/repo/eluna` (single squashed commit, no per-patch history) |
| Pinned commit | `12e4d2fa03b64714a83a0363eaadf26a125d9fe6` ("update readme", 2026-08-10) |
| Vendored on | 2026-09-13 |
| Vendored by | Kirakira mission M127 (re-vendored from M50's `d172fef` pin) |

The fork head renames the library package from `eluna` to `eluna_rs` (its lib
target is still `eluna`), so Kirakira's workspace manifest declares the
dependency as `eluna = { package = "eluna_rs", path = "vendor/eluna/crates/eluna" }`.

### Fidelity

The previous UPSTREAM.md claimed the tree was byte-identical to the pin. That
claim is **retired**: the vendored tree is byte-identical to the fork head for
every file that was copied, with exactly five kinds of deliberate deviation,
all listed in the patch ledger below:

1. the pruned directories (`crates/eluna_player`, `images/`),
2. the licence texts upstream does not ship,
3. the regenerated `Cargo.lock` (the fork's lock names the pruned player and
   its ~370-package GUI tree),
4. two stale unit tests fixed in place (the fork head's own suite is red
   without them), and
5. the frame-sampler semantics `motionplayer_nod3d.dll` authors and the fork
   does not implement: `content.mask` key gating, the `{c,x,y}` cubic-Bezier
   easing curves and the mesh `cc` curve (M137; functional, `emote.rs`).

`diff -r` against `/home/ruri/repo/eluna` reports only those, plus this file.

## What is vendored

- `Cargo.toml`, `Cargo.lock`, `README.md`, `.gitignore` — workspace root.
- `crates/eluna` — the library crate (`eluna_rs`, lib name `eluna`): PSB parser
  with MDF/LZ4/key handling, Emote schema/runtime extraction, the static scene
  builder, the recovered `StepFrame` pipeline (particles, stencil ancestry,
  camera/stereovision, timelines, wind/physics) and the `sdk` player facade.
  This is the crate `krkr-emote` depends on.
- `crates/psb_extract` — the standalone PSB unpacker/schema dumper (`--input`,
  `--bruteforce-key`). Vendored so the upstream workspace still builds in
  place; nothing in Kirakira depends on it, but it is the tool used to produce
  the evidence behind the `krkr-emote` normalisation.

## What is pruned

- `crates/eluna_player/**` — upstream's winit + wgpu + egui preview player
  (7.5 MB `default.ttf`, the whole GUI stack, ~370 lockfile packages). Not
  needed by the library. The workspace `members = ["./crates/*"]` glob simply
  no longer matches it; re-adding it means copying the directory back and
  regenerating the lock.
- `images/**` — the README screenshot referenced by `README.md`. Metadata only.

Pruning the player is what makes the vendored lock differ from the fork's
(395 packages → 25). The vendored lock is generated for the pruned workspace,
so `cargo build`/`cargo test` inside this directory do not rewrite it.

## Building and testing

```bash
# standalone, from this directory; keep the target dir outside the tree
CARGO_TARGET_DIR=<repo>/target/eluna-vendor cargo test
```

At the fork head this reports **95 passed; 0 failed** (93/95 before the two
test fixes in the patch ledger). For comparison, the previous pin's own tests
did not compile at all (a stale unit test referenced a field that no longer
existed), so eluna's suite was not runnable there.

With M137's frame-sampler patch the vendored tree reports **99 passed;
0 failed** — the four added tests cover the `content.mask` gate and the
`{c,x,y}` Bezier evaluator (single segment, chained segments, piece list).

Kirakira's workspace excludes this directory (`exclude = ["vendor/eluna"]`);
`krkr-emote` consumes it as a path dependency and its own suite covers the
parser/schema paths Kirakira uses.

## Patch ledger

Every file in this tree that differs from the fork head, with the reason, the
evidence, and whether the fork still needs the same change.

| File | Patch | Reason / evidence | Upstream status |
| ---- | ----- | ----------------- | --------------- |
| `crates/eluna/LICENSE-MPL-2.0` | Added licence text | Upstream ships no licence files, only the SPDX `license = "MPL-2.0"` field. Distribution needs the text. | TO MIRROR upstream |
| `crates/psb_extract/LICENSE-MIT`, `LICENSE-APACHE-2.0` | Added licence texts | Same, for `license = "MIT OR Apache-2.0"`. | TO MIRROR upstream |
| `Cargo.lock` | Regenerated for the pruned workspace | The fork lock names `eluna_player` and its ~370 GUI packages; `cargo` rewrites the lock on first build here. Regenerating once keeps the vendored tree clean. | Vendoring artefact (not for upstream) |
| `crates/eluna/src/vertex.rs` (`builds_single_cell_strip`) | u/v compared with the arithmetic's tolerance instead of exact f32 literals | The test asserts `v == 0.1` while the builder computes `(tex_y + y * v_step) * (1.0 / texture_height)`: `10.0 * (1.0 / 100.0)` is `0.099999994` in f32. Same test and same builder as the previous pin; the pin simply never compiled. | TO MIRROR upstream |
| `crates/eluna/src/runtime.rs` (`timeline_hold_markers_do_not_pollute_authored_ranges`) | `default_value` expectation `3.0` → `0.0` | The implementation deliberately starts a timeline-only variable at the scalar zero default (`merge_timeline_variable_info`, comment citing `sub_1026FA30`, `timeline_default = 0.0`), and the sibling test `timeline_only_variable_does_not_take_first_key_as_initial_value` already pins that. The stale expectation was the only failure; the hold marker still stays out of the authored 3.0..5.0 range. | TO MIRROR upstream |
| `crates/eluna/src/emote.rs` (frame sampler) | PARQUET's reference frame semantics: `content.mask` gates every key read (`FUN_1001d000`); the authored `{c,x,y}` easing curve is evaluated by cubic-Bezier parameter inversion (`FUN_100087d0`/`FUN_10008220`, chained `3N+1` segments) for `ccc/acc/zcc/scc/occ`; the mesh interpolates with the frame's mesh `cc` curve (`FUN_100098f0`). Mask-less content keeps the old permissive reads. | `motionplayer_nod3d.dll` (full Ghidra export `/tmp/ghidra-full/motionplayer_nod3d/decompiled`, M134 notes) reads frame keys only under the bitfield and evaluates `x`/`y` Bezier arrays; PARQUET's 23 `.mtn` carry 508 single-segment and 8 chained `{c,x,y}` curves whose `p` (second-derivative) array the fork's spline path needs and the assets never write. M137. | TO MIRROR upstream (functional) |

"TO MIRROR" rows are changes the fork head should receive; they were made in
this vendored copy only because the fork working tree is outside the mission's
write scope. The first two rows are test-side only; the `emote.rs` row changes
the library's behaviour for the reference flavor the fork's spline form did
not cover, and is covered by new unit tests in that file plus the
`krkr-emote` asset tests.

## Adapter passes at this pin (M127)

The `krkr-emote` normalisation (`crates/krkr-emote/src/normalize.rs`) exists to
bridge PARQUET's `.mtn` flavor to eluna's. Re-verified against the fork head:

| Adapter pass | Verdict | Evidence at the fork head |
| ------------ | ------- | ------------------------- |
| `content.opa` rescale (0..255 → eluna's old 0..10 scale) | **DELETED** | eluna now reads the byte natively: `emote.rs:2383` (`opa` default 255), `:2629-2631` (`opa_raw / 255.0`), `:2999` (sprite opacity), `:4061` (interpolated state rounded like the native DLL). Keeping the rescale double-scaled: sd101's `opa: 192` came out as 8/255 = 0.0314. |
| `parameterize: null` stripped | **KEPT** | Still required: `layer_parameter_eval` (`emote.rs:4468-4474`) enters the parameterised branch for a present-but-null field and `resolve_parameterize` (`:4549-4557`) resolves `Null` to no parameter, freezing the layer at local time 0; the un-parameterised path is the absent-field branch at `:4539-4546`. Test: `tests/synthetic.rs::parameterize_null_freezes_without_the_strip` (raw scene 1.0 vs adapted 128/255) and the game's own fade (sd101 `ef_moya/bgef1`). |
| Per-icon `pixel` → synthetic `source["<source>/<icon>"]` + `src/` rewrite | **KEPT** | Still required: `collect_textures` skips a source without a `texture` sub-object (`emote.rs:1585-1587`) and only reads `texture.pixel`/`data`/`resource` (`:1588-1594`); no code resolves `src/<source>/<icon>`. Test: `tests/synthetic.rs::parquet_flavor_icons_need_the_synthesized_sources`. |

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

## Capabilities the fork head adds (hand-off for the plugin wiring)

The `krkr-plugins` motionplayer surface still registers physics, timelines,
mesh deformation, particles, separate-layer mode and `.psb` model playback as
warn-once stubs. The fork head now carries recovered implementations behind
these entry points (paths under `crates/eluna/src`):

- **Particles** — `ParticleStaticConfig` (`emote.rs:563`), the per-scene
  persistent `ParticleEmitterRuntime` (`emote.rs:593`, held by
  `EmoteStaticScene.particle_emitters` at `emote.rs:240`), spawn/instance/random
  helpers (`emote.rs:5990-6372`), parity claims at `sdk.rs:250-251`.
- **Stencil / alpha-mask** — `EmoteStaticScene.composite_mask_owners`
  (`emote.rs:232`) and `composite_mask_sources_by_key` (`emote.rs:237`), the
  per-frame `stencil_type`/`stencil_phase`/`stencil_composite_item`/
  `stencil_wipe_*` metadata (`emote.rs:399-406`), the
  `EmoteDrawPass::{MaskGeneration, StencilCompositeMask, Filtered}` passes
  (`emote.rs:428-433`), `EmoteMaskMode` (`api.rs:27`) and
  `EmoteDeviceRenderOptions` (`api.rs:38`).
- **Camera / stereovision** — `EmoteCameraRuntimeState` (`emote.rs:506`) with
  per-scope state on the scene (`camera_runtimes`, `emote.rs:254`),
  `EmoteStereovisionControl`/`EmoteStereovisionProfile` (`emote.rs:35-48`),
  `EmoteStereovisionScreen` (`runtime.rs:416`), driven through
  `EmoteRuntime::camera_runtimes` (`sdk.rs:1057`) and the stereovision setters
  (`sdk.rs:983-1048`).
- **transformOrder** — `EmoteDrawFrameInfo.transform_order` (`emote.rs:390`),
  the native mask constants in `api::transform_order_mask` (`api.rs:55-70`),
  `EmoteRuntime::set_transform_order_mask` (`sdk.rs:860`) and
  `EmoteTransformMode` (`sdk.rs:32`).
- **Mesh deformation** — `EmoteMeshPatch` (`emote.rs:93-202`) with the native
  patch ops `sample`/`combined_with`/`interpolate`/`control_bounds`, the
  per-frame `mesh_transform`/`mesh_combine`/`mesh_sync_child_*` flags
  (`emote.rs:377-383`), the retained per-layer `mesh_chain`
  (`EmoteStepFrameLayerState`, `emote.rs:460`) and the meshCombinator split
  (`evaluate_mesh_combinator_split`, `emote.rs:4585`); `EmoteStepFrameMeshState`/
  `EmoteMeshChainNode` (`emote.rs:693`, `:702`) expose the recovered chain.
- **Feedback / previous framebuffer** — type-10 sprites carry
  `EmoteStaticSprite.feedback_history` (`emote.rs:333`) and are materialised by
  `build_feedback_history_sprite` (`emote.rs:3021`) from
  `EmoteFeedbackRuntimeState` (`emote.rs:518`); the decay math and sampling are
  in the parity report's confirmed list (`sdk.rs:253`).
- **Model pass (type 6)** — `EmoteModelRuntimeState` (`emote.rs:497`) carries
  the recovered local-time/direction state; loading and drawing
  `referenceModelFileList` resources stays a host 3-D-backend responsibility
  (`sdk.rs:264`).
- **Timeline lifecycle** — `EmoteTimeline`/`EmoteTimelineFrame`/
  `EmoteTimelineVariable` (`runtime.rs:56-74`), `collect_emote_timelines`
  (`runtime.rs:5371`), and on the runtime `play_timeline`/`fade_in_timeline`/
  `fade_out_timeline`/`set_timeline_blend_ratio`/`set_timeline_time`/
  `stop_timeline` (`sdk.rs:678-735`) plus the `is_timeline_playing`/
  `is_loop_timeline`/`timeline_blend_ratio` queries (`sdk.rs:747-759`) and
  `TimelinePlayMode` with PARALLEL/DIFFERENCE modes (`api.rs:79`).
- **Wind / physics** — `WindPulse`/`WindState` (`runtime.rs:190`, `:207`),
  `HairPhysicsState`/`BustPhysicsState` (`runtime.rs:260`, `:230`),
  `PhysicsControlDefinition`/`ClampControl`/`SelectorControl`/`LoopControl`/
  `MirrorControl`/`OpaqueControl`/`TransitionControl` (`runtime.rs:368-591`),
  `ElunaPlayer` (`runtime.rs:280`), and the runtime knobs
  `set_physics_enabled`/`reset_physics`/`set_outer_force`/`set_outer_rot`/
  `start_wind`/`stop_wind`/`set_hair_scale`/`set_parts_scale`/`set_bust_scale`
  (`sdk.rs:809-877`) with `EmoteGroundCorrectionHook` (`emote.rs:541`) wired
  through `set_ground_correction_hook` (`sdk.rs:438`).
- **Whole-player façade** — `EmoteRuntime` (`sdk.rs:278`) already implements
  `emote_show`/`emote_hide`/`emote_motion`/`emote_variable`/`emote_trans`
  (`sdk.rs:471-567`) with their option structs (`sdk.rs:49-128`), colour and
  grayscale filters (`sdk.rs:622-632`), smoothing/queuing/mesh-division
  (`sdk.rs:601-618`), an API-log recorder/replayer (`sdk.rs:1079-1115`) and the
  chara profile reader (`sdk.rs:1120-1144`). `emote_runtime_parity_report()`
  (`sdk.rs:238`) lists what upstream considers confirmed/partial/missing; its
  "missing" list still includes running these paths under `cargo test`, which
  this vendored tree now does (95/95) and `crates/krkr-emote`'s PARQUET suite
  exercises over the game's 23 `.mtn` files.
- **Metadata readers** — `collect_emote_runtime_pipeline` (`runtime.rs:3265`),
  `collect_emote_timelines` (`runtime.rs:5371`), `collect_emote_variables`
  (`runtime.rs:5484`) and `load_emote_static_scene` (`emote.rs:1546`) build the
  pipeline/timeline/variable tables straight from a `PsbFile`;
  `EmoteModelSchema::motion_infos`/`default_motion_name` (`emote.rs:858`, `:884`)
  list a model's motions. `crates/krkr-emote` re-exports the types its own
  signatures use (`EmoteModelSchema`, `EmoteStaticScene`, `EmoteStaticSprite`,
  `EmoteDrawFrameInfo`, `EmoteDrawPass`, `EmoteMeshPatch`, …) so the plugin side
  can reach them without naming the vendored path dependency.

## Updating the vendored copy

1. Update `/home/ruri/repo/eluna` to the new head and note the commit.
2. Re-copy the files listed under "What is vendored" with the same prune
   (`crates/eluna_player`, `images/`).
3. Regenerate `Cargo.lock` in the pruned tree
   (`CARGO_TARGET_DIR=<repo>/target/eluna-vendor cargo generate-lockfile`).
4. Re-check the patch ledger: drop rows upstream no longer needs, re-apply the
   rest (or take upstream's fix if it landed), and update "Upstream status".
5. Update the pin and date here, then re-run `cargo test` here (standalone) and
   `KRKR_EMOTE_PARQUET_DIR=<game> cargo test -p krkr-emote` in the workspace.
