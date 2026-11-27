# Contributing to LAVMFX

Thanks for wanting to help. Bug reports, testing on your hardware, effects,
fixture definitions, docs and code are all welcome.

## Reporting bugs

Open an issue with:

- what you did, what you expected, and what happened instead
- your system: OS and version, GPU and driver, how you got LAVMFX (a release
  package or your own build, and which commit)
- the log: the terminal output on Linux, or `%APPDATA%\lavmfx\data\lavmfx.log`
  on Windows (its first line names the build)
- for playback problems, the codec and resolution of the file (`ffprobe`
  output is ideal). Don't attach media you aren't allowed to share.

For security problems, don't open an issue: see [SECURITY.md](SECURITY.md).

## Before larger changes

Open an issue first for new features, new dependencies or bigger refactors, so
we can agree on the approach before you spend time on it.
[ROADMAP.md](ROADMAP.md) shows what's planned.

## Building

You need Rust 1.95 or newer, a C/C++ compiler, CMake, clang (for bindgen), and
the development files of FFmpeg 8 or 9, PipeWire and ALSA (Linux).

```sh
cd app
cargo run --release
cargo test --workspace
```

Portable packages are built with the scripts in [`packaging/`](packaging/README.md).

## Code layout

The Rust workspace is in `app/`:

| Crate | What's in it |
|---|---|
| `lavmfx-core` | Composition model, parameters, undo history, effect / source catalogues, parameter animation, clock and transport, DMX and output models |
| `lavmfx-render` | The wgpu compositor, effects (WGSL), slices and warping, ISF shaders |
| `lavmfx-media` | FFmpeg decoding, the player threads, HAP, audio files, recording, NDI, PipeWire sharing, capture |
| `lavmfx-audio` | Audio devices, FFT analysis, the audio mixer and audio effects |
| `lavmfx-io` | OSC, MIDI, Ableton Link, the REST server, FFGL loading, Art-Net / sACN |
| `lavmfx-project` | `.lavmfx` load / save, `.avc` import, media path resolution, fixture library |
| `lavmfx` | The app: main window, panels, output windows |

## Pull requests

- Keep each PR to one change, and explain what it does and why.
- Before you push, run:
  ```sh
  cd app
  cargo fmt --all
  cargo clippy --workspace --all-targets
  cargo test --workspace
  ```
  Clippy should stay free of warnings.
- Add tests for logic changes (model, parsing, import, DSP). For UI changes,
  add a screenshot to the PR.
- UI code uses the shared widgets in `app/lavmfx/src/app/widgets.rs`, and
  colours come only from `app/lavmfx/src/app/theme.rs`. Put new styling there,
  not inline in a panel.
- Text must never spill over other widgets: long labels end in "…" and show the
  full text on hover.
- Every change to the composition goes through the undo history.

## Working with other apps' formats (important)

LAVMFX reads some formats of other VJ software, for example Resolume Arena's
`.avc` compositions. This is an independent project, so:

- **Don't add other vendors' files**: no copied presets, shaders, fixture
  libraries, help texts, icons, screenshots or sample media from Resolume or
  any other product.
- **Don't add decompiled or disassembled code** from any closed-source app, or
  code translated from it.
- Base compatibility on public documentation, on files you made yourself, and
  on how an app behaves when you use it. Parameter names, ranges and file
  structures needed to read a format are fine. Creative content (presets,
  looks) is not.
- In code comments, describe what a format or behaviour *is*, not where you
  copied it from.

If you aren't sure whether something is OK, ask in the issue first.

## Licence of contributions

LAVMFX is licensed under the [GNU GPL v3.0 or later](LICENSE). By submitting a
contribution, you agree that it's licensed under the same terms, and that you
have the right to submit it. Only submit code, shaders and assets that you
wrote yourself, or that come under a GPL-3.0-compatible licence (say which one
in the PR).
