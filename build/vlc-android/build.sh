#!/bin/sh
#
# Builds the Android LibVLC runtime: one monolithic libvlc.so, with every module
# linked into it.
#
# README.md says why this build exists at all. In one line: the runtime VideoLAN
# publishes has no dummy "vout window" module, so LibVLC cannot create a video
# output at all, and the extension's software video callbacks are never reached.
#
# It runs inside the pinned container built from Dockerfile, which is the same
# route the desktop runtimes take. It also runs directly on a Linux host that has
# the NDK, which is what keeps the container honest:
#
#     docker build -t godot-vlc/vlc-android-build build/vlc-android
#     docker run --rm -v "$PWD/artifacts:/out" godot-vlc/vlc-android-build arm64-v8a /out
#
# Usage: build.sh [<abi>] [<output-dir>] [--ndk <path>] [--work <dir>]
#                 [--jobs <n>] [--with-prebuilt-contribs] [--dry-run]
#
#   abi         arm64-v8a (default) | armeabi-v7a | x86_64
#   output-dir  receives vlc-android-<arch>.tar.gz (default /out)
#
# The tarball is flat, exactly like the desktop artifacts, so that one staging
# script can unpack either of them:
#
#     include/vlc/**     the engine's headers, which bindgen reads
#     lib/libvlc.so      the runtime
#     build-info.txt     provenance
#
# There is no libexec/ and no share/: the Android platform layer compiles its
# data path as /system/usr/share, and a monolithic runtime has no plugin tree and
# no out-of-process preparser to point at.
#
# Both inputs are pinned in vlc.lock: the engine revision, and the revision of
# the Android build system that turns it into a library.

set -eu

script_dir=$(cd "$(dirname "$0")" && pwd -P)

# vlc.lock sits beside this script inside the container and one directory up in
# the repository; it is the same file either way.
if [ -f "$script_dir/vlc.lock" ]; then
    lock_file="$script_dir/vlc.lock"
else
    lock_file="$script_dir/../vlc/vlc.lock"
fi

abi=arm64-v8a
out_dir=/out
ndk=${ANDROID_NDK:-}
work_dir=${VLC_WORKDIR:-/tmp/vlc-build}
jobs=$( (nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4) )
prebuilt_contribs=0
dry_run=0
positional=0

usage() {
    sed -n '2,37p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case $1 in
        --ndk) ndk=$2; shift ;;
        --work) work_dir=$2; shift ;;
        --jobs) jobs=$2; shift ;;
        --with-prebuilt-contribs) prebuilt_contribs=1 ;;
        --dry-run) dry_run=1 ;;
        -h|--help) usage 0 ;;
        --*) echo "unknown option: $1" >&2; usage 1 ;;
        *)
            positional=$((positional + 1))
            if [ "$positional" = 1 ]; then abi=$1
            elif [ "$positional" = 2 ]; then out_dir=$1
            else echo "unexpected argument: $1" >&2; usage 1
            fi
            ;;
    esac
    shift
done

case $abi in
    arm64-v8a|arm64) abi=arm64-v8a; platform=android-arm64; target_tuple=aarch64-linux-android ;;
    armeabi-v7a|arm) abi=armeabi-v7a; platform=android-arm32; target_tuple=arm-linux-androideabi ;;
    x86_64) abi=x86_64; platform=android-x64; target_tuple=x86_64-linux-android ;;
    *) echo "unsupported ABI: $abi (arm64-v8a, armeabi-v7a, x86_64)" >&2; exit 1 ;;
esac

lock_value() {
    # The lock file is a flat set of key=value lines and says so itself, so it is
    # read the way its other consumers read it rather than parsed properly.
    sed -n "s/^$1=//p" "$lock_file" | head -1
}

if [ ! -f "$lock_file" ]; then
    echo "cannot find vlc.lock (looked in $script_dir and $script_dir/../vlc)" >&2
    exit 1
fi

for key in VLC_REPO VLC_COMMIT VLC_API_VERSION_STRING LIBVLCJNI_REPO LIBVLCJNI_COMMIT; do
    if [ -z "$(lock_value "$key")" ]; then
        echo "$lock_file does not define $key" >&2
        exit 1
    fi
done

vlc_repo=$(lock_value VLC_REPO)
vlc_commit=$(lock_value VLC_COMMIT)
locked_api_version=$(lock_value VLC_API_VERSION_STRING)
libvlcjni_repo=$(lock_value LIBVLCJNI_REPO)
libvlcjni_commit=$(lock_value LIBVLCJNI_COMMIT)

if [ -z "$ndk" ]; then
    echo "no NDK: pass --ndk or set ANDROID_NDK" >&2
    exit 1
fi
if [ ! -f "$ndk/source.properties" ]; then
    echo "$ndk does not look like an Android NDK: no source.properties" >&2
    exit 1
fi
if [ ! -d "$ndk/toolchains/llvm/prebuilt/linux-x86_64" ]; then
    echo "$ndk has no linux-x86_64 prebuilt toolchain." >&2
    echo "The NDK ships one toolchain per host, and the Windows download has only windows-x86_64." >&2
    exit 1
fi

ndk_revision=$(sed -n 's/^Pkg.Revision *= *//p' "$ndk/source.properties" | head -1)
ndk_major=$(echo "$ndk_revision" | cut -d. -f1)
# Upstream exits on anything but 28 or 29, three minutes into a configure run.
if [ "$ndk_major" != 28 ] && [ "$ndk_major" != 29 ]; then
    echo "the Android runtime needs NDK 28 or 29, got $ndk_revision" >&2
    exit 1
fi

src_dir="$work_dir/libvlcjni"
vlc_dir="$src_dir/vlc"
artifact="$out_dir/vlc-$platform.tar.gz"

echo "engine:      $vlc_repo @ $vlc_commit"
echo "buildsystem: $libvlcjni_repo @ $libvlcjni_commit"
echo "ndk:         $ndk_revision"
echo "abi:         $abi ($target_tuple)"
echo "work:        $work_dir"
echo "artifact:    $artifact"

if [ "$dry_run" = 1 ]; then
    exit 0
fi

fetch_commit() {
    # A shallow fetch of one commit is cheap when the server will serve it, which
    # it only does when that commit happens to be a ref tip. For an older pin --
    # the normal case for a locked revision -- it is refused outright ("couldn't
    # find remote ref"), and the fallback is a full fetch, which is roughly a
    # gigabyte of VLC. The desktop build arrived at the same two-step and says so
    # in build/vlc/build.ps1.
    repo=$1
    commit=$2
    dir=$3

    if [ ! -d "$dir/.git" ]; then
        mkdir -p "$dir"
        git -C "$dir" init -q
        git -C "$dir" remote add origin "$repo"
    fi

    if ! git -C "$dir" fetch -q --depth 1 origin "$commit" 2>/dev/null; then
        echo "  the server does not serve $commit shallowly; fetching the full history"
        git -C "$dir" fetch -q --tags origin
    fi

    git -C "$dir" checkout -q --force "$commit"
    git -C "$dir" submodule update --init --recursive --depth 1 2>/dev/null || true
}

mkdir -p "$work_dir"

echo "== fetching the build system =="
fetch_commit "$libvlcjni_repo" "$libvlcjni_commit" "$src_dir"

echo "== fetching the engine =="
fetch_commit "$vlc_repo" "$vlc_commit" "$vlc_dir"

# The lock may hold an abbreviation, so the resolved commit has to agree with it;
# otherwise the artifact would name a revision it was not built from.
resolved_commit=$(git -C "$vlc_dir" rev-parse HEAD)
case $resolved_commit in
    "$vlc_commit"*) ;;
    *) echo "vlc.lock pins $vlc_commit but $resolved_commit was checked out" >&2; exit 1 ;;
esac
describe=$(git -C "$vlc_dir" describe --always --tags 2>/dev/null || echo "$resolved_commit")

source_api_version=$(sed -n 's/^#define VLC_API_VERSION_STRING *"\(.*\)"/\1/p' \
    "$vlc_dir/include/vlc_plugin.h" | head -1)
if [ "$source_api_version" != "$locked_api_version" ]; then
    echo "the checked-out engine reports API $source_api_version, vlc.lock says $locked_api_version" >&2
    exit 1
fi

echo "== keeping the modules this extension needs =="
# Upstream's module blacklist removes two things this extension cannot work
# without. Both entries are regular expressions matched against lib*_plugin.a
# names, which is why neither reads like one:
#
#   .dummy               matches libwdummy_plugin.a, the dummy *window* module.
#                        LibVLC asks for it whenever a display needs no real
#                        window, which is exactly the case for the vmem output
#                        this extension uses to produce a Godot texture.
#   access_(bd|shm|imem) matches libaccess_imem_plugin.a, the module behind the
#                        imem:// MRL. That is how VLCMedia hands LibVLC an
#                        in-memory buffer for media inside res://.
#
# Without the first, video output creation fails ("no vout window modules matched
# with name dummy") and no display module is ever chosen, so the software
# callbacks never run: sound, no picture. Without the second, nothing under
# res:// plays at all.
#
# Verified here and asserted again on the built library, because either failure
# produces a runtime that builds cleanly and does nothing visible on a device.
blacklist_script="$src_dir/buildsystem/build-libvlc.sh"
sed -i -e '/^ *\.dummy$/d' -e 's/access_(bd|shm|imem)/access_(bd|shm)/' "$blacklist_script"
if grep -q -e '^ *\.dummy$' -e 'access_(bd|shm|imem)' "$blacklist_script"; then
    echo "$blacklist_script still excludes modules this extension needs" >&2
    exit 1
fi

echo "== building contribs, engine and libvlc.so =="
cd "$src_dir"
export ANDROID_NDK="$ndk"
export MAKEFLAGS="-j$jobs"
# --license l is this buildsystem's "LGPLv3 + ad-clauses" mode; its other modes
# are a: LGPLv2.1 + ad-clauses and g: GPL (the default). The desktop runtimes
# build the same combination, and the two lines have to agree -- a runtime that
# is LGPLv2.1 on one platform and LGPLv3 on another is a licence statement
# nobody can make.
#
# l is also what buys TLS on Android: contrib builds GnuTLS only when
# version-3 (L)GPL code is allowed, because its crypto backend (nettle, then
# gmp) is LGPLv3+/GPLv2+, and without GnuTLS no plugin provides LibVLC's
# "tls client" capability, so https:// and everything else over TLS fails to
# open. The desktop runtimes reach the same tier by passing --disable-gpl and
# --enable-ad-clauses and *not* passing --disable-gnuv3; build/vlc/build.ps1
# carries the long version of this reasoning, and README.md's "Licensing"
# section records what it means for whoever bundles these runtimes.
set -- -a "$abi" --release --license l --no-jni
if [ "$prebuilt_contribs" = 1 ]; then
    set -- "$@" --with-prebuilt-contribs
fi
./buildsystem/compile-libvlc.sh "$@"

built_lib="$src_dir/libvlc/jni/libs/$abi/libvlc.so"
if [ ! -f "$built_lib" ]; then
    echo "the build reported success but $built_lib is not there" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
echo "== checking what was built =="
# ---------------------------------------------------------------------------
toolchain="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"
fail=0

# The two modules the extension's Android path cannot work without. Both are one
# grep away, and one of them was missing from the runtime that motivated writing
# this script.
if ! "$toolchain/llvm-strings" -a "$built_lib" | grep -qx 'wdummy'; then
    echo "FAIL: the dummy vout window module is absent; video output cannot be created" >&2
    fail=1
fi
if ! "$toolchain/llvm-strings" -a "$built_lib" | grep -qx 'imem_access'; then
    echo "FAIL: the imem access module is absent; media inside res:// cannot be played" >&2
    fail=1
fi

# The licence, asserted rather than announced. VideoLAN's Android builds default
# to --license g, which links GPL-licensed encoders and access modules into
# libvlc.so -- x264 among them, which the runtime they publish carries. This
# repository ships runtimes a commercial game can bundle, so a GPL module
# reaching the artifact has to fail the build rather than a licence review.
#
# The check keys on strings only VLC's x264 module produces, not on the module's
# name. A bare "x264" also occurs inside libavcodec's own tables, so matching it
# fails builds that are in fact clean -- and a licence check that cries wolf is
# one somebody eventually deletes.
if "$toolchain/llvm-strings" -a "$built_lib" |
    grep -qE 'sout-x264-|H\.264/MPEG-4 Part 10/AVC encoder \(x264\)'; then
    echo "FAIL: VLC's x264 encoder module is in the runtime; --license l was not honoured" >&2
    fail=1
fi

# Android 15 and later refuse a native library whose segments are not 16 KB
# aligned, and the page size belongs to the device, not to a build option.
alignment=$("$toolchain/llvm-readelf" -l "$built_lib" | awk '/ LOAD /{print $NF}' | sort -u)
if [ "$alignment" != "0x4000" ]; then
    echo "FAIL: LOAD alignment is '$alignment', expected 0x4000 (16 KB)" >&2
    fail=1
fi

# The desktop runtimes shipped DWARF until it was measured, and it was most of
# what users downloaded: 666 MB of the Linux tree's 798 MB of shared objects and
# 827 MB of the Windows tree's 1026 MB. ndk-build's release mode strips, so this
# runtime never carried any -- and this is what would notice if a future NDK
# stopped doing it, rather than the addon quietly growing by a factor of five.
if "$toolchain/llvm-readelf" -W -S "$built_lib" |
    grep -qE '^[[:space:]]*(\[[[:space:]]*[0-9]+\][[:space:]]+)?\.z?debug'; then
    echo "FAIL: the runtime carries debug sections; the shipped runtime must be stripped" >&2
    fail=1
fi

if [ "$fail" != 0 ]; then
    exit 1
fi

# ---------------------------------------------------------------------------
echo "== staging =="
# ---------------------------------------------------------------------------
stage="$work_dir/stage-$platform"
rm -rf "$stage"
mkdir -p "$stage/lib"
cp "$built_lib" "$stage/lib/libvlc.so"
# The engine's headers, which build.rs reads through bindgen for every target.
cp -R "$vlc_dir/include" "$stage/include"

# The keys the desktop artifacts carry, so that one provenance check can read
# either, plus the inputs only the Android runtime has. vlc_core_abi_major is
# deliberately absent: a monolithic runtime has no separate core.
cat > "$stage/build-info.txt" <<EOF
platform=$platform
abi=$abi
vlc_repo=$vlc_repo
vlc_commit=$resolved_commit
vlc_commit_requested=$vlc_commit
vlc_describe=$describe
vlc_api_version_string=$source_api_version
libvlcjni_repo=$libvlcjni_repo
libvlcjni_commit=$libvlcjni_commit
license=a
gpl_free=yes
ndk_revision=$ndk_revision
EOF

mkdir -p "$out_dir"
rm -f "$artifact"
# Members at the root, exactly like the desktop artifacts: the staging script
# names them individually rather than extracting whatever the tarball holds.
tar -czf "$artifact" -C "$stage" include lib build-info.txt

echo ''
echo "build: artifact $artifact"
echo "build: size     $(du -h "$artifact" | cut -f1)"
echo "build: vlc      $describe (API $source_api_version)"
