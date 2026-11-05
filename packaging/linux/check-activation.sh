#!/bin/sh
# Starts the app from the package's D-Bus service file, as the agent and
# DBusActivatable launchers do: on a private session bus that finds the
# service in the extracted package, with Xvfb for GTK. Runs in the native
# Linux container.
#
#   sh packaging/linux/check-activation.sh dist/inputweave_<version>_amd64.deb
set -u
deb=$(realpath "${1:?the package}")
t=$(mktemp -d)
xvfb=
trap '[ -n "$xvfb" ] && kill "$xvfb" 2>/dev/null; pkill -f "$t/usr/bin/inputweave-app"; rm -rf "$t"' EXIT
# dpkg-deb cannot set directory modes on some older host kernels (Linux
# 5.10); the files arrive.
dpkg-deb -x "$deb" "$t" 2>/dev/null
mkdir -p "$t/share/dbus-1/services" "$t/run"
sed "s|Exec=/usr/bin/|Exec=$t/usr/bin/|" "$t/usr/share/dbus-1/services/cc.ctsite.inputweave.service" \
    > "$t/share/dbus-1/services/cc.ctsite.inputweave.service"
# Xvfb picks a display nobody uses and says which.
mkfifo "$t/displayfd"
Xvfb -displayfd 3 -nolisten tcp 3>"$t/displayfd" >"$t/xvfb.log" 2>&1 &
xvfb=$!
display=$(timeout 10 head -n 1 "$t/displayfd") || {
    echo "Xvfb did not start:" >&2
    cat "$t/xvfb.log" >&2
    exit 1
}
export DISPLAY=":$display" GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=none XDG_RUNTIME_DIR="$t/run"
XDG_DATA_DIRS="$t/share:/usr/share" dbus-run-session -- sh -c '
    app="--dest cc.ctsite.inputweave --object-path /cc/ctsite/inputweave"
    gdbus call --session $app --method org.freedesktop.Application.ActivateAction background "[]" "{}" &&
        echo "background: activated"
    sleep 4
    gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner cc.ctsite.inputweave
    gdbus call --session $app --method org.freedesktop.Application.Activate "{}" && echo "window: activated"
    sleep 2
    echo "windows: $(xwininfo -root -tree | grep -c InputWeave)"
'
