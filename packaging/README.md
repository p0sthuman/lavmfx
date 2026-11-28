# Packaging

Portable builds. Each script writes into `dist/`. All are made on Linux (x86-64)
except `build-windows-native.sh`, which runs on Windows.

| Script | Makes | Runs on |
|---|---|---|
| `build-linux-native.sh` | `lavmfx-<v>-linux-x86_64-arch.tar.gz` | Arch / CachyOS with `ffmpeg` 9.x (uses the system's FFmpeg) |
| `build-linux-portable.sh` | `lavmfx-<v>-linux-x86_64.tar.gz` + `.AppImage` | other distros with glibc 2.35+ (FFmpeg bundled in `lib/`); built in an Ubuntu 22.04 podman container (`linux-portable/Containerfile`) |
| `build-windows.sh` | `lavmfx-<v>-windows-x86_64.zip` | Windows 10 / 11 x64 (FFmpeg + libc++ DLLs next to the exe; `troubleshooting/` holds the `windows-tests/*.bat` graphics tests) |
| `build-windows-native.sh` | the same `.zip` name + `lavmfx-<v>-windows-x86_64-setup.exe` (per-user installer, `windows-installer.iss`) | Windows 10 / 11 x64 (FFmpeg + Visual C++ runtime DLLs next to the exe) |

Every package carries `LICENSE` and `licenses/` (`THIRD-PARTY.md`, and
`rust-crates.txt` made by `collect-licenses.py`), and is built with
`--remap-path-prefix` so no build-machine paths end up in the binary (both in
`common.sh`).

Helpers: `make-icons.sh` (re-render the icon files after editing
`app/lavmfx/assets/lavmfx.svg`), `install-menu-entry.sh` (shipped in the Linux
folders), `lavmfx.desktop`, `common.sh`, `wintc/` (Windows cross toolchain).

## Build dependencies

Downloaded once into `~/.local/opt/lavmfx-build/` (override with
`LAVMFX_BUILD_DEPS`), each archive unpacked into its own folder with
`--strip-components=1`. Versions used so far:

| Folder | Download | SHA-256 |
|---|---|---|
| `ffmpeg-linux/` | [BtbN FFmpeg-Builds autobuild-2026-11-24-12-41](https://github.com/BtbN/FFmpeg-Builds/releases/tag/autobuild-2026-11-24-12-41) `ffmpeg-n9.0.2-963-g4d831360c9-linux64-gpl-shared-9.0.tar.xz` | `d7ca63e8f70f84eca921dc1295673854fcd756feeaff11f21a669104a54a3533` |
| `ffmpeg-win/` | same release, `ffmpeg-n9.0.2-963-g4d831360c9-win64-gpl-shared-9.0.zip` | `3fd7efe7967e056a0dd5146ff4cc5071ec03b5d1f417005a302b6482be14b6c6` |
| `llvm-mingw/` | [llvm-mingw 20261117](https://github.com/mstorsjo/llvm-mingw/releases/tag/20261117) `llvm-mingw-20261117-ucrt-ubuntu-22.04-x86_64.tar.xz` | |
| `appimagetool` | [AppImage/appimagetool 1.9.1](https://github.com/AppImage/appimagetool/releases/tag/1.9.1) `appimagetool-x86_64.AppImage`, renamed (optional; runs only inside the container) | `ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0` |

System tools: podman (rootless; the image installs Rust 1.100.0 itself), Rust (rustup) with the `x86_64-pc-windows-gnullvm` target,
clang + lld + llvm-ar + llvm-rc, cmake, rsvg-convert, ImageMagick, bsdtar.

## Building on Windows

Run `bash packaging/build-windows-native.sh` from Git Bash. It needs:

- rustup with the default `x86_64-pc-windows-msvc` toolchain
  (`winget install Rustlang.Rustup`)
- Visual Studio 2022 or its Build Tools: C++ tools, Windows SDK, CMake
- LLVM, for libclang and llvm-objdump (`winget install LLVM.LLVM`)
- Inno Setup 6 (`winget install JRSoftware.InnoSetup`)
- Python 3 and Git for Windows
- the `ffmpeg-win/` download above, in the same folder

For a plain `cargo build` outside the script, set `FFMPEG_DIR` and
`LIBCLANG_PATH`, have `cmake` on `PATH` and `RC` pointing at the SDK's
`rc.exe` (without it the exe has no icon), and add FFmpeg's `bin\` to `PATH` to
run the program or the tests.

## Windows cross toolchain (`wintc/`)

The Windows build uses the **system** clang / lld, with llvm-mingw only as the
sysroot (Windows headers, import libraries, libc++ / libunwind / compiler-rt).
No mingw-gcc, no sudo, nothing from llvm-mingw is executed. Workarounds it
carries:

- `case-include/`: `Windows.h`, `WinSock2.h`, `WS2tcpip.h` aliases (Ableton
  Link includes them with capitals; Linux file names are case-sensitive). They
  also `#undef interface` (from `rpc.h`), which breaks Link's code.
- A copy of clang's resource dir (`~/.local/opt/lavmfx-build/clang-resource-win`)
  with llvm-mingw's Windows compiler-rt, and the AMX intrinsics headers hidden
  from bindgen (`-D__LAVMFX_BINDGEN__`): bindgen can't lay out
  `__tile1024i_str` (ffmpeg-sys-next bindings fail to compile otherwise).
- `-lc++` for the final link: rusty_link asks for no C++ runtime on the
  `gnullvm` target.

After a clang upgrade, delete `clang-resource-win/` so it is rebuilt.

## Licences

The bundled FFmpeg is the GPL build, so the packages are GPL as a whole; its
licence text goes to `licenses/FFmpeg-LICENSE.txt`. Before a public release:
the project licence file and a source offer for FFmpeg (BtbN's release tag).
