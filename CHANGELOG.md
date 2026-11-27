# Changelog

What changed in each LAVMFX release. Downloads are on the
[Releases](https://github.com/p0sthuman/lavmfx/releases) page.

## Unreleased

### Added

- **Help menu**: the online read me, what's new in this version, releases,
  Report a Problem, Copy System Info (version, system and GPU for a report),
  Show Settings Folder, and **About LAVMFX** (version and build, licence,
  links). About is in the LAVMFX menu too.

## 0.2.2 (pre-release)

### Fixed

- **Arrows and the record dot showed as empty boxes** (for example "Left →
  right" in Slice Chase). The UI font now falls back to egui's bundled Hack
  font, which has them.

## 0.2.1 (pre-release)

### Fixed

- **OBS showed no video format for the PipeWire output** (Video Capture
  Device (PipeWire)), so it couldn't receive the composition. The output now
  offers each pixel format on its own, as cameras do.

## 0.2.0 (pre-release)

The first release in this repository. LAVMFX is a live VJ and
projection-mapping app: a clip grid with layers, columns and decks, about 100
GPU video effects and 17 generators, ISF shaders, six audio effects,
audio-reactive and BPM-driven parameters, projection mapping, DMX / pixel
mapping, MIDI / OSC / REST control, NDI, recording, and import of `.avc`
compositions. Builds for Arch / CachyOS, other Linux (AppImage, tarball) and
Windows; on macOS, build from source.

New since the earlier 0.1 test builds:

### Added

- **Slice Chase**: a built-in step sequencer for Advanced Output slices.
  - **Output › Slice Chase…** window: sequences of steps, each step lighting
    some slices. A sequence comes from a rule (left → right, top → bottom,
    centre → out, clockwise, shuffle, rows / columns sharing a step, screen
    and name filters), so new slices join by themselves, or is hand-made by
    clicking slices on a map of the outputs. Hand-made steps keep renamed or
    moved slices and find them by name in another setup.
  - **Slice Chase Sequencer** effect: steps on the beat (BPM), every n
    seconds, by a Position slider, on FFT hits or with Next / Previous
    buttons; directions, random slices, fade in / trails, Blast, Reset,
    Auto Reset. Space Output dims whole slices on the outputs (for slices that
    all show the full frame); Space Input masks the layer inside each slice's
    input area.
  - **Slice Chase Wipe** (gradient wipes through each slice per step:
    direction, reveal / hide / bar, softness, wipes per step, odd / even
    alternation), **Runner** (lines running round each slice's outline, or
    the outline drawn / filled up), **Tiles** (squares, circles or diamonds in
    each slice, building up per step, in the layer's colours, one colour or a
    rainbow), **Sample** (each slice filled with the colour under it) and
    **Delay** (each step of the sequence a few frames later than the one
    before). They all step like the Sequencer and work in both spaces.
  - Sequences are saved in the `.lavmfx` file; every change can be undone.
- **Event buttons** in effects (such as Next or Reset) now work from the
  panel and from OSC, MIDI, REST and keyboard shortcuts.
- **Advanced Output: several slices at once.** Ctrl+click or Shift+click
  slices (on the canvas or in the list) to select more than one; dragging one
  of them, the arrow keys and Delete then act on all of them.

### Fixed

- **The app stopped when its window was hidden** (Linux: the screen went dark
  from energy saving, or the window was hidden while switching windows).
  Playback and the output windows froze until the window showed again. They
  now keep running.
- **`LAVMFX_PRESENT=mailbox` crashed at startup** on drivers without that
  present mode (NVIDIA on Linux). The app now starts with vsync and switches
  only to a mode the window supports; otherwise it logs a warning.

### Changed

- **Outputs start off.** Starting the app no longer opens the output windows
  of the last session (on a projector or over the screen). The setup is kept;
  turn the outputs on in the Output menu.

### Known issues

- DirectX 12 (`WGPU_BACKEND=dx12`) takes up to a minute to start on Windows,
  while it compiles the shaders. Vulkan, the default, starts in seconds.
- DMX / Art-Net output hasn't been tested with real hardware yet.
- No macOS package yet: build from source (see the README).
