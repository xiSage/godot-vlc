# Building LibVLC

This directory is the only place LibVLC is produced. Nothing here downloads a
prebuilt binary, and nothing here depends on a distribution package.

## Why it exists

The Linux runtime used to be produced by extracting VLC's official snap package
and editing the result by hand, and the Windows runtime came from a nightly
archive whose URL expires. Neither process was recorded, and the Linux runtime
turned out to be structurally unable to work:

- its plugin modules were linked against the snap runtime's libraries
  (`libavformat.so.60`, `libvpx.so.9`, `libFLAC.so.12`, `liblua5.2.so.0`, ...),
  none of which were shipped, so they only loaded while the host happened to
  provide those exact sonames;
- the plugin modules carried no `RUNPATH` at all, because inside the snap they
  found their dependencies through the snap's `LD_LIBRARY_PATH`;
- when a plugin fails to load, LibVLC logs one error and carries on, so the
  visible symptom was "Codec `h264` is not supported" rather than "a library is
  missing".

Building from source with VLC's `contrib` tree fixes this at the root: contrib
libraries are linked into the modules statically, so the runtime becomes
self-contained.

## Layout

```
build/vlc/
  Dockerfile         toolchain only; sources are fetched at run time
  build.ps1          fetch -> verify pin -> patch -> contrib -> make -> stage
  postprocess.ps1    normalise RUNPATHs so the tree is relocatable, then assert
  check-runpath.ps1  the runpath rule, with a self-test
  lib/               shared helpers: the runpath rule, native-command wrappers
  vlc.lock           the pinned VLC revision and the ABI it must report
  patches/           every modification we make to the VLC sources
```

## Building

```sh
docker build -t godot-vlc/vlc-build build/vlc
docker run --rm -v "$PWD/artifacts:/out" godot-vlc/vlc-build linux-x64 /out
docker run --rm -v "$PWD/artifacts:/out" godot-vlc/vlc-build win-x64   /out
```

Both commands produce `artifacts/vlc-<platform>.tar.gz`:

```
include/vlc/**     headers (consumed by build.rs / bindgen)
lib/**             the shipped runtime
libexec/vlc/**     the out-of-process preparser and the plugin cache generator
share/vlc/**       the Lua playlist parsers and service discovery scripts
tools/vlc          the CLI, for looking at a runtime by hand, never shipped
build-info.txt     provenance: commit, ABI version, whether GPL was disabled
```

`scripts/stage_libvlc.ps1` unpacks it into `thirdparty/vlc/<platform>/`; pass
`-IncludeTools` to also stage `tools/`, which is for running the runtime by hand.
Nothing that is not shipped is copied into the addon: `assemble_addon.ps1` copies
what the `.gdextension` manifest declares, and `libexec/` and `share/vlc` are
declared because VLC cannot start an interface without them.

The contrib build is the slow part; upstream quotes one to two hours for the
full set, and it is the `make -C contrib` step rather than `contrib/bootstrap`,
which only writes a Makefile. `VLC_BUILD_JOBS` controls parallelism.

Mount a volume at `/tmp/vlc-build` when building locally, otherwise every run
re-fetches VLC and restarts the contrib build from nothing:

```sh
docker run --rm -v "$PWD/artifacts:/out" -v godot-vlc-work:/tmp/vlc-build \
    godot-vlc/vlc-build linux-x64 /out
```

In CI the whole container run is skipped on a cache hit, keyed on the contents
of `vlc.lock` and the build scripts.

## Build configuration

Contribs are built with `--disable-gpl --disable-gnuv3 --enable-ad-clauses`, the
same set VideoLAN's own Apple, Android and wasm builds use.

- `--disable-gpl` drops GPL-licensed libraries. The notable one is x264, an H.264
  *encoder*, which a player does not need; decoding H.264/VP9/AV1 goes through
  avcodec and dav1d and is unaffected.
- `--disable-gnuv3` drops (L)GPLv3-only libraries such as libidn2.
- `--enable-ad-clauses` is **not optional**. `freetype` is the only package in the
  entire contrib tree gated on `AD_CLAUSES`; without the flag the build stops
  with `Package "freetype" requires the GPL license`. freetype is VLC's subtitle
  and OSD text renderer, so leaving it out costs a player visible functionality.

`contrib/bootstrap` reports the resulting licence string itself. With this set it
prints `Lesser GPL version 2.1, with advertisement clauses` — upstream's own
designation for the combination, not this project's interpretation. That matters
for the wording in `README.md`: the runtime is LGPL *with advertisement clauses*,
which is what VideoLAN ships, and it is not the same claim as "plain LGPLv2.1".

The benefit over a default build is that the plugin tree contains no viral GPL
modules, so there is no need to explain to users of a commercial Godot project
which GPL plugins were bundled.

`--disable-vlc` is deliberately *not* passed. The `vlc` CLI is the cheapest way to
look at a runtime by hand when the acceptance test reports something, and it is
staged under `tools/`. It is a diagnostic artifact, not a delivered one, and not
what the acceptance test uses.

## The glibc floor

The addon's shared library is `dlopen`ed into the Godot process, so its glibc
requirement must not exceed that of the official Godot Linux binaries.
Otherwise Godot starts and the addon silently fails to load.

The official requirement is not documented reliably; measure it from the binary
instead:

```sh
objdump -T godot | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1
```

The base image is `ubuntu:22.04`, i.e. glibc 2.35 — the same floor the extension
is compiled against on the CI runner, so neither half raises the other's. See the
top of the `Dockerfile` for why an older base was abandoned: it cannot build a
2026 VLC.

Change `BASE_IMAGE` only together with this bound. The supported range is a
product decision, and `scripts/check_glibc_floor.ps1` enforces it across the
whole runtime rather than only the extension.

## Updating the pin

1. Set `VLC_COMMIT` in `vlc.lock`.
2. Build once. If the build reports an ABI mismatch, copy the reported
   `VLC_API_VERSION_STRING` into `vlc.lock` — the plugins and the core must
   report the exact same string, so a stale value must never be waved through.
3. Re-run the dependency gate in `scripts/check_addon.ps1`, which asserts that
   every runtime file the addon ships is declared in the `.gdextension`
   manifest and that no plugin depends on a library the host is not guaranteed
   to provide.
4. Re-run the acceptance test on both platforms.

## Known limitations

Every stage of this pipeline has been executed for real, on both platforms, from
the pinned revision.

Established by running it:

- the pinned revision `546e18e53e` exists and reports
  `VLC_API_VERSION_STRING=4.0.6`, exactly what `vlc.lock` claims, so the ABI gate
  compares against a real value;
- `--disable-gpl --disable-gnuv3` takes effect: `contrib/bootstrap` reports
  "Packages licensing... Lesser GPL version 2.1";
- `contrib/bootstrap` only writes a Makefile; compiling the contrib packages is a
  separate `make`. It must also run from `contrib/<triplet>/`, because it writes
  that Makefile into the current directory — its own header says "if ../bootstrap
  is run again". Run from the repository root it leaves `make` with nothing to
  build. Without a built contrib tree `configure` fails on lua outright and
  silently loses zlib, libidn and dbus, so a missing contrib tree is now a hard
  error rather than a fallback to system libraries;
- `--disable-gpl --disable-gnuv3` alone is not enough: the build stops with
  `Package "freetype" requires the GPL license`, because freetype is the only
  contrib package gated on `AD_CLAUSES`. `--enable-ad-clauses` is required, and
  it is what makes `contrib/bootstrap` report the licence as "Lesser GPL version
  2.1, with advertisement clauses";
- debian:11 cannot build a 2026 VLC. Its autoconf is 2.69 while libtheora
  requires 2.71, its meson is 0.56 while harfbuzz requires 0.60, and its gcc
  (10) and cmake (3.18) are equally behind. Back-porting each of those is not a
  frozen toolchain but a pile of exceptions, so the base is ubuntu:22.04
  instead — which also happens to have exactly the glibc floor the extension is
  already compiled against, so nothing about the supported range changes;
- meson is pinned in the image rather than taken from the base. Ubuntu 22.04's
  0.61.2 satisfies today's contrib packages, but contrib tracks upstream closely
  and the version a package demands moves; VLC ships no meson of its own
  (`extras/tools` builds cmake, nasm, libtool, tar and xz only);
- the container is missing a few build dependencies that only surface once a
  package that needs them is reached. `python3-venv` is one: contrib creates a
  Python virtualenv for packages such as glad, and without it the venv fails
  with "ensurepip is not available". The largest is the X11/xcb development set,
  which Vulkan-Loader needs for its Xlib WSI (`x11` pkg-config module, then
  `X11/extensions/Xrandr.h`) — contrib provides xcb itself, but the Vulkan
  loader and VLC's X11 outputs link the system X11 stack. Treat a missing-tool
  error from contrib as a gap in this image rather than a reason to change the
  build flags, and note that `contrib/bootstrap --disable-vulkan-loader` is the
  official way to drop that chain, at the cost of Vulkan on both platforms;
- `lib/` is not the whole runtime. `libexec/vlc` (the out-of-process preparser,
  the plugin cache generator and the compiled Lua scripts) and `share/vlc` (the
  Lua playlist parsers) are needed too, and missing them is close to invisible:
  every plugin still loads, and the failure only appears as `Fail to create
  Process in process_pool` and then `cannot start any interface`. Both are staged
  now, and `scripts/stage_libvlc.ps1` refuses an artifact that lacks them;
- `contrib/bootstrap --disable-<package>` adds the package to `PKGS_DISABLE`,
  which stops it being built but does **not** uninstall it. In a work directory
  that already holds it, disabling a package therefore has no effect at all until
  its installed files and stamp are removed as well. This cost a build cycle;
- the measured host-dependency surface of the finished Linux runtime is **zero**:
  `ldd` over all 359 plugins finds no unresolvable library. Reaching that
  required dropping `libplacebo` and `vulkan-loader` on Linux, because contrib
  installs a *shared* `libvulkan.so.1` that `libplacebo_vk_plugin` then needs from
  the host. A one-off measurement before the change showed exactly one such
  library and exactly one plugin needing it;
- VLC's own CLI refuses to run as root ("VLC is not supposed to be run as root"),
  so the acceptance test has to run as an ordinary user. CI runners are ordinary
  users; a `docker run` as root is not, and the failure looks like a plugin
  problem;
- the two platforms are built from different base images, and that is not an
  inconsistency: the Linux runtime must sit at or below the extension's own glibc
  floor, while a Windows runtime is PE and has no glibc dependency at all. So the
  Windows build uses ubuntu:24.04 for mingw-w64 13 (gcc 13). On 22.04's
  mingw-w64 9 (gcc 10) a 2026 VLC does not cross-compile: glslang uses
  `std::once_flag` without including `<mutex>`, ggml uses `std::mutex` and
  `std::thread`, and libaribcaption calls `GetUserDefaultLocaleName`. gcc 13
  builds all three, but it does not rescue ggml entirely: mingw-w64 lacks the
  *thread* power-throttling API at v9 and at v13 alike, so ggml stays out of the
  Windows runtime for a reason no compiler version addresses. The
  distribution-dependent package names this creates are resolved at build time
  rather than named, so one Dockerfile serves both bases;
- VLC's Vulkan stack is left out of both platforms: libplacebo (whose outputs the
  extension cannot reach, since it renders through output callbacks), the
  Shaderc/SPIRV compiler that only libplacebo pulls in, and the Vulkan loader. On
  Linux this is what makes the runtime self-contained; on Windows it removes a
  package that does not compile there anyway;
- a `PKGS_DISABLE` entry does not survive a dependency. `contrib/src/sam3/
  rules.mak` carries `DEPS_sam3 = ggml $(DEPS_ggml)`, so disabling ggml alone
  still builds it, and glslang is only ever built because libplacebo requires it.
  Disabling a package means disabling everything that depends on it;
- `libass` cross-compiles only if an assembler named for the target is named in
  the meson machine file. contrib does not put one there, and meson does not guess
  a prefix for nasm, so `patches/0001-...` adds the entry and the image provides
  `x86_64-w64-mingw32-nasm`. Without it: "Assembly was requested, but cannot be
  built";
- anything COPYed into the image is a build-time snapshot, and `build.ps1` and
  `patches/` both are. A change to either is not in effect until the image is
  rebuilt, which is how a run appeared to succeed while still using the previous
  script: the image still held the version that disabled a package. When a run's
  behaviour does not match the working tree, rebuild the image before reading
  anything into it;
- a patch added to `build/vlc/patches/` is not in the image until the image is
  rebuilt; `COPY patches/` is a build-time snapshot, and a stale image reports
  "no patches to apply" while the file sits in the working tree;
- the apt sources are the live Ubuntu archive, not a snapshot. Snapshot pinning
  was tried first because Debian's live archive had to be abandoned for
  snapshot.debian.org when it began failing with "Release file is expired", but
  snapshot.ubuntu.com serves HTTPS only and apt cannot use an HTTPS source before
  ca-certificates is installed, so the source is silently ignored
  (`Ign:... InRelease`) and the build stalls. Ubuntu's live archive does not
  publish the short-lived Release files that broke Debian's. The package set is
  therefore not pinned, which is recorded under Known limitations;
- every script parses and self-tests under the container's pwsh 7.4.6 on Linux.

Both platforms build and are verified. The numbers below come from actual runs,
not from expectations:

| | linux-x64 | win-x64 |
|---|---|---|
| artifact | `vlc-linux-x64.tar.gz`, 307.2 MB | `vlc-win-x64.tar.gz`, 355.3 MB |
| plugin modules | 359 | 383 |
| host dependencies | none: `ldd` over every plugin resolves | none: `objdump` over 388 binaries resolves |
| decoding | `scripts/acceptance_test.ps1` passes | the libvlc decode test passes |
| pin | `4.0.0-dev-37536-g546e18e53e`, API 4.0.6 | the same revision and ABI |

Solved, and worth recording because the symptom was so misleading:

- `libgme_plugin.dll` failed to load on Windows with error 126
  (`ERROR_MOD_NOT_FOUND`), the only one of the 383. Its only dependency beyond
  the core was `libgcc_s_seh-1.dll`, which the package shipped, and putting that
  DLL in the loader's first search location did not help, so it was not path
  resolution.
  
  The cause was in LibVLC, not in the package: it opens every plug-in with
  `LoadLibraryExW(path, NULL, LOAD_LIBRARY_SEARCH_SYSTEM32)`
  (`src/win32/plugin.c`), so a plug-in's dependencies are searched for in System32
  and nowhere else -- not the application directory, not the plug-in's own
  directory, and not `PATH`. A dependency already loaded in the process still
  satisfies the import, which is how every plug-in resolves `libvlccore.dll`; the
  mingw runtime was not loaded by anything, because this build's `libvlccore.dll`
  does not import it. Upstream does not meet this: its Windows build either links
  the runtime statically (`extras/package/win32/build.sh` sets
  `-Wl,-l:libunwind.a -Wl,-l:libpthread.a -static-libstdc++` for its clang builds)
  or its own `libvlccore.dll` imports the runtime first.

  The fix is the build-side one rather than a host-side workaround: Windows links
  with `LDFLAGS='-static-libgcc -static-libstdc++'`, applied to contrib as well as
  to VLC, because a plug-in links contrib's static libraries and a `.la` that
  records `-lgcc_s` brings the shared runtime back at the final link whatever that
  link asks for -- setting it for configure alone was measured to have no effect.
  Measured after the change: **none** of the 383 plug-ins imports a mingw runtime
  DLL, and the package no longer ships `libgcc_s_seh-1.dll` at all.

The acceptance test drives LibVLC directly, from `src/acceptance.rs`, rather than
the `vlc` command-line tool. The CLI was tried first and abandoned: it is not what
ships, and on Windows it is a GUI-subsystem binary that needs a console it may not
be able to attach, so it wrote its output to `vlc-help.txt` and blocked in a modal
"Press the RETURN key to continue" dialog. Four separate problems came from that
harness -- the dialog, its non-hermetic configuration, plugin discovery relative
to the CLI rather than the runtime, and an unbounded wait -- and none of them
concerned the bytes under test. A library test has none of them.

The build environment is pinned by *inputs*, not by hashes. One gap remains and
is deliberate rather than overlooked:

- The base image is referenced by tag (`ubuntu:22.04`) rather than by digest, and
  the apt package set is not pinned either: snapshot.ubuntu.com serves HTTPS
  only, and apt cannot use an HTTPS source before ca-certificates is installed,
  so the source is ignored and the build stalls. The live archive is used
  instead; unlike Debian's, it does not publish the short-lived Release files
  that forced snapshot.debian.org the first time round. Resolve the digest with:

  ```sh
  docker buildx imagetools inspect ubuntu:22.04 --format '{{.Manifest.Digest}}'
  ```

  Add the digest to `BASE_IMAGE` once the pipeline is known to build at all;
  pinning a base that cannot build is not progress.

The runtime and the extension are both dlopen()ed into the Godot process and are
therefore subject to the same glibc floor. They are not built in the same place —
the runtime here, the extension on the CI runner — but they are built on the same
distribution, ubuntu 22.04, which is why `glibc-baseline.txt` is 2.35 for both and
why neither can raise the other's requirement. `scripts/check_glibc_floor.ps1`
measures the assembled runtime as well as the extension and fails if either
exceeds the bound.
