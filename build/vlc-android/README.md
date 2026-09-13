# Building the Android LibVLC runtime

This directory is the only place the Android runtime is produced. It is not a
variant of `build/vlc/`: VLC's desktop build and VLC's Android build are two
different programs. The desktop one runs autotools and meson in a Debian image
and ships a modular runtime (`libvlccore.so` plus a `plugins` tree). The Android
one runs the NDK and ships a **monolithic** `libvlc.so` with every module linked
into it, because Android will not load a plugin tree out of an application's
assets. Both are built from the same engine revision, and `build/vlc/vlc.lock`
pins both inputs.

## Why it exists

The runtime VideoLAN publishes cannot show a picture through this extension, and
it cannot be shipped by this repository either. Those are two separate problems
with one build.

### The dummy window

Observed on an arm64 device, with the extension's software video path active:

```
looking for vout window module matching "dummy": 2 candidates
no vout window modules matched with name dummy
ERROR: LibVLC: failed to create video output
```

Audio plays throughout — the extension feeds LibVLC's `amem` output into a Godot
`AudioStreamPlayer` — and no frame ever arrives, because
`libvlc_video_set_callbacks`' lock/unlock/display callbacks belong to a video
output that was never created.

The cause is one entry in upstream's module blacklist,
`buildsystem/build-libvlc.sh`. It ends with a line reading `.dummy`, and the
list is used as a regular expression over module file names:

```sh
find $1 -name 'lib*plugin.a' | grep -vE "lib(${blacklist_regexp})_plugin.a"
```

`.dummy` matches `libwdummy_plugin.a`. LibVLC asks for a **dummy vout window**
whenever the display it uses needs no real window — which is exactly the case
for the `vmem` output this extension uses to turn frames into a Godot texture.
Remove that module and the vout cannot be built at all; no display module is
even considered, so `vmem` never runs and the callbacks are never called.

The same list has an `access_(bd|shm|imem)` entry, which matches
`libaccess_imem_plugin.a`. That is the access module behind the `imem://` MRL,
which is how `VLCMedia` hands LibVLC an in-memory buffer for media inside
`res://`. Without it nothing in a Godot project plays.

`build.sh` drops both, and then asserts on the built library that `wdummy` and
`imem_access` are present. Both assertions would have failed on the runtime we
first tried, which is the point of them.

It works, and this is what says so. On an arm64 device, with the demo and this
runtime, LibVLC reports:

```
looking for vout window module matching "dummy": 3 candidates
using vout window module "wdummy"
...
looking for vout display module matching "vmem": 1 candidates
A filter to adapt decoder I420 to display RV24 is needed
using vout display module "vmem"
```

The same device, with the runtime VideoLAN publishes, reports
`no vout window modules matched with name dummy` and then
`failed to create video output`, and plays audio with no picture.
`scripts/acceptance_test_android.ps1` asserts the difference.

### Licence

VideoLAN's Android build defaults to `--license g`, which means contribs built
with GPL-licensed libraries — `sout-x264-*`, `dvdread`, `live555` and the rest
are visible in the published `libvlc.so`. The desktop runtimes here are built
with `--license`-equivalent flags that exclude them, and `README.md` explains
why: bundling GPL modules means answering licensing questions for every project
that ships this extension. `--license a` is the Android equivalent of that
choice — LGPL v2.1 plus the advertisement clauses, which is what `vlc.lock`
records for the desktop runtime as well.

## What it produces

`artifacts/vlc-android-arm64.tar.gz`, shaped like the desktop artifacts so that
`scripts/stage_libvlc.ps1` can unpack it the same way:

```
include/vlc/**     the engine's public headers, which bindgen reads
lib/libvlc.so      the runtime, monolithic, one per ABI
build-info.txt     engine and build-system revisions, licence, ABI, NDK
```

There is no `libexec/` or `share/`: the Android platform layer compiles its data
path as `/system/usr/share`, and the monolithic build has no out-of-process
preparser to point at. There is no `libvlcjni.so` and no AAR either — the
extension is a plain GDExtension, so the Java bindings are not built
(`--no-jni`).

## Building

The same route as the desktop runtimes: a pinned container holds the toolchain,
and the sources are fetched at the pinned revisions when the container runs.

```sh
docker build -f build/vlc-android/Dockerfile -t godot-vlc/vlc-android-build build
docker run --rm -v "$PWD/artifacts:/out" godot-vlc/vlc-android-build arm64-v8a /out
```

The build context is `build/` because the recipe needs both the script and
`vlc.lock`, and the lock belongs to the desktop build as well; copying it into
this directory would be a second place to keep one revision.

The result is `artifacts/vlc-android-arm64.tar.gz`, which
`scripts/stage_libvlc.ps1` unpacks into `thirdparty/vlc/android-arm64/`.

The container is not the only way to run it, and the script is checked that way:
it runs directly on any Linux host that has the NDK, which is how it is exercised
without Docker.

```sh
build/vlc-android/build.sh arm64-v8a ./artifacts --ndk ~/android-ndk-r28b --dry-run
```

Two things the script insists on before it starts, because both are cheap to
check and expensive to discover later. The NDK must be **r28 or r29** — upstream
exits on anything else, three minutes into a configure run. And it must be the
**Linux** NDK: the download for Windows contains only a `windows-x86_64` prebuilt
toolchain, which a Linux container cannot execute.

Contribs are the slow part, an hour or two from source — the same cost the
desktop runtimes pay. `--with-prebuilt-contribs` fetches VideoLAN's instead,
which takes minutes, but only if a prebuilt exists for the licence configuration
asked for; when it does not, the fetch fails and the build falls back to source.
