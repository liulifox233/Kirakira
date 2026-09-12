# eluna

`eluna` is an open-source Emote/PSB reverse-engineering and reimplementation work made with Rust.

### Workspace members:

- `eluna`, the library crate.
- `eluna_player`, a winit + wgpu preview player.
- `psb_extract`, a PSB extractor and schema dumper.

### Current library scope:

- PSB parsing with MDF/LZ4 normalization and optional Emote key decryption. The canonical Emote key state is `0x075BCD15, 0x159A55E5, 0x1F123BB5, key, 0, 0`; only `key` varies for normal use. `psb_extract` also has a multi-threaded `--bruteforce-key` mode for this single DWORD.
- PSB resource extraction.
- D3D9-compatible Emote vertex layout and triangle-strip helpers.
- Emote schema/runtime extraction for `source`, `texture`, `icon`, `object`, `motion`, `layer`, `frameList`, `timelineControl`, controller metadata, and drawFrameInfo.


### Example:

```bash
## Obtain the PSB key by bruteforce
cargo run -p psb_extract -- --input model.psb --bruteforce-key

## Play the PSB
cargo run -p eluna_player -- --input model.psb --key 0x12345678 --motion main
```

If `--motion` is omitted, the first motion under the base object is used.
