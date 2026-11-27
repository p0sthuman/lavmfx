# Guide for AI coding agents

This file is for AI agents (Claude Code, Codex, Cursor, Copilot and others)
working on LAVMFX. Human contributors: [CONTRIBUTING.md](CONTRIBUTING.md) has
the same rules in more detail, and they apply to agents too.

LAVMFX is a live VJ and projection-mapping app written in Rust: a clip grid,
GPU effects, audio-reactive control, mapping, DMX, MIDI / OSC / REST control,
NDI, recording, and import of Resolume Arena `.avc` files. Licence:
GPL-3.0-or-later.

## Hard rules

1. **No third-party creative content or closed-source code.** Never add
   presets, shaders, fixture libraries, help texts, icons, screenshots or
   sample media from Resolume or any other product, and never add decompiled,
   disassembled or translated code from a closed-source app. Parameter names,
   ranges and file structures needed to read a format are fine. If you can't
   tell whether something is allowed, stop and ask the human.
2. **No personal or machine-specific data.** Never hard-code paths like
   `/home/<user>`, Wine user folders, drive letters, user names or e-mail
   addresses. Find such locations at run time (see `paths::import_prefs_dirs`
   in `app/crates/project/src/paths.rs`).
3. **No secrets.** Never commit `.env` files, tokens or keys.
4. **Licence-compatible dependencies only.** A new crate must be under a
   GPL-3.0-compatible licence (MIT, Apache-2.0, BSD, Zlib, MPL-2.0, ISC,
   GPL-2.0-or-later, GPL-3.0 and the like). Say why it's needed in the PR.
5. **Don't play sound on the user's speakers or open windows they didn't ask
   for** when testing. Tests must pass without audio or MIDI hardware.

## Build and test

Everything runs from `app/` (the Cargo workspace):

```sh
cd app
cargo build                                  # debug build
cargo run --release -- [file.lavmfx | file.avc | media files…]
cargo test --workspace
cargo clippy --workspace --all-targets       # must stay warning-free
cargo fmt --all                              # max_width = 120 (rustfmt.toml)
```

Needs Rust 1.95+, a C/C++ compiler, CMake, clang, and the FFmpeg 8 / 9,
PipeWire and ALSA development files (Linux). The release build takes several
minutes; prefer `cargo check` / `cargo test -p <crate>` while iterating.

Before you say a change is done: `cargo fmt --all`, clippy clean, and
`cargo test --workspace` passing. Report failures as they are, with the output.

## Layout

| Path | What's there |
|---|---|
| `app/crates/core` | Composition model, parameters, undo history, effect / source catalogues (`data/*.json`), parameter animation (`drive`), clock / transport, control addresses (OSC / REST), DMX and output models |
| `app/crates/render` | wgpu compositor, WGSL effects (`src/shaders/effects.wgsl`), slices and warping, ISF |
| `app/crates/media` | FFmpeg decoding, player threads, HAP, audio files, recording, NDI, PipeWire sharing, capture |
| `app/crates/audio` | Audio devices, FFT, the audio mixer and audio effects |
| `app/crates/io` | OSC, MIDI, Ableton Link, REST server, FFGL loading, Art-Net / sACN |
| `app/crates/project` | `.lavmfx` load / save, `.avc` import, media path resolution, fixture library |
| `app/lavmfx` | The app (egui): `src/app.rs` main window, `src/app/*` panels, `src/output.rs` output windows |
| `packaging/` | Portable build scripts (Linux native, Linux portable + AppImage in a podman container, Windows cross-build) |

## Code conventions

- **UI:** build panels from the shared widgets in
  `app/lavmfx/src/app/widgets.rs`. Colours come only from
  `app/lavmfx/src/app/theme.rs` (`theme::pal(ui)` / `theme::cur()`). No inline
  styling in panels.
- **Text never spills over other widgets:** long labels end in "…" and show
  the full text on hover. Paragraphs opt in to wrapping.
- **Every change to the composition is undoable** (goes through the history in
  `core`). REST / OSC / MIDI changes too.
- **Saved files stay readable:** new fields need `#[serde(default)]`; don't
  rename or remove serialized fields without a migration.
- **Interop shapes:** the composition model and the REST API follow the
  REST JSON shape existing VJ controllers use, on purpose. Keep it that way.
- Add tests for logic (model, parsing, import, DSP, undo). Keep them fast and
  independent of hardware, network and the user's files; a test that needs an
  installed app or file must skip quietly when it's missing.
- Comments say what the code does and why, not where an idea was copied from.

## Seeing the UI without a human

Debug hooks write a screenshot of the main window after about 5 seconds:

```sh
LAVMFX_SCREENSHOT=/tmp/shot.ppm timeout 20 cargo run --release
```

The app keeps running after the shot, so always wrap it in `timeout`. More
`LAVMFX_SCREENSHOT_*` variables open particular panels or states first:
`grep -rn "LAVMFX_SCREENSHOT_" app/lavmfx/src`. Use a scratch
`XDG_CONFIG_HOME` so the user's settings aren't changed, and a null audio
sink, or none, so nothing is heard.

Other hooks: `LAVMFX_PRESENT=vsync|novsync|fifo|mailbox|immediate` (present
mode), `WGPU_BACKEND=vulkan|dx12|…`, `RUST_LOG=…` (Windows release builds log
to `%APPDATA%\lavmfx\data\lavmfx.log`; the first line carries the build
stamp, commit + build time). Crashes are logged, and a watchdog logs the
last `prof::mark` when the UI thread stops for over 2 s: add a mark around new
long-running steps of the frame.

Output windows are egui **immediate** viewports on purpose: deferred ones
stopped redrawing on Windows. Don't switch them back.

## Packaging

Package scripts live in `packaging/` (see its README). Keep two things intact:
`remap` in `packaging/common.sh`, which keeps build-machine paths out of the
binaries, and `copy_licenses`, so every package carries `LICENSE` and
`licenses/`. When the bundled FFmpeg changes, update `packaging/THIRD-PARTY.md`.

## Commits and pull requests

- Only commit or push when the human asks you to. Work on a branch, not `main`.
- One topic per commit or PR. Explain what changed and why; include a
  screenshot for UI changes.
- Say in the PR that it was made with an AI agent, and which one. A human must
  have read and tested the change before it's submitted.
- Don't commit build output (`app/target/`, `dist/`) or files outside the
  change.
