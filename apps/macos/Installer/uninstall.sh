#!/bin/sh
# Removes InputWeave from this Mac: the background service stops (it tells
# the other computers first, so nothing stays pressed or captured) and is
# taken out of Login Items, then the app, the login window's agent, the
# permissions macOS granted it, and every user's logs and preferences are
# deleted. Pairings, the
# arrangement and this Mac's identity are kept unless --purge is given.
# Received files are never touched.
#
#   sudo sh /Applications/InputWeave.app/Contents/Resources/uninstall.sh [--purge]
#
# The app runs it for Uninstall InputWeave… (passing --app and --caller, so
# the copy that asked is left to quit by itself), and the installer package
# for its removal choices. --service-only stops and unregisters the
# service and keeps everything else; the package does that before
# replacing an app signed by another team. --migrate does the same and
# then removes what macOS keeps under the identifiers of v0.1.0 (the
# permissions, the login window's agent, the package receipts); the
# package does that before replacing such an app, and the new app copies
# the user's settings itself. --stop-old only stops a v0.1.0 background
# service still running in any session, whatever app is installed; the
# package does that on every install.
set -u
# The app may read this script's output through a pipe; if it goes away
# halfway, finish the job anyway.
trap '' PIPE

app=/Applications/InputWeave.app
caller=""
purge=false
service_only=false
migrate=false
stop_old=false
while [ $# -gt 0 ]; do
  case "$1" in
    --purge) purge=true ;;
    --service-only) service_only=true ;;
    --migrate) service_only=true; migrate=true ;;
    --stop-old) stop_old=true ;;
    --app) app=${2:?--app needs a path}; shift ;;
    --caller) caller=${2:?--caller needs a process ID}; shift ;;
    *) echo "usage: $0 [--purge] [--service-only | --migrate | --stop-old] [--app PATH] [--caller PID]" >&2; exit 2 ;;
  esac
  shift
done
[ "$(id -u)" -eq 0 ] || { echo "$0: must run as root (sudo)" >&2; exit 1; }

bundle_id=cc.ctsite.inputweave
label=cc.ctsite.inputweave.agent
login_label=cc.ctsite.inputweave.loginwindow
# The identifiers of v0.1.0 and the builds before 2026-10-27, whose
# leftovers an update (--migrate) and an uninstall remove too.
old_bundle_id=com.inputweave.InputWeave
old_label=com.inputweave.InputWeave.Agent
old_login_label=com.inputweave.InputWeave.LoginWindow
say() { echo "InputWeave: $*"; }

# Waits up to $2 seconds for process $1 to exit.
wait_exit() {
  n=0
  while kill -0 "$1" 2>/dev/null && [ $n -lt $(($2 * 4)) ]; do
    sleep 0.25
    n=$((n + 1))
  done
}

# Stops the background service under each label given, in every user's
# session (each has a loginwindow). On SIGTERM the service tells the other
# computers it is leaving.
stop_services() {
  for uid in $(ps -axo uid=,comm= | awk '$2 ~ /\/loginwindow$/ { print $1 }' | sort -u); do
    for service in "$@"; do
      pid=$(launchctl print "gui/$uid/$service" 2>/dev/null | awk '$1 == "pid" { print $3 }')
      launchctl bootout "gui/$uid/$service" 2>/dev/null || continue
      if [ -n "$pid" ]; then
        wait_exit "$pid" 10
        kill -KILL "$pid" 2>/dev/null
      fi
      say "stopped the background service $service of user $uid"
    done
  done
}

if $stop_old; then
  stop_services "$old_label"
  exit 0
fi

# Everything below runs as root and ends in rm -rf "$app": only ever touch
# an app bundle that is InputWeave.
case "$app" in /*.app) ;; *) echo "$0: --app must be the absolute path of an .app" >&2; exit 2 ;; esac
if [ -e "$app" ]; then
  app_id=$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$app/Contents/Info.plist" 2>/dev/null)
  case "$app_id" in
    "$bundle_id" | "$old_bundle_id") ;;
    *) echo "$0: $app is not InputWeave" >&2; exit 2 ;;
  esac
fi

# The user at the screen; nobody at the login window.
user=$(stat -f %Su /dev/console)
case "$user" in root | loginwindow | _mbsetupuser) user="" ;; esac
as_user() { launchctl asuser "$(id -u "$user")" sudo -u "$user" "$@"; }

# Quit every copy of the app except the one that asked; it quits once this
# returns.
for pid in $(pgrep -x InputWeave); do
  [ "$pid" = "$caller" ] || kill "$pid"
done

# Only the app can take its service out of Login Items, as the user who
# registered it, and only while its bundle is still there. Unregistering
# stops the service. The app has 20 seconds.
executable=$(/usr/libexec/PlistBuddy -c 'Print CFBundleExecutable' "$app/Contents/Info.plist" 2>/dev/null)
unregistered=false
if [ -n "$user" ] && [ -n "$executable" ]; then
  as_user "$app/Contents/MacOS/$executable" --unregister-service &
  helper=$!
  wait_exit "$helper" 20
  if kill -0 "$helper" 2>/dev/null; then
    kill "$helper"
  elif wait "$helper"; then
    unregistered=true
    say "removed the background service from Login Items"
  fi
fi
if ! $unregistered && [ -d "$app" ]; then
  say "could not remove the background service from Login Items; turn it off in System Settings › General › Login Items & Extensions"
fi

# Whatever is still running stops here.
stop_services "$label" "$old_label"

# The permissions macOS granted the service under label $1 (Accessibility,
# Input Monitoring), so it does not stay listed in System Settings.
reset_permissions() {
  tccutil reset All "$1" >/dev/null 2>&1
  [ -n "$user" ] && as_user tccutil reset All "$1" >/dev/null 2>&1
}

# The login window's agent under label $1, which launchd would otherwise
# start at the next login window. It only runs there, so normally not now.
remove_login_agent() {
  if [ -e "/Library/LaunchAgents/$1.plist" ]; then
    rm -f "/Library/LaunchAgents/$1.plist" && say "removed the login window's agent"
  fi
  pkgutil --forget "$1" >/dev/null 2>&1
}

# What v0.1.0 left under its identifiers: the new app's agent has other
# permissions to ask for, and its own login window's agent.
# The receipts of the app and of the payload-free removal packages under
# bundle identifier $1.
forget_receipts() {
  for receipt in "$1" "$1.remove" "$1.purge"; do
    pkgutil --forget "$receipt" >/dev/null 2>&1
  done
}

if $migrate; then
  reset_permissions "$old_label"
  remove_login_agent "$old_login_label"
  forget_receipts "$old_bundle_id"
  say "removed what the earlier identifiers left"
fi
$service_only && exit 0

reset_permissions "$label"
reset_permissions "$old_label"
remove_login_agent "$login_label"
remove_login_agent "$old_login_label"
pkill -f "InputWeave Agent.app/Contents/MacOS/inputweave agent --login-window" 2>/dev/null
rm -rf /Library/Logs/InputWeave

rm -rf "$app" && say "deleted $app"
forget_receipts "$bundle_id"
forget_receipts "$old_bundle_id"

# Every user's logs, preferences, window state and caches. cfprefsd holds
# the logged-in user's preferences, so those are deleted through it.
for id in "$bundle_id" "$old_bundle_id"; do
  [ -n "$user" ] && as_user defaults delete "$id" >/dev/null 2>&1
done
dscl . -list /Users NFSHomeDirectory | while read -r _ home; do
  case "$home" in /Users/*) ;; *) continue ;; esac
  support="$home/Library/Application Support/InputWeave"
  rm -rf "$home/Library/Logs/InputWeave"
  for id in "$bundle_id" "$old_bundle_id"; do
    rm -rf "$home/Library/Preferences/$id.plist" \
      "$home/Library/Saved Application State/$id.savedState" \
      "$home/Library/Caches/$id" \
      "$home/Library/HTTPStorages/$id"
  done
  rm -f "$support/agent.sock"
  [ -d "$support" ] || continue
  if $purge; then
    rm -rf "$support" && say "deleted the pairings and identity in $support"
  else
    say "kept the pairings and identity in $support"
  fi
done
say "uninstalled; received files were kept"
