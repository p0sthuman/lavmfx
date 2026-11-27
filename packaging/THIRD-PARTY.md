# Third-party software in LAVMFX packages

LAVMFX itself is Copyright (C) 2026 p0sthuman, under the GNU General Public
License v3.0 or later (`LICENSE`). Its source code:
<https://github.com/p0sthuman/lavmfx>.

## Rust crates

Every crate compiled into the program, with its licence and licence texts, is
listed in `licenses/rust-crates.txt`. All of them are under licences
compatible with the GPL-3.0 (mostly MIT / Apache-2.0; Ableton Link through
`rusty_link` is GPL-2.0-or-later).

## FFmpeg (bundled in the Windows `.zip`, the portable Linux `.tar.gz` and the `.AppImage`)

The FFmpeg libraries (`avcodec`, `avformat`, `avutil`, `avfilter`,
`avdevice`, `swscale`, `swresample`) are a GPL build of FFmpeg, licensed under
the GNU GPL v3 or later (`licenses/FFmpeg-LICENSE.txt`). They are the shared
"gpl" build from BtbN's FFmpeg-Builds, unmodified:

- Build: <https://github.com/BtbN/FFmpeg-Builds/releases/tag/autobuild-2026-11-24-12-41>
  (`ffmpeg-n9.0.2-963-g4d831360c9-…-gpl-shared-9.0`)
- FFmpeg source, the exact commit: <https://github.com/FFmpeg/FFmpeg/tree/4d831360c9>
- Build scripts, which pin the version of every library built into FFmpeg:
  <https://github.com/BtbN/FFmpeg-Builds/tree/autobuild-2026-11-24-12-41>

If any of these links stops working, open an issue at
<https://github.com/p0sthuman/lavmfx/issues> and you'll get the complete
corresponding source, free of charge. This offer is valid for at least three
years after the release that included the build.

The Arch / CachyOS package doesn't bundle FFmpeg; it uses the system's.

## LLVM runtime (Windows `.zip` only)

`libc++.dll` and `libunwind.dll` come from the LLVM project (via llvm-mingw),
under the Apache License v2.0 with LLVM Exceptions:
<https://github.com/llvm/llvm-project>.
