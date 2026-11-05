#!/bin/sh
# Checks the tray icon over D-Bus against the agent with its mock
# backends: the StatusNotifierItem's icon, pixmaps and tooltip, the
# dbusmenu layout, and that clicking Pause Sharing pauses sharing in the
# agent and changes the menu and the icon. Runs in the amd64 Linux
# container, on Xvfb and a private session bus:
#
#   scripts/linux-container.sh run 'sh apps/linux/scripts/check-tray.sh <agent> <app>'
#
# <agent> is the root workspace's inputweave, <app> the inputweave-app to
# check (both debug builds will do).
set -u
agent_bin=$1
app_bin=$2
dir=$(mktemp -d)
cat > "$dir/config.toml" <<CONF
[device]
id = "tray-test"
name = "Tray test"
[network]
listen = "127.0.0.1:0"
discovery = false
allow_pairing = false
[paths]
identity_dir = "$dir/identity"
ipc_socket = "$dir/agent.sock"
devices_file = "$dir/devices.toml"
[backend]
kind = "mock"
displays = [{ id = "DP-1", x = 0, y = 0, width = 1920, height = 1080, scale = 1.0, primary = true }]
CONF
HOME=$dir "$agent_bin" agent --config "$dir/config.toml" > "$dir/agent.log" 2>&1 &
agent=$!
Xvfb :74 -screen 0 1280x800x24 -nolisten tcp > /dev/null 2>&1 &
xvfb=$!
cleanup() {
    kill "$agent" "$xvfb" 2>/dev/null
    rm -rf "$dir"
}
trap cleanup EXIT
sleep 2
export DISPLAY=:74 GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=none
export XDG_RUNTIME_DIR="$dir/run" LANG=C.UTF-8
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
export AGENT_BIN="$agent_bin" APP_BIN="$app_bin" DIR="$dir"
dbus-run-session -- sh -c '
    fail=0
    check() {
        if printf "%s" "$2" | grep -q "$3"; then
            echo "ok: $1"
        else
            echo "FAIL: $1 (wanted $3)"; printf "%s\n" "$2" | head -5; fail=1
        fi
    }
    "$APP_BIN" --background --socket "$DIR/agent.sock" > "$DIR/app.log" 2>&1 &
    app=$!
    name=org.kde.StatusNotifierItem-$app-1
    item() {
        gdbus call --session --dest $name --object-path /StatusNotifierItem \
            --method org.freedesktop.DBus.Properties.Get org.kde.StatusNotifierItem "$1" 2>&1
    }
    layout() {
        gdbus call --session --dest $name --object-path /MenuBar \
            --method com.canonical.dbusmenu.GetLayout -- 0 -1 "@as []" 2>&1
    }
    for i in $(seq 1 60); do
        layout | grep -q "Sharing keyboard and mouse" && break
        sleep 0.5
    done
    check "icon name" "$(item IconName)" "cc.ctsite.inputweave-tray-normal-symbolic"
    check "icon theme path" "$(item IconThemePath)" "$XDG_RUNTIME_DIR/inputweave/app/icons"
    check "pixmaps of 22 and 44 pixels" "$(item IconPixmap | grep -o "(22, 22\|(44, 44" | tr "\n" " ")" "(22, 22 (44, 44"
    check "tooltip" "$(item ToolTip)" "InputWeave, sharing"
    check "status line in the menu" "$(layout)" "Sharing keyboard and mouse"
    check "Pause Sharing in the menu" "$(layout)" "Pause Sharing"
    check "Quit in the menu" "$(layout)" "Quit InputWeave"
    check "icons written" "$(ls "$XDG_RUNTIME_DIR/inputweave/app/icons/hicolor/scalable/status")" "tray-paused-symbolic.svg"
    gdbus call --session --dest $name --object-path /MenuBar \
        --method com.canonical.dbusmenu.Event 5 clicked "<0>" 0 > /dev/null
    sleep 2
    check "the agent paused sharing" "$("$AGENT_BIN" ctl --socket "$DIR/agent.sock" status)" "sharing: *paused"
    check "the menu offers Resume Sharing" "$(layout)" "Resume Sharing"
    check "the icon shows paused" "$(item IconName)" "tray-paused-symbolic"
    kill $app
    exit $fail
'
