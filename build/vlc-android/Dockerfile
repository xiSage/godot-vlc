# syntax=docker/dockerfile:1
# Pinned build environment for the Android LibVLC runtime.
#
# Toolchain only, like build/vlc/Dockerfile: the engine and the Android build
# system are fetched at the revisions in vlc.lock when the container runs, so
# moving a pin does not rebuild the image. What is baked in is the NDK, because
# the NDK is a toolchain rather than a source -- it is the analogue of the
# mingw-w64 package in the desktop image.
#
# It is the *Linux* NDK on purpose. The NDK ships one prebuilt toolchain per
# host, and the Windows download contains only windows-x86_64, which a Linux
# container cannot execute.
ARG BASE_IMAGE=ubuntu:22.04

FROM ${BASE_IMAGE}

# A non-UTF-8 locale breaks parts of the VLC build.
ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8

# The same archive configuration as build/vlc/Dockerfile, for the reasons given
# there in full: Ubuntu's live archive, no snapshot pinning, and universe
# because several build dependencies live in it.
RUN set -eu; \
    printf 'deb http://archive.ubuntu.com/ubuntu/ jammy main restricted universe multiverse\n' > /etc/apt/sources.list; \
    printf 'deb http://archive.ubuntu.com/ubuntu/ jammy-updates main restricted universe multiverse\n' > /etc/apt/sources.list.d/updates.list; \
    printf 'deb http://security.ubuntu.com/ubuntu/ jammy-security main restricted universe multiverse\n' > /etc/apt/sources.list.d/security.list; \
    printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99untrusted-clock

# autotools/gettext/autopoint/gperf/flex/bison: required by VLC's ./bootstrap
#   and by contrib's own bootstrap, which the Android build runs as well.
# cmake/meson/ninja: build systems of the contrib packages.
# python3-venv: contrib builds a Python virtualenv for packages such as glad;
#   without it the venv creation fails with "ensurepip is not available".
# ragel/protobuf-compiler/subversion: inputs of individual contrib packages on
#   Android -- upstream lists all three in its own instructions.
# nasm/yasm: assembly optimisations in codec modules.
# unzip/zip/xz-utils/tar/wget/curl: contrib fetches and unpacks its sources.
# file: configure probes with it.
#
# No Java and no Gradle: the extension is a GDExtension, so libvlcjni is built
# with --no-jni and the Java bindings are never produced. That is also why the
# image carries none of the Android SDK -- ndk-build needs the NDK and make,
# nothing else.
#
# The apt caches are mounted rather than written into the image, so retries and
# later rebuilds reuse what has already been downloaded.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    set -eu; \
    apt-get update; \
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
              make \
              meson \
              nasm \
              ninja-build \
              patch \
              pkg-config \
              protobuf-compiler \
              python3 \
              python3-venv \
              ragel \
              subversion \
              tar \
              unzip \
              wget \
              xz-utils \
              yasm \
              zip; then \
            installed=yes; \
            break; \
        fi; \
        echo "apt-get install attempt $attempt failed; retrying with what was already downloaded"; \
        sleep 20; \
    done; \
    [ "$installed" = yes ]

# The NDK is pinned by URL and asserted on its revision, so the image cannot
# silently drift to a release the build refuses: VLC's Android build system
# accepts r28 and r29 and exits on anything else.
#
# Pinned by r-number in the URL rather than by digest because Google publishes
# no digest for these archives; the assertion below is what makes the pin real.
ARG NDK_URL=https://dl.google.com/android/repository/android-ndk-r28b-linux.zip
ARG NDK_REVISION=28.1.13356709
RUN set -eu; \
    curl -fsSL --retry 5 --retry-all-errors -o /tmp/ndk.zip "$NDK_URL"; \
    unzip -q /tmp/ndk.zip -d /opt; \
    mv /opt/android-ndk-r28b /opt/android-ndk; \
    rm -f /tmp/ndk.zip; \
    grep -q "^Pkg.Revision = ${NDK_REVISION}" /opt/android-ndk/source.properties

ENV ANDROID_NDK=/opt/android-ndk

# The build fetches thousands of small files and compiles a hundred libraries;
# a named volume keeps that out of the container's writable layer when a caller
# wants to reuse it. Left unset, the script uses /tmp.
ENV VLC_WORKDIR=

# The context is build/ rather than this directory, because the recipe needs both
# this build script and vlc.lock, and vlc.lock is the desktop build's pin too.
# Copying it here would make a second place to keep the same revision. build/ is
# a few megabytes -- the runtime trees are not in it.
#
#     docker build -f build/vlc-android/Dockerfile -t godot-vlc/vlc-android-build build
WORKDIR /vlcbuild
COPY vlc/vlc.lock ./vlc.lock
COPY vlc-android/build.sh ./build.sh

# build.sh <abi> <output-dir>
#   abi: arm64-v8a | armeabi-v7a | x86_64
#   output-dir: receives vlc-android-<abi>.tar.gz
ENTRYPOINT ["/bin/sh", "/vlcbuild/build.sh"]
CMD ["arm64-v8a", "/out"]
