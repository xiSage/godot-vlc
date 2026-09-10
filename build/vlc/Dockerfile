# syntax=docker/dockerfile:1
# Pinned build environment for LibVLC.
#
# This image provides the toolchain only. The VLC sources are fetched at the
# revision recorded in vlc.lock at run time, so bumping the pin does not require
# rebuilding the image.
#
# Everything this repository writes is PowerShell, so the image carries pwsh as
# well as the autotools toolchain VLC's own build system needs. VLC's bootstrap,
# contrib/bootstrap and configure are shell scripts; that is VLC's build system
# rather than ours, and they are invoked as such.
#
# ---------------------------------------------------------------------------
# Why ubuntu:22.04
# ---------------------------------------------------------------------------
# The base decides the glibc floor of everything built in it, and the addon's
# shared library is dlopen()ed into the Godot process, so its requirement must
# not exceed the requirement of the official Godot Linux binaries. The extension
# itself is compiled on ubuntu-22.04 in CI, so matching that here means the
# runtime and the extension share one floor, 2.35, and neither raises it.
#
# The obvious alternative was debian:11 (glibc 2.31, a lower floor). It does not
# survive contact with a 2026 VLC: its autoconf is 2.69 while libtheora requires
# 2.71, its meson is 0.56 while harfbuzz requires 0.60, its gcc is 10, and its
# cmake is 3.18. Back-porting each of those into an old base is not a frozen
# toolchain, it is a pile of exceptions. Ubuntu 22.04 has autoconf 2.71, gcc 11,
# cmake 3.22 and libtool 2.4.7, and its floor is already the one in use.
#
# scripts/check_glibc_floor.ps1 measures the whole assembled runtime against the
# bound in glibc-baseline.txt, so a base that raised the floor could not go
# unnoticed.
#
# ---------------------------------------------------------------------------
# The Windows runtime is built from a different base: ubuntu:24.04
# ---------------------------------------------------------------------------
# Every word above is about the *Linux* artifact, and none of it applies to the
# Windows one. A Windows runtime is PE: it has no glibc dependency, and the glibc
# of the container that cross-compiles it never reaches the artifact. What the
# container's mingw-w64 decides is only what cross-compiles at all.
#
# On 22.04 that is mingw-w64 9 (gcc 10), and a 2026 VLC does not build against
# it: ggml's Windows backend needs THREAD_POWER_THROTTLING_STATE, glslang uses
# std::once_flag without including <mutex>, and libaribcaption calls
# GetUserDefaultLocaleName. Each failure is a third-party library that would need
# a patch for a capability this extension cannot reach.
#
# On 24.04 it is mingw-w64 13 (gcc 13), which builds all three. So the base is a
# per-platform input, and the same Dockerfile serves both:
#
#     docker build -t godot-vlc/vlc-build build/vlc                      # linux
#     docker build --build-arg BASE_IMAGE=ubuntu:24.04 \
#                  -t godot-vlc/vlc-build-win build/vlc                 # windows
#
# The distribution-dependent package names this creates (libicu70 against
# libicu74, libssl3 against libssl3t64) are resolved at build time rather than
# named, which is what lets one Dockerfile build on both.
#
# The base is referenced by tag rather than by digest. Resolve a digest with:
#
#     docker buildx imagetools inspect ubuntu:22.04 --format '{{.Manifest.Digest}}'
#
# and add it to BASE_IMAGE once the pipeline is known to build at all; pinning a
# base that cannot build is not progress.
ARG BASE_IMAGE=ubuntu:22.04

FROM ${BASE_IMAGE}

# A non-UTF-8 locale breaks parts of the VLC build.
ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8

# The Ubuntu archive is used directly.
#
# Snapshot pinning was tried first, because Debian's live archive had to be
# abandoned in favour of snapshot.debian.org when it started failing with
# "Release file is expired". It does not carry over: snapshot.ubuntu.com serves
# HTTPS only, and apt cannot use an HTTPS source before ca-certificates is
# installed, so the source is silently ignored ("Ign:... InRelease") and the
# build hangs. Ubuntu's live archive does not publish the short-lived Release
# files that broke Debian's, so nothing is gained by fighting that.
#
# What reproducibility there is comes from the pins that matter: the VLC revision
# in vlc.lock, the patch series, and the meson and PowerShell versions below. The
# apt package set is not pinned, which is recorded in README.md.
#
# universe is listed because several build dependencies live there.
RUN set -eu; \
    printf 'deb http://archive.ubuntu.com/ubuntu/ jammy main restricted universe multiverse\n' > /etc/apt/sources.list; \
    printf 'deb http://archive.ubuntu.com/ubuntu/ jammy-updates main restricted universe multiverse\n' > /etc/apt/sources.list.d/updates.list; \
    printf 'deb http://security.ubuntu.com/ubuntu/ jammy-security main restricted universe multiverse\n' > /etc/apt/sources.list.d/security.list; \
    printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99untrusted-clock

# autotools/meson/ninja: VLC's build system (autotools is canonical; the meson
#   build is upstream-labelled experimental and cannot do static plugins).
# gettext/autopoint/gperf/flex/bison: required by ./bootstrap and the parsers.
# nasm/yasm: assembly optimisations in codec modules.
# cmake/meson/ninja: build systems of several contrib packages.
# patchelf: normalises RPATH so the runtime is relocatable (postprocess.ps1).
# binutils: readelf, which the runpath and DT_NEEDED assertions read.
# mingw-w64: cross toolchain for the win-x64 artifact. Upstream supports only
#   mingw-w64 >= 8.0; MSVC/clang-cl is explicitly unsupported by the VLC build.
# libicu/libssl/libgssapi-krb5/zlib: runtime dependencies of pwsh, which the
#   PowerShell tarball does not bundle. Their major versions move with the
#   distribution (libicu70 becomes libicu74, libssl3 becomes libssl3t64), so they
#   are resolved below instead of named, which is what lets the same Dockerfile
#   build on more than one base.
# python3-pip/python3-setuptools: the pinned meson below, plus the helper
#   scripts some contrib packages install with pip.
# python3-venv: contrib builds a Python virtualenv for packages such as glad;
#   without it the venv creation fails with "ensurepip is not available".
# libx11-dev and the X11/xcb development set: Vulkan-Loader's CMake requires the
#   `x11` pkg-config module for its Xlib WSI and then compiles against
#   X11/extensions/Xrandr.h, and VLC's own X11 and xcb video outputs want the
#   rest. contrib provides xcb itself, so these cover the parts that link the
#   system X11 stack. The alternative is `contrib/bootstrap
#   --disable-vulkan-loader`, an official switch that removes this whole chain
#   but applies to both platforms, so it would drop Vulkan on Windows too.
#
# The apt caches are mounted rather than written into the image, so retries and
# later rebuilds reuse what has already been downloaded. That is also why the
# lists are not deleted at the end: with a cache mount they cost nothing, and
# deleting them would force a full re-index on every retry.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    set -eu; \
    apt-get update; \
    resolve() { apt-cache search --names-only "$1" 2>/dev/null | awk '{print $1}' | sort -V | tail -1; }; \
    pwsh_deps=""; \
    for pattern in '^libicu[0-9]+$' '^libssl[0-9]+' '^libgssapi-krb5-2' '^zlib1g$'; do \
        pkg=`resolve "$pattern"`; \
        if [ -n "$pkg" ]; then pwsh_deps="$pwsh_deps $pkg"; fi; \
    done; \
    echo "pwsh runtime dependencies resolved to:$pwsh_deps"; \
    installed=no; \
    for attempt in 1 2 3 4 5; do \
        if apt-get install -y --no-install-recommends -o Acquire::Retries=5 \
              autoconf \
              automake \
              autopoint \
              binutils \
              bison \
              build-essential \
              ca-certificates \
              cmake \
              curl \
              file \
              flex \
              gettext \
              git \
              gperf \
              libtool \
              libtool-bin \
              libx11-dev \
              libxcb1-dev \
              libxcb-composite0-dev \
              libxcb-damage0-dev \
              libxcb-randr0-dev \
              libxcb-render0-dev \
              libxcb-shape0-dev \
              libxcb-shm0-dev \
              libxcb-xfixes0-dev \
              libxcb-xkb-dev \
              libxcomposite-dev \
              libxcursor-dev \
              libxdamage-dev \
              libxext-dev \
              libxfixes-dev \
              libxinerama-dev \
              libxkbcommon-dev \
              libxkbcommon-x11-dev \
              libxrandr-dev \
              libxrender-dev \
              libxxf86vm-dev \
              make \
              meson \
              mingw-w64 \
              nasm \
              ninja-build \
              patch \
              patchelf \
              pkg-config \
              python3 \
              python3-pip \
              python3-setuptools \
              python3-venv \
              tar \
              unzip \
              wget \
              xz-utils \
              yasm \
              zip \
              $pwsh_deps; then \
            installed=yes; \
            break; \
        fi; \
        echo "apt-get install attempt $attempt failed; retrying with what was already downloaded"; \
        sleep 20; \
    done; \
    [ "$installed" = yes ]

# meson is pinned rather than left to the base image. Ubuntu 22.04 ships 0.61.2,
# which satisfies today's contrib packages, but contrib tracks upstream closely
# and the version a package demands moves. VLC provides no meson of its own:
# extras/tools builds cmake, nasm, libtool, tar and xz, but no meson.
#
# --break-system-packages because distributions that follow PEP 668 mark the
# system Python as externally managed and refuse the install outright
# ("error: externally-managed-environment"). That protection exists to keep a
# distribution's own Python consistent, and this is a throwaway build image whose
# only Python consumer is meson, so the flag is the intended escape here.
ARG MESON_VERSION=1.3.2
RUN pip3 install --no-cache-dir --break-system-packages "meson==${MESON_VERSION}" \
 && meson --version \
 && command -v meson

# PowerShell comes from a pinned tarball rather than from Microsoft's apt
# repository, so the version is an explicit input that changes only when this
# line does. Bump PWSH_VERSION deliberately.
ARG PWSH_VERSION=7.4.6
RUN curl -fsSL --retry 5 --retry-all-errors -o /tmp/powershell.tar.gz \
      "https://github.com/PowerShell/PowerShell/releases/download/v${PWSH_VERSION}/powershell-${PWSH_VERSION}-linux-x64.tar.gz" \
 && mkdir -p /opt/microsoft/powershell/7 \
 && tar -xzf /tmp/powershell.tar.gz -C /opt/microsoft/powershell/7 \
 && chmod +x /opt/microsoft/powershell/7/pwsh \
 && ln -sf /opt/microsoft/powershell/7/pwsh /usr/bin/pwsh \
 && rm -f /tmp/powershell.tar.gz \
 && pwsh -NoProfile -Command '$PSVersionTable.PSVersion.ToString()'

# A cross build needs an assembler named for the target it emits code for, and
# meson looks for exactly that: contrib's generated crossfile.meson lists no
# nasm, so meson falls back to the `<host>-nasm` convention. nasm satisfies the
# convention naturally -- it runs on the build machine and takes -f win64 -- so
# the native one is exposed under the mingw triplet's name.
#
# Without this, libass fails to configure with "Assembly was requested, but
# cannot be built", and every other meson-based package that assembles (dav1d,
# libvpx) would fail the same way. VLC's own Windows build gets these names from
# extras/tools, which this image does not need to bootstrap because the distro
# toolchain already meets the versions that script checks for.
RUN ln -sf /usr/bin/nasm /usr/local/bin/x86_64-w64-mingw32-nasm \
 && x86_64-w64-mingw32-nasm -v

# Upstream states the contrib tree builds "a hundred different libraries"; the
# official figure for a full contrib build is one to two hours. Keep the
# parallelism explicit so the CI job's runtime is predictable.
ENV VLC_BUILD_JOBS=

WORKDIR /vlcbuild
COPY vlc.lock ./
COPY build.ps1 postprocess.ps1 check-runpath.ps1 ./
COPY lib/ ./lib/
COPY patches/ ./patches/

# build.ps1 <platform> <output-dir>
#   platform: linux-x64 | win-x64
#   output-dir: receives vlc-<platform>.tar.gz
ENTRYPOINT ["pwsh", "-NoProfile", "-File", "/vlcbuild/build.ps1"]
CMD ["linux-x64", "/out"]
