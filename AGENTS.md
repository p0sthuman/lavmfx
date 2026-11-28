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

On Windows: the MSVC toolchain, CMake, LLVM (libclang) and an FFmpeg 8 / 9
shared build; the environment variables are listed under "Building on Windows"
in [packaging/README.md](packaging/README.md). The app and the tests need
FFmpeg's `bin\` on `PATH`.

Before you say a change is done, run **`tools/check.sh`** (from Git Bash on
Windows). It runs rustfmt, clippy with `-D warnings` and the tests, and on Linux
also clippy for Windows through the cross toolchain. It ends with a summary.
Report that summary as it is: every FAIL with its output, and every NOT CHECKED
line. A test that skips still says "ok", so it is not a pass. Never write
"tests pass" when the summary lists skips that touch your change.

## Working across Linux, Windows and macOS

The app ships for Linux and Windows; macOS is planned. Each system compiles
only its own `cfg(...)` code, so a change can build here and break elsewhere.

- **CI** (`.github/workflows/ci.yml`) runs `tools/check.sh` on Linux and Windows
  for every push and pull request, and compile-checks macOS. Don't merge a
  branch whose CI is red, or whose CI hasn't run.
- **Locally:** on Linux, `tools/check.sh` also checks the Windows code. On
  Windows nothing checks the Linux code, so push the branch and read CI before
  you say it works.
- **Keep platform code apart.** Put each system's code in its own `cfg` module
  with the same functions (see `mod context` in `crates/io/src/ffgl.rs`, or
  `share.rs` / `spout.rs`), and keep shared logic outside the `cfg`. When you
  move code you can't compile, move it without changing it, in its own commit.
- **Test inputs come from a script, not a scratch folder.** If a test needs
  files from outside the repo (plugins, an SDK, media), add or reuse a script
  in `tools/` that fetches a pinned version and builds them (for example
  `tools/ffgl-test-plugins.sh`). The test skips when they're missing, with a
  message that contains "skipping" and names the script.
- **Handoffs.** If you can't build or run part of a change on a system it
  affects, write `docs/<topic>-handoff.md` with a "Do on <system>, before
  merging" list of exact commands. Link it at the top of this file. The next
  agent on that system works through the list and records the results there.
  The handoff and its link are deleted when the branch is merged.

| Needed for | Linux | Windows | macOS |
|---|---|---|---|
| Build + tests | here | MSVC; or cross-check from Linux (`tools/check.sh`) | CI only (compile check) |
| FFGL host test | `tools/ffgl-test-plugins.sh`, then `tools/check.sh` | no test plugins yet: try one plugin by hand | not built |
| Packages | `packaging/build-linux-*.sh` | `packaging/build-windows-native.sh` (Git Bash) | not yet |

`packaging/*` scripts write into `dist/` under the release's version number.
Before running one for a check, move that version's existing packages aside
and put them back afterwards, so you don't overwrite a published release.

## Layout

| Path | What's there |
|---|---|
| `app/crates/core` | Composition model, parameters, undo history, effect / source catalogues (`data/*.json`), parameter animation (`drive`), clock / transport, control addresses (OSC / REST), DMX and output models |
| `app/crates/render` | wgpu compositor, WGSL effects (`src/shaders/effects.wgsl`), slices and warping, ISF |
| `app/crates/media` | FFmpeg decoding, player threads, HAP, audio files, recording, NDI, shared video (`share.rs`: PipeWire; `spout.rs`: Spout), capture |
| `app/crates/audio` | Audio devices, FFT, the audio mixer and audio effects |
| `app/crates/io` | OSC, MIDI, Ableton Link, REST server, FFGL host (EGL / WGL), Art-Net / sACN |
| `app/crates/project` | `.lavmfx` load / save, `.avc` import, media path resolution, fixture library |
| `app/lavmfx` | The app (egui): `src/app.rs` main window, `src/app/*` panels, `src/output.rs` output windows |
| `tools/` | `check.sh` (the checks before "done"), `ffgl-test-plugins.sh` (FFGL test plugins) |
| `.github/workflows/` | CI: Linux, Windows, macOS |
| `packaging/` | Portable build scripts (Linux native, Linux portable + AppImage in a podman container, Windows cross-build, Windows native build + installer) |

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
- Render tests take their GPU from `lavmfx-render`'s `test_gpu::gpu()`: a
  device per test hung the run on Windows.
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

On Windows `XDG_CONFIG_HOME` is ignored: settings and the log are always in
`%APPDATA%\lavmfx`. Check that the app isn't running, move that folder aside,
run, and put it back. There is no `timeout` for this either: start the process
and end it after about 20 s.

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
