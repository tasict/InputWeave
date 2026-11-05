# The Linux build and test environments:
# Ubuntu 24.04, the oldest release the packages target. Two stages, since
# the GTK and libadwaita development packages of amd64 and arm64 cannot
# be installed together on 24.04:
# - native: builds and tests for amd64, with the headless X server and
#   compositor the ignored tests run against;
# - arm64: cross builds for arm64 against arm64 GTK and libadwaita.
FROM ubuntu:24.04 AS base

ENV DEBIAN_FRONTEND=noninteractive

# amd64 from the archive, arm64 from the ports mirror, for the arm64
# GTK and libadwaita the app links against.
RUN sed -i '/^Signed-By:/i Architectures: amd64' /etc/apt/sources.list.d/ubuntu.sources \
 && printf '%s\n' \
    'Types: deb' \
    'URIs: http://ports.ubuntu.com/ubuntu-ports' \
    'Suites: noble noble-updates noble-security' \
    'Components: main universe' \
    'Architectures: arm64' \
    'Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg' \
    > /etc/apt/sources.list.d/ubuntu-ports.sources \
 && dpkg --add-architecture arm64

# systemd comes with sway (libpam-systemd) and gives systemd-analyze for
# checking the user unit; nothing here runs it. On a Linux 5.10 host
# kernel systemd-sysusers fails (fchmod through /proc on an O_PATH descriptor),
# and with it the postinst of every package that creates a system user, so
# it is diverted to a stand-in using shadow's tools. systemd's own
# postinst still fails for the same reason elsewhere and is replaced with
# a no-op.
COPY sysusers.sh /usr/local/sbin/sysusers.sh
RUN chmod 755 /usr/local/sbin/sysusers.sh \
 && dpkg-divert --local --rename --add /usr/bin/systemd-sysusers \
 && ln -s /usr/local/sbin/sysusers.sh /usr/bin/systemd-sysusers \
 && apt-get update \
 && (apt-get install -y --no-install-recommends systemd dbus-x11 || true) \
 && printf '#!/bin/sh\nexit 0\n' > /var/lib/dpkg/info/systemd.postinst \
 && dpkg --configure -a \
 && rm -rf /var/lib/apt/lists/*

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config curl ca-certificates git file \
    dpkg-dev fakeroot lintian \
    gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross \
    qemu-user \
 && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
      --component rustfmt,clippy --target aarch64-unknown-linux-gnu,x86_64-unknown-linux-musl

# Cross linking for arm64; the agent needs no arm64 libraries but libc.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++

# Two jobs, for a small build host that runs other services too.
ENV CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/target

WORKDIR /src

FROM base AS native
RUN apt-get update && apt-get install -y --no-install-recommends \
    libgtk-4-dev libadwaita-1-dev \
    xvfb xauth x11-utils x11-xserver-utils xdotool xclip \
    sway wl-clipboard fonts-noto-cjk librsvg2-common \
    musl-tools \
 && rm -rf /var/lib/apt/lists/*

FROM base AS arm64
# Some .gir files differ between the two architectures; Rust builds do
# not read them, so the arm64 ones may overwrite the amd64 ones.
RUN apt-get update && apt-get install -y --no-install-recommends \
    -o Dpkg::Options::=--force-overwrite \
    libgtk-4-dev:arm64 libadwaita-1-dev:arm64 \
 && rm -rf /var/lib/apt/lists/*
ENV PKG_CONFIG_PATH_aarch64_unknown_linux_gnu=/usr/lib/aarch64-linux-gnu/pkgconfig \
    PKG_CONFIG_ALLOW_CROSS=1

