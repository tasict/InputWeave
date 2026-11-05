#!/bin/bash
# Xcode build phase: builds the Rust agent for the architectures being
# built and embeds it in the app as
#   Contents/Library/LoginItems/InputWeave Agent.app        the helper bundle
#   Contents/Library/LaunchAgents/<label>.plist             for SMAppService
# The helper is signed here; Xcode signs the app around it afterwards.
#
# With INPUTWEAVE_AGENT_PROFILE (a build setting package.sh passes), a
# provisioning profile granting com.apple.developer.hid.virtual.device,
# the helper carries the profile and is signed with that entitlement, so
# the agent can create the virtual keyboard and mouse the lock screen
# needs (agent-entitlements.sh checks the profile). Without it the helper
# has neither, and the agent posts events as before.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
repo="$(cd "$PROJECT_DIR/../.." && pwd)"
app="$TARGET_BUILD_DIR/$WRAPPER_NAME"
helper="$app/Contents/Library/LoginItems/InputWeave Agent.app"
label="cc.ctsite.inputweave.agent"
# Keep Rust's build products outside the repo, which may sit on a network
# share, and apart from the host's own target/.
export CARGO_TARGET_DIR="${INPUTWEAVE_CARGO_TARGET_DIR:-$DERIVED_FILE_DIR/cargo}"

# The app restarts a service whose version differs from its own, which is
# how a service left running by an update is replaced, so the two must
# agree. Sparkle tells which version is newer by CFBundleVersion, so it
# follows the version (build-number.sh) and only grows. scripts/set-version.sh
# sets all of them.
agent_version="$("$repo/scripts/workspace-version.sh")"
build="$("$PROJECT_DIR/scripts/build-number.sh" "$MARKETING_VERSION")"
if [ "$agent_version" != "$MARKETING_VERSION" ] || [ "$CURRENT_PROJECT_VERSION" != "$build" ]; then
    echo "error: Cargo.toml is $agent_version, MARKETING_VERSION $MARKETING_VERSION and CURRENT_PROJECT_VERSION $CURRENT_PROJECT_VERSION (for $MARKETING_VERSION it is $build); scripts/set-version.sh sets them together" >&2
    exit 1
fi

profile_flag=(--release)
profile_dir=release
if [ "$CONFIGURATION" = "Debug" ]; then
    profile_flag=()
    profile_dir=debug
fi

binaries=()
for arch in $ARCHS; do
    case "$arch" in
        arm64) triple=aarch64-apple-darwin ;;
        x86_64) triple=x86_64-apple-darwin ;;
        *) echo "error: unsupported architecture $arch" >&2; exit 1 ;;
    esac
    cargo build --manifest-path "$repo/Cargo.toml" -p inputweave --target "$triple" ${profile_flag[@]+"${profile_flag[@]}"}
    binaries+=("$CARGO_TARGET_DIR/$triple/$profile_dir/inputweave")
done

rm -rf "$helper"
mkdir -p "$helper/Contents/MacOS" "$helper/Contents/Resources" "$app/Contents/Library/LaunchAgents"
lipo -create "${binaries[@]}" -output "$helper/Contents/MacOS/inputweave"
# install, not cp: the source files can carry whatever mode the share
# they live on shows, and a plist only root can read leaves Launch
# Services unable to see LSUIElement, which puts the agent in the Dock.
install -m 0644 "$PROJECT_DIR/Agent/Info.plist" "$helper/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $MARKETING_VERSION" \
    -c "Set :CFBundleVersion $CURRENT_PROJECT_VERSION" "$helper/Contents/Info.plist"
# The Local Network prompt's text in each language the app speaks.
for lproj in "$PROJECT_DIR"/Agent/*.lproj; do
    mkdir -p "$helper/Contents/Resources/${lproj##*/}"
    install -m 0644 "$lproj/InfoPlist.strings" "$helper/Contents/Resources/${lproj##*/}/InfoPlist.strings"
done
plist="$app/Contents/Library/LaunchAgents/$label.plist"
install -m 0644 "$PROJECT_DIR/Agent/$label.plist" "$plist"

# The helper shows in System Settings with the app's icon.
iconset="$DERIVED_FILE_DIR/AppIcon.iconset"
rm -rf "$iconset"
cp -R "$PROJECT_DIR/InputWeave/Assets.xcassets/AppIcon.appiconset" "$iconset"
rm -f "$iconset/Contents.json"
iconutil -c icns "$iconset" -o "$helper/Contents/Resources/AppIcon.icns"
# Everything in the helper readable by everyone, whatever the sources were.
chmod -R u=rwX,go=rX "$helper" "$app/Contents/Library/LaunchAgents"

# Notarization needs a secure timestamp; ad-hoc signatures cannot have
# one, and debug builds skip the round trip to Apple.
identity="${EXPANDED_CODE_SIGN_IDENTITY:--}"
timestamp=--timestamp
if [ "$identity" = "-" ] || [ "$CONFIGURATION" = "Debug" ]; then
    timestamp=--timestamp=none
fi
# The same runtime as the app: on only with a real identity (project.yml).
runtime=()
if [ "${ENABLE_HARDENED_RUNTIME:-NO}" = "YES" ]; then
    runtime=(--options runtime)
fi
entitlements=()
profile="${INPUTWEAVE_AGENT_PROFILE:-}"
if [ -n "$profile" ]; then
    if [ "$identity" = "-" ]; then
        echo "error: INPUTWEAVE_AGENT_PROFILE needs a signing identity of the profile's team" >&2
        exit 1
    fi
    bundle_id="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$helper/Contents/Info.plist")"
    bash "$PROJECT_DIR/scripts/agent-entitlements.sh" "$profile" "${DEVELOPMENT_TEAM:-}" "$bundle_id" \
        "$identity" "$DERIVED_FILE_DIR/agent.entitlements"
    install -m 0644 "$profile" "$helper/Contents/embedded.provisionprofile"
    entitlements=(--entitlements "$DERIVED_FILE_DIR/agent.entitlements")
fi
codesign --force ${runtime[@]+"${runtime[@]}"} ${entitlements[@]+"${entitlements[@]}"} "$timestamp" \
    --sign "$identity" "$helper"

# The SHA-256 of the agent as signed: signing rewrites the executable, so
# this comes last (the plist is outside the helper; Xcode signs the app,
# plist included, afterwards). The agent reports the same digest of itself
# (`AgentHello.agent_digest`), so the app can tell a service still running
# another build, even of the same version, and restart it
# (OutdatedService.swift). An ad hoc signature needs it too: with no team,
# launchd runs the service only with the agent's code hash from when it was
# registered, and registering again keeps that hash while the plist stays
# the same; a new digest lets the app's registering again take a rebuilt
# agent.
digest="$(shasum -a 256 "$helper/Contents/MacOS/inputweave" | cut -d ' ' -f 1)"
/usr/libexec/PlistBuddy -c "Add :InputWeaveAgentDigest string $digest" "$plist"
chmod 0644 "$plist"
