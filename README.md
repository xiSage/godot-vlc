<img src="icon.svg" alt="icon" width="128"/>

# godot-vlc
[![Dynamic JSON Badge](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fgodotengine.org%2Fasset-library%2Fapi%2Fasset%2F3766&query=%24.version_string&logo=godotengine&label=asset%20library&labelColor=333639)](https://godotengine.org/asset-library/asset/3766)

VLC extension for Godot. Supports Godot 4.3 and newer. Supports Windows and Linux.
## How to use
Put media files into `res://` and they will be loaded as `VLCMedia`. Then you can play them with `VLCMediaPlayer` node.

You can also use `VLCMedia.load_from_file()` to load media from disk or `VLCMedia.load_from_mrl()` to load media from a [media resource locator](https://wiki.videolan.org/Media_resource_locator).

There are some other features, such as subtitles and chapters, can be accessed through scripts. For more information, see the in-editor documentation.

## Screenshot
<img src="img/screenshot.png" alt="screenshot">

## Supported platforms

| Platform | Status |
|---|---|
| Windows x64 | supported |
| Linux x64 | supported; requires glibc 2.35 or newer |
| macOS, Linux arm64, Windows arm64 | not supported |

Nothing has to be installed on the user's machine: the addon ships its own
LibVLC and does not use a system VLC. The Linux requirement above comes from how
the extension is built, not from a choice in the code; the bound is recorded in
`build/vlc/glibc-baseline.txt` and enforced by `scripts/check_glibc_floor.ps1`.

## Building from source

Docker is the only prerequisite. Both platforms are built from one container and
one pinned VLC commit, so the two artifacts cannot drift apart:

```sh
docker build -t godot-vlc/vlc-build build/vlc
mkdir -p artifacts
docker run --rm -v "$PWD/artifacts:/out" godot-vlc/vlc-build linux-x64 /out
docker run --rm -v "$PWD/artifacts:/out" godot-vlc/vlc-build win-x64   /out

pwsh scripts/stage_libvlc.ps1 -IncludeTools
pwsh scripts/test.ps1
pwsh scripts/build_release.ps1
pwsh scripts/assemble_addon.ps1
pwsh scripts/check_addon.ps1
pwsh scripts/acceptance_test.ps1
```

`scripts/check_addon.ps1` asserts that every file the addon ships is declared in
the `.gdextension` manifest, and that every shared object's dependencies resolve
either inside the addon or against a library the host is guaranteed to provide.
`scripts/acceptance_test.ps1` then decodes a real H.264 file **from the assembled
addon**, through LibVLC directly rather than through the `vlc` command-line tool
(which is not shipped), and fails if any plugin failed to load. See
`build/vlc/README.md` for the build environment, the pinned revision, the measured
state of both platforms, and the one known plugin that does not load on Windows.

Common failures, and what they mean:

| Message | Meaning |
|---|---|
| `'thirdparty/vlc/...' is missing` | The runtime has not been staged. |
| `... is missing but the manifest declares ...` | The runtime is incomplete: a file the package must ship is absent. |
| `... is neither shipped nor in the host-provided list` | A module depends on a library the addon does not carry and the host may not have. This is the failure that made the previous Linux runtime unusable. |
| `present in the addon but not declared` | The manifest and the runtime have drifted apart, and Godot would not export that file. |

## Known limitations

- The LibVLC build pipeline has not been run end to end yet; see
  `build/vlc/README.md` for the parts most likely to need adjustment.
- The Linux extension and the bundled LibVLC runtime are both built on ubuntu
  22.04, so both require glibc 2.35. `build/vlc/glibc-baseline.txt` records the
  bound and `scripts/check_glibc_floor.ps1` enforces it across the whole runtime.
- macOS, Linux arm64 and Windows arm64 are not supported.


## Licensing

This library is LGPL-2.1-or-later; see `LICENSE`.

The bundled LibVLC runtime is built with `contrib/bootstrap --disable-gpl
--disable-gnuv3 --enable-ad-clauses`. VideoLAN's own Apple, Android and wasm
builds use the same set, and `contrib/bootstrap` reports the resulting licence
as *"Lesser GPL version 2.1, with advertisement clauses"* — so this is LGPL with
advertisement clauses, not plain LGPLv2.1.

That is deliberate. VLC's plugin tree is licence-heterogeneous, and bundling
GPL-licensed modules such as x264 would mean answering licensing questions for
every project that uses this extension in a commercial game. What the flags
exclude is encoders and DVD access; H.264, VP9 and AV1 *decoding* go through
avcodec and dav1d and are unaffected. `--enable-ad-clauses` is what admits
freetype, VLC's subtitle and OSD text renderer, so it is not optional.
