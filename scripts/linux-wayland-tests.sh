#!/bin/sh
# Runs the Wayland backends' ignored tests inside the Linux container
# (scripts/linux-container/README.md):
#
#   scripts/linux-container.sh run sh scripts/linux-wayland-tests.sh [test filter]
#
# The tests start what they need themselves: a private dbus-daemon with a
# RemoteDesktop portal and a libei server of their own, and headless sway
# for the outputs. They set WAYLAND_DISPLAY for the process, so they run one
# at a time.
set -eu

cd "$(dirname "$0")/.."
filter="${1:-linux::wayland}"

cargo test -p iw-platform --lib -- --ignored --test-threads=1 "$filter"
