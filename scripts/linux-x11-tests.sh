#!/bin/sh
# Runs the X11 backends' ignored tests against Xvfb, inside the Linux
# container (scripts/linux-container/README.md):
#
#   scripts/linux-container.sh run sh scripts/linux-x11-tests.sh [test filter]
#
# The tests share the server, so they run one at a time. Xvfb's screen is
# two monitors wide; the tests lay out their own monitors on it.
set -eu

cd "$(dirname "$0")/.."
filter="${1:-linux::x11}"

# Xvfb picks a display number nobody uses and says which (-displayfd):
# other test runs in the same container may be starting theirs.
# -noreset: between two tests nobody is connected, and the server would
# start over then, turning away the next test meanwhile.
fifo=$(mktemp -u /tmp/xvfb-display.XXXXXX)
log=$(mktemp /tmp/xvfb-log.XXXXXX)
mkfifo "$fifo"
Xvfb -displayfd 3 -screen 0 3840x1080x24 -nolisten tcp -noreset 3>"$fifo" >"$log" 2>&1 &
xvfb=$!
trap 'kill $xvfb 2>/dev/null || true; rm -f "$fifo" "$log"' EXIT INT TERM
# Xvfb that says nothing in 10 s did not start.
n=$(timeout 10 head -n 1 "$fifo" || true)
if [ -z "$n" ]; then
    echo "Xvfb did not start:" >&2
    cat "$log" >&2
    exit 1
fi

DISPLAY=":$n" cargo test -p iw-platform --lib -- --ignored --test-threads=1 "$filter"
