# Roadmap

LAVMFX is built in milestones, and each one ends with something you can run.
This page shows what's done, what's being worked on, and what comes later.
Nothing here is a promise or a date. The order changes as the project learns
what live users need most.

Priorities, highest first: **live VJ work (clips and effects) › projection
mapping › audio-reactive control › DMX**.

## Done

| Milestone | What it brought |
|---|---|
| Playback | Video decoding through FFmpeg (H.264, HEVC, ProRes, HAP, DXV and more), hardware decoding where available, scrubbing |
| Clip grid | Decks of layers × columns, clip and column launch, thumbnails |
| Compositing | Layer opacity, blend modes, transitions, master fader, crossfader, groups |
| UI shell | Docking panels, saved layouts, a shared theme and widget set (light / dark) |
| Effects | About 100 GPU video effects and 17 generators, effect chains on clips, layers, groups and the composition, user presets, parameter animation (timeline, BPM, envelopes) |
| Audio-reactive | Audio input and clip audio, FFT bands driving any parameter, dashboards |
| Projection mapping | Fullscreen per display, slices, input crop, corner-pin / bezier / grid warp, masks, soft edges, polygon slices, test card, video routing and feedback sources |
| Live control | Keyboard and MIDI learn, OSC, a REST API, Ableton Link, tap tempo, quantised launch, SMPTE timecode in (LTC / MTC), an Open Stage Control tablet layout, undo for every change |
| Interop | Import of Resolume Arena `.avc` compositions, ISF shaders, NDI in / out, PipeWire video sharing and capture devices (Linux), Spout video sharing (Windows), FFGL plugins (Linux, Windows) |
| Recording | Record the output (with audio), snapshots, render clips with their effects; codec presets |

## Being finished

- **DMX / pixel mapping**: Art-Net and sACN output, lumiverses and fixtures,
  a fixture library and editor, Art-Net input. Built; it closes after hands-on
  testing.
- **Audio effects**: Bitcrusher, Distortion, EQ-3, Flanger, HighPass and
  LowPass on clip, layer, group and composition audio. Built; it closes after
  listening tests.
- **Slice Chase** (built-in step sequencer for slices): sequences saved in the
  composition (from a rule or hand-made) and the Slice Chase Sequencer effect.
  Six effects: Sequencer, Wipe, Runner, Tiles, Sample and Delay. Built; being
  tuned.

## Next: shipping

- Portable builds are made for Arch / CachyOS, other Linux distributions
  (tarball and AppImage) and Windows; they still need testing on more
  machines. macOS builds from source.
- Installers next: an Arch `PKGBUILD`, a Windows installer and a macOS
  package.
- Then GitHub CI and the first stable release (0.x are pre-releases).

## Later

These are wanted, but not scheduled yet:

- **Platforms**: Syphon (macOS) texture sharing, FFGL plugins on macOS, Spout
  GPU-to-GPU transfer, more capture formats.
- **Live control**: MIDI clock in / out, controller presets (APC40, APC mini,
  nanoKONTROL2), OSC learn, more REST endpoints.
- **Playback**: beat-sliced loops, more crossfader behaviours, zero-copy
  hardware decoding.
- **Audio**: VST plugins, level meters, per-output channel routing.
- **Effects**: ISF transitions and image / audio inputs, shader auto-reload.
- **NDI**: audio, per-layer and per-screen outputs.
- **DMX**: sACN input, ArtSync, node discovery.
- **UI**: panels in separate OS windows, multi-screen layout presets.
- **Ideas**: a "collect project" command (all media and settings in one
  folder).

Want to help with something here? See [CONTRIBUTING.md](CONTRIBUTING.md). Open
an issue first for anything big, so we can agree on the approach.
