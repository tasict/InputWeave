#!/bin/sh
# Draws the app's windows with sample data into PNG files, in English and
# Traditional Chinese, light and dark (inputweave-app --screenshot). Runs
# in the amd64 Linux container (scripts/linux-container/README.md), on Xvfb:
#
#   scripts/linux-container.sh run 'apps/linux/scripts/make-screenshots.sh [folder]'
#
# The app builds into /target/app (INPUTWEAVE_APP_TARGET), apart from the
# root workspace's target directory.
set -eu

here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-/target/app-screenshots}"
export CARGO_TARGET_DIR="${INPUTWEAVE_APP_TARGET:-/target/app}"
cd "$here"
cargo build -p inputweave-app
app="$CARGO_TARGET_DIR/debug/inputweave-app"
rm -rf "$out"
mkdir -p "$out"

display=:73
Xvfb "$display" -screen 0 1600x1000x24 -nolisten tcp >/dev/null 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true' EXIT
sleep 1

for scheme in light dark; do
    for language in en zh-TW; do
        dir="$out/$scheme"
        mkdir -p "$dir"
        if [ "$scheme" = dark ]; then
            colors=prefer-dark
        else
            colors=prefer-light
        fi
        DISPLAY="$display" GDK_BACKEND=x11 GSK_RENDERER=cairo \
            ADW_DEBUG_COLOR_SCHEME="$colors" LANG=C.UTF-8 \
            dbus-run-session -- "$app" --screenshot "$dir" --language "$language"
    done
done
find "$out" -name '*.png' | sort
