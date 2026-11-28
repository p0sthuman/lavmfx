# Handoff: Windows session, 2026-11-28

For the next agent or developer on the Linux side. It records what a Claude Code
session on Windows changed on top of `4500027` (LAVMFX 0.2.5), what was checked
and what wasn't. Permanent rules are in `AGENTS.md` and `packaging/README.md`;
delete this file, and its pointer in `AGENTS.md`, once the checks under
"Do on Linux" are done and the work is merged.

## Do on Linux, before merging

**None of this work has been compiled or run on Linux.**

1. `cargo build --release`, `cargo test --workspace`,
   `cargo clippy --workspace --all-targets` (from `app/`).
2. `crates/io/src/ffgl.rs` was restructured. The EGL setup moved, logic
   unchanged, from `gl::Context::new` into `mod context` (`context::create`);
   `mod gl` now holds only the shared texture / process code. Check that FFGL
   still works: `LAVMFX_FFGL_TEST=<folder> cargo test -p lavmfx-io loads_and_runs
   -- --nocapture`, and one plugin in the app.
3. Render tests now share one GPU device (`crates/render/src/test_gpu.rs`).
   `slices.rs` tests used a default device before; they now get the same
   features as the others.
4. `crates/project` tests: `paths.rs` compares canonical paths, `avc.rs`
   compares `Path`s.
5. Packaging: `common.sh` calls `"${PYTHON:-python3}"`; run one Linux package
   script. `.gitattributes` keeps `*.sh` at LF.
6. Dependencies: `glow` moved to `cfg(any(linux, windows))` in
   `crates/io/Cargo.toml`; `windows-sys` and `windows` were added for Windows
   only (both MIT / Apache-2.0). `Cargo.lock` gained those two edges, no version
   changes.

## Fix: white line on fullscreen outputs

`output.rs` `place_window` now sends `ViewportCommand::Decorations(false)` right
after `SetMonitor`. egui 0.37 gives a borderless window that wasn't created
fullscreen winit's "undecorated shadow", whose drawing area starts 1 px down.
On a 150% display: before the fix, window at `2560,0` 2560×1440 with its
drawing area at `2560,1`; after, `2560,0`. The maintainer confirmed it on
screen. Windows only (`place` is None elsewhere). Best kept as its own commit.

Before the fix, the maintainer also saw F11 and double-click behave differently
from each other on these windows. The cause of that wasn't looked into.

## What changed

| Area | Change | Why |
|---|---|---|
| FFGL on Windows | WGL 4.1 core context on a message-only (never shown) window; plugins load with `LOAD_WITH_ALTERED_SEARCH_PATH` | was a stub |
| Spout | new `crates/media/src/spout.rs`, the Windows `imp` of `share.rs`: sender list, receivers, composition output; Rust, protocol as in Spout2 (BSD), no code copied | was a stub |
| Tests | shared GPU device; two path assertions | render tests hung and two project tests failed on Windows |
| Packaging | `build-windows-native.sh`, `windows-installer.iss`: portable `.zip` and per-user setup `.exe`; MSVC runtime DLLs bundled; fails if the exe holds build paths | build on Windows itself |
| Docs | README, ROADMAP, CHANGELOG ("Unreleased"), `packaging/README.md`, `THIRD-PARTY.md`, `AGENTS.md`; FFGL note in Preferences | |

The app builds on Windows (MSVC) without any source change.

## Results on Windows

- `cargo test --workspace`: 288 passed, 0 failed, 7 ignored. Clippy: no warnings.
- FFGL: a source and a mirror effect written for this check (not kept in the
  repo) ran correctly: parameter, input texture, row order, several sizes.
- Spout: the loopback test (our sender to our receiver) passes.
- The release build started with the screenshot hook (before FFGL and Spout were
  added). The packages built afterwards pass the script's DLL and path checks.
- The maintainer then checked by hand: the installer, Spout in and out, and the
  app's main functions all pass. **FFGL was not tried by hand**, and no
  third-party FFGL plugin has been run: don't claim it as tested.

Toolchain: Windows 11, Rust 1.100.1, VS 2022 Build Tools (MSVC 14.44, SDK
10.0.22621), LLVM 23.1.6, Inno Setup 6.7.3, the pinned FFmpeg n9.0.2.

## Open points

- `cargo fmt --all --check` flags one line in `crates/core/src/history.rs`. It
  predates this work; the maintainer asked to leave it.
- Render test hang: a device per test, created in parallel, on a two-GPU laptop
  with the default backends. Sharing the device fixed it; where the drivers
  stalled wasn't traced.
- Spout frames go through system memory (no GPU-to-GPU path); cost at 1080p / 4K
  not measured. Sender names outside ASCII may not match other apps (ANSI names).
- FFGL on Windows uses the GPU Windows assigns the program for OpenGL (the GPU
  name hint is ignored). Official FFGL SDK sample plugins were not built.
- `build-windows.sh` (cross-build) and `build-windows-native.sh` name their
  `.zip` the same; choose which one releases use. The cross-build wasn't run
  with these changes: check that the new `windows` / `windows-sys` code builds
  for `gnullvm`.
- The Windows packages aren't code-signed.
