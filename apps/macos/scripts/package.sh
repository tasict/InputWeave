#!/bin/bash
# Builds a universal release of the macOS app and packs it into the
# installer package:
#
#   apps/macos/scripts/package.sh [output-directory]    (default: dist/)
#
# InputWeave-<version>.pkg is the one way to install InputWeave and to
# update it. Dragging the app into Applications cannot replace a copy the
# package installed: that copy belongs to root, and the app and its
# background service are running. The package quits the app before
# replacing it and opens the new one afterwards, which restarts the
# service (see apps/macos/Installer). Besides installing, the package can
# remove an existing installation, keeping or deleting the pairings.
# Installer shows it with a welcome page in English or Traditional Chinese
# and the brand's mark, from apps/macos/Installer/Resources.
#
# With INPUTWEAVE_DMG=1 the script also makes InputWeave-<version>.dmg,
# the app to drag into Applications, for trying a build on a Mac where the
# package has not installed InputWeave. Its window sets both on the
# brand's canvas: the picture comes from
# apps/macos/scripts/make-brand-assets.swift, the layout from Finder,
# driven by apps/macos/Installer/dmg/layout.applescript. So that needs a
# logged-in session, and the first run asks to let the terminal control
# Finder; without Finder it fails, unless INPUTWEAVE_DMG_LAYOUT says to
# skip the layout.
#
# Environment:
#   INPUTWEAVE_SIGN_IDENTITY   e.g. "Developer ID Application: Name (TEAMID)".
#                              Without it the build is signed ad hoc, which
#                              is only good for trying it locally, and
#                              every rebuild loses the permissions granted.
#                              An "Apple Development" identity suits builds
#                              for your own Macs: they keep the permissions.
#   INPUTWEAVE_TEAM_ID         the team of that identity.
#   INPUTWEAVE_INSTALLER_IDENTITY
#                              e.g. "Developer ID Installer: Name (TEAMID)",
#                              to sign the installer package. Without it
#                              the package is unsigned.
#   INPUTWEAVE_NOTARY_PROFILE  a keychain profile saved with
#                              `xcrun notarytool store-credentials`. With
#                              it, the app, the package (when signed) and
#                              the DMG are notarized and stapled.
#   INPUTWEAVE_APPCAST_URL, INPUTWEAVE_SPARKLE_PUBLIC_KEY
#                              the Sparkle update feed and the public key
#                              from Sparkle's generate_keys. Unset, the
#                              build checks InputWeave's GitHub releases
#                              (project.yml's Release values). Set, they
#                              replace those; either set to nothing
#                              (INPUTWEAVE_APPCAST_URL=) makes a build that
#                              never checks for updates, for trying a
#                              build without it offering the release.
#   INPUTWEAVE_AGENT_PROFILE   a provisioning profile of the team that
#                              grants com.apple.developer.hid.virtual.device
#                              for cc.ctsite.inputweave.agent: a
#                              Developer ID profile for releases, or, with
#                              an Apple Development identity, a development
#                              profile listing this Mac (such a build runs
#                              only on the Macs it lists). With it,
#                              the agent's helper carries the profile and
#                              that entitlement, and other computers can
#                              type on this Mac's lock screen through a
#                              virtual keyboard and mouse. Without it the
#                              helper has neither. Needs
#                              INPUTWEAVE_SIGN_IDENTITY and
#                              INPUTWEAVE_TEAM_ID.
#   INPUTWEAVE_DMG=1           also make the DMG.
#   INPUTWEAVE_DMG_LAYOUT=skip with INPUTWEAVE_DMG=1, a plain DMG, for
#                              machines without a logged-in Finder: the app
#                              and the Applications link in Finder's default
#                              window, with no background picture. It is
#                              signed as usual.
#
# macOS keeps the permissions it granted (Accessibility, Input Monitoring)
# for the background service's bundle identifier,
# cc.ctsite.inputweave.agent, and its signing team. Every release must
# keep both, or users have to grant the permissions again after updating.
# Ad hoc builds lose them on every rebuild.
set -euo pipefail

repo="$(cd "$(dirname "$0")/../../.." && pwd)"
out="$(mkdir -p "${1:-$repo/dist}" && cd "${1:-$repo/dist}" && pwd)"
identity="${INPUTWEAVE_SIGN_IDENTITY:--}"
team="${INPUTWEAVE_TEAM_ID:-}"
installer_identity="${INPUTWEAVE_INSTALLER_IDENTITY:-}"
installer="$repo/apps/macos/Installer"
notary="${INPUTWEAVE_NOTARY_PROFILE:-}"
want_dmg="${INPUTWEAVE_DMG:-}"
agent_profile="${INPUTWEAVE_AGENT_PROFILE:-}"
if [ -n "$agent_profile" ]; then
    # Absolute: Xcode runs embed-agent.sh from the project's folder.
    agent_profile="$(cd "$(dirname "$agent_profile")" && pwd)/$(basename "$agent_profile")"
fi
dmg_layout="${INPUTWEAVE_DMG_LAYOUT:-}"
work="$(mktemp -d "${TMPDIR:-/tmp}/inputweave-package.XXXXXX")"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
# Xcode and the launch check register the app in $work with Launch
# Services. launchd resolves the background service through the newest
# copy it knows, so a deleted build product here would break the service
# installed on this Mac. Nested apps need unregistering one by one.
# A failed run also ejects the DMG's volume, or the next run would find its
# name taken.
rw_device=""
verify_mount=""
cleanup() {
    for disk in "$verify_mount" "$rw_device"; do
        [ -z "$disk" ] || hdiutil detach -quiet -force "$disk" 2>/dev/null || true
    done
    if [ -d "$work/build/Build/Products" ]; then
        find "$work/build/Build/Products" -name "*.app" -type d -print0 |
            xargs -0 -n 1 "$lsregister" -u 2>/dev/null || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT

if [ "$identity" = "-" ] && [ -n "$notary" ]; then
    echo "error: notarizing needs a Developer ID identity in INPUTWEAVE_SIGN_IDENTITY" >&2
    exit 1
fi
if [ -n "$agent_profile" ] && { [ "$identity" = "-" ] || [ -z "$team" ]; }; then
    echo "error: INPUTWEAVE_AGENT_PROFILE needs INPUTWEAVE_SIGN_IDENTITY and INPUTWEAVE_TEAM_ID" >&2
    exit 1
fi
case "$want_dmg" in
    "" | 0 | 1) ;;
    *) echo "error: INPUTWEAVE_DMG is 1 or unset, not '$want_dmg'" >&2; exit 1 ;;
esac
case "$dmg_layout" in
    "" | skip) ;;
    *) echo "error: INPUTWEAVE_DMG_LAYOUT is 'skip' or unset, not '$dmg_layout'" >&2; exit 1 ;;
esac

# notarytool exits 0 when Apple refuses the upload too, so the status is
# checked here, and Apple's list of issues printed.
notarize() {
    xcrun notarytool submit "$1" --keychain-profile "$notary" --wait | tee "$work/notary.log"
    if ! grep -q '^ *status: Accepted' "$work/notary.log"; then
        submission="$(sed -n 's/^ *id: //p' "$work/notary.log" | head -1)"
        [ -z "$submission" ] || xcrun notarytool log "$submission" --keychain-profile "$notary" >&2 || true
        echo "error: Apple did not accept $(basename "$1")" >&2
        exit 1
    fi
}

universal() {
    for arch in arm64 x86_64; do
        lipo "$1" -verify_arch "$arch" || { echo "error: $1 lacks $arch" >&2; exit 1; }
    done
}

echo "==> Generating the Xcode project"
xcodegen generate --spec "$repo/apps/macos/project.yml" --quiet

echo "==> Building the app (arm64 + x86_64)"
# No get-task-allow: notarization refuses it, and releases are not debugged.
# The hardened runtime only with a real identity: an ad hoc app has no team,
# so library validation would refuse Sparkle.framework at launch.
signing=(CODE_SIGN_IDENTITY="$identity" CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO
    INPUTWEAVE_AGENT_PROFILE="$agent_profile")
# The update feed only when the environment names one, or names none (set
# to nothing); otherwise project.yml's Release values stand.
if [ -n "${INPUTWEAVE_APPCAST_URL+set}" ]; then
    signing+=(INPUTWEAVE_APPCAST_URL="$INPUTWEAVE_APPCAST_URL")
fi
if [ -n "${INPUTWEAVE_SPARKLE_PUBLIC_KEY+set}" ]; then
    signing+=(INPUTWEAVE_SPARKLE_PUBLIC_KEY="$INPUTWEAVE_SPARKLE_PUBLIC_KEY")
fi
if [ "$identity" != "-" ]; then
    # Manual for every target: the identity given here also reaches the
    # Swift packages' targets, which Xcode would otherwise sign
    # automatically and refuse the conflict.
    signing+=(DEVELOPMENT_TEAM="$team" CODE_SIGN_STYLE=Manual OTHER_CODE_SIGN_FLAGS=--timestamp
        ENABLE_HARDENED_RUNTIME=YES)
fi
xcodebuild -project "$repo/apps/macos/InputWeave.xcodeproj" -scheme InputWeave \
    -configuration Release -derivedDataPath "$work/build" -destination "generic/platform=macOS" \
    ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO "${signing[@]}" \
    -quiet build
app="$work/build/Build/Products/Release/InputWeave.app"
agent="$app/Contents/Library/LoginItems/InputWeave Agent.app/Contents/MacOS/inputweave"
version="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$app/Contents/Info.plist")"

if [ "$identity" != "-" ]; then
    # Xcode signs Sparkle.framework as it embeds it, but not the helpers
    # inside, which keep Sparkle's own signatures without a timestamp, and
    # notarization refuses them. Inside out, as Sparkle's documentation
    # says: the helpers, the framework, then the app, whose signature seals
    # the framework's. (Downloader.xpc has entitlements; the others and the
    # app have none, and preserving nothing is harmless.)
    echo "==> Signing Sparkle's helpers"
    sparkle="$app/Contents/Frameworks/Sparkle.framework"
    for code in "$sparkle/Versions/B/XPCServices/Installer.xpc" \
        "$sparkle/Versions/B/XPCServices/Downloader.xpc" "$sparkle/Versions/B/Autoupdate" \
        "$sparkle/Versions/B/Updater.app" "$sparkle" "$app"; do
        codesign --force --sign "$identity" --options runtime --timestamp \
            --preserve-metadata=entitlements "$code"
    done
fi

echo "==> Checking the build"
feed="$(/usr/libexec/PlistBuddy -c "Print :SUFeedURL" "$app/Contents/Info.plist" 2>/dev/null || true)"
feed_key="$(/usr/libexec/PlistBuddy -c "Print :SUPublicEDKey" "$app/Contents/Info.plist" 2>/dev/null || true)"
universal "$agent"
universal "$app/Contents/MacOS/InputWeave"
codesign --verify --deep --strict "$app"
# The virtual keyboard and mouse's entitlement, with its profile or not at
# all: a restricted entitlement without a profile keeps the agent from
# starting.
helper_entitlements="$(codesign -d --entitlements - --xml "$app/Contents/Library/LoginItems/InputWeave Agent.app" 2>/dev/null || true)"
if [ -n "$agent_profile" ]; then
    grep -q com.apple.developer.hid.virtual.device <<<"$helper_entitlements" || {
        echo "error: the agent is not signed with the virtual keyboard and mouse's entitlement" >&2
        exit 1
    }
elif grep -q com.apple.developer.hid.virtual.device <<<"$helper_entitlements"; then
    echo "error: the agent has the virtual keyboard and mouse's entitlement without a profile" >&2
    exit 1
fi
# The app restarts a service whose agent reports another digest than its
# LaunchAgent plist names (embed-agent.sh), so the two must match.
plist_digest="$(/usr/libexec/PlistBuddy -c "Print :InputWeaveAgentDigest" \
    "$app/Contents/Library/LaunchAgents/cc.ctsite.inputweave.agent.plist")"
agent_digest="$(shasum -a 256 "$agent" | cut -d ' ' -f 1)"
[ "$plist_digest" = "$agent_digest" ] || {
    echo "error: the LaunchAgent plist names agent $plist_digest, but the agent is $agent_digest" >&2
    exit 1
}
# A valid signature does not mean dyld will load the frameworks under it.
launched="$("$app/Contents/MacOS/InputWeave" --version 2>"$work/launch.log")" || {
    cat "$work/launch.log" >&2
    echo "error: the app does not launch" >&2
    exit 1
}
[ "$launched" = "$version" ] || { echo "error: the app reports version '$launched', not $version" >&2; exit 1; }

if [ -n "$notary" ]; then
    echo "==> Notarizing the app"
    ditto -c -k --keepParent "$app" "$work/InputWeave.zip"
    notarize "$work/InputWeave.zip"
    xcrun stapler staple "$app"
fi

echo "==> Making the installer package"
# The app itself, plus two payload-free packages whose scripts remove an
# existing installation.
stage="$work/pkg"
mkdir -p "$stage/root" "$stage/pkgs"
ditto "$app" "$stage/root/InputWeave.app"
# The package keeps the modes it is given; make sure nothing in the app is
# readable by root alone (sources copied from a share have been).
chmod -R u=rwX,go=rX "$stage/root"
for dir in app remove purge; do
    mkdir -p "$stage/scripts/$dir"
    cp "$installer/$dir"/* "$installer/uninstall.sh" "$stage/scripts/$dir/"
done
# The team as codesign reports it, so preinstall can compare.
app_team="$(codesign -dv "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
sed -i '' "s/@TEAM@/$app_team/" "$stage/scripts/app/preinstall"
sed "s/@VERSION@/$version/g" "$installer/Distribution.xml" >"$stage/Distribution.xml"
# Installer's welcome pages, the choices' translations and the
# backgrounds, without the extended attributes or .DS_Store files a copy
# on a network share may carry.
for file in background.tiff {en,zh_TW}.lproj/{welcome.html,Localizable.strings}; do
    [ -f "$installer/Resources/$file" ] || { echo "error: apps/macos/Installer/Resources/$file is missing" >&2; exit 1; }
done
ditto --norsrc --noextattr --noqtn "$installer/Resources" "$stage/resources"
find "$stage/resources" -name .DS_Store -delete
# Always /Applications: by default Installer "upgrades" any copy with the
# same bundle ID it can find, such as a development build. Nor is the
# identifier strict: Installer decides where the app goes before
# preinstall runs, and with a strict identifier it would not replace
# v0.1.0's /Applications/InputWeave.app (com.inputweave.InputWeave) but
# put the new app in a folder of its own, InputWeave.localized, as it did
# on a test Mac on 2026-10-28; preinstall then removed the old app and
# nothing was left in /Applications. preinstall still removes an app of
# other identifiers first, so none of its files stay in the new one.
# (pkgbuild on macOS 27 prints "write: Permission denied" four times even
# for a trivial package; the package is fine.)
pkgbuild --analyze --root "$stage/root" "$stage/component.plist" >/dev/null
plutil -replace 0.BundleIsRelocatable -bool NO "$stage/component.plist"
plutil -replace 0.BundleHasStrictIdentifier -bool NO "$stage/component.plist"
pkgbuild --quiet --root "$stage/root" --component-plist "$stage/component.plist" \
    --identifier cc.ctsite.inputweave --version "$version" --install-location /Applications \
    --scripts "$stage/scripts/app" "$stage/pkgs/app.pkg"
# The login window's agent: a LaunchAgent that launchd starts there as
# root, in a package of its own, since it goes in /Library/LaunchAgents
# rather than /Applications. Its program must be the agent this package
# installs.
login_plist=cc.ctsite.inputweave.loginwindow.plist
mkdir -p "$stage/login-root/Library/LaunchAgents"
install -m 0644 "$installer/login-window/$login_plist" "$stage/login-root/Library/LaunchAgents/$login_plist"
plutil -lint -s "$stage/login-root/Library/LaunchAgents/$login_plist"
login_program="$(/usr/libexec/PlistBuddy -c "Print :ProgramArguments:0" \
    "$stage/login-root/Library/LaunchAgents/$login_plist")"
[ "$login_program" = "/Applications/InputWeave.app/${agent#"$app/"}" ] || {
    echo "error: the login window's LaunchAgent starts $login_program, not the app's agent" >&2
    exit 1
}
pkgbuild --quiet --root "$stage/login-root" --ownership recommended \
    --identifier cc.ctsite.inputweave.loginwindow --version "$version" --install-location / \
    "$stage/pkgs/login-window.pkg"
for dir in remove purge; do
    pkgbuild --quiet --nopayload --identifier "cc.ctsite.inputweave.$dir" --version "$version" \
        --scripts "$stage/scripts/$dir" "$stage/pkgs/$dir.pkg"
done
pkg="$out/InputWeave-$version.pkg"
rm -f "$pkg"
pkg_signing=()
if [ -n "$installer_identity" ]; then
    pkg_signing=(--sign "$installer_identity" --timestamp)
fi
productbuild --quiet --distribution "$stage/Distribution.xml" --package-path "$stage/pkgs" \
    --resources "$stage/resources" ${pkg_signing[@]+"${pkg_signing[@]}"} "$pkg"
if [ -n "$notary" ] && [ -n "$installer_identity" ]; then
    echo "==> Notarizing the installer package"
    notarize "$pkg"
    xcrun stapler staple "$pkg"
fi

if [ "$want_dmg" = 1 ]; then
    echo "==> Making the DMG"
    # Filled and laid out by Finder in a read-write image, then compressed.
    # Finder keeps the layout in the volume's .DS_Store, and scripts the
    # volume by its name, so no other volume of that name may be mounted
    # meanwhile. A plain DMG skips Finder's part.
    dmg="$out/InputWeave-$version.dmg"
    volume=InputWeave
    if [ -e "/Volumes/$volume" ]; then
        echo "error: eject /Volumes/$volume first; the DMG's volume has the same name" >&2
        exit 1
    fi
    rw="$work/InputWeave-rw.dmg"
    hdiutil create -quiet -size "$(($(du -sm "$app" | cut -f1) + 20))m" -fs HFS+ -volname "$volume" \
        -type UDIF -nospotlight "$rw"
    # hdiutil warns that -nobrowse is deprecated; its errors still matter.
    attached="$(hdiutil attach -nobrowse -noautoopen -readwrite -noverify "$rw" 2>"$work/attach.log")" || {
        cat "$work/attach.log" >&2
        exit 1
    }
    rw_device="$(echo "$attached" | awk 'NR == 1 { print $1 }')"
    mnt="$(echo "$attached" | awk -F '\t' '$NF ~ /^\/Volumes\// { print $NF }')"
    [ "$mnt" = "/Volumes/$volume" ] || { echo "error: the DMG's volume mounted at '$mnt'" >&2; exit 1; }
    ditto "$app" "$mnt/InputWeave.app"
    ln -s /Applications "$mnt/Applications"
    expected=(.VolumeIcon.icns InputWeave.app Applications)
    if [ "$dmg_layout" != skip ]; then
        mkdir "$mnt/.background"
        install -m 0644 "$installer/dmg/background.tiff" "$mnt/.background/background.tiff"
        osascript "$installer/dmg/layout.applescript" "$volume" >/dev/null || {
            echo "error: Finder could not lay out the DMG's window" >&2
            echo "       (INPUTWEAVE_DMG_LAYOUT=skip makes a plain DMG without Finder)" >&2
            exit 1
        }
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            [ -f "$mnt/.DS_Store" ] && break
            sleep 1
        done
        [ -f "$mnt/.DS_Store" ] || { echo "error: Finder did not save the DMG's layout" >&2; exit 1; }
        expected+=(.DS_Store .background/background.tiff)
    fi
    # The volume's icon, after Finder is done: its update command removes it.
    mkdir "$work/VolumeIcon.iconset"
    cp "$repo/apps/macos/InputWeave/Assets.xcassets/AppIcon.appiconset/"icon_*.png "$work/VolumeIcon.iconset/"
    iconutil -c icns -o "$mnt/.VolumeIcon.icns" "$work/VolumeIcon.iconset"
    SetFile -a C "$mnt"
    rm -rf "$mnt/.fseventsd" "$mnt/.Trashes" "$mnt/.Spotlight-V100"
    sync
    # Finder may hold on to the volume for a few seconds after closing it.
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        hdiutil detach -quiet "$rw_device" && rw_device="" && break
        sleep 2
    done
    [ -z "$rw_device" ] || { echo "error: cannot eject the DMG's volume" >&2; exit 1; }
    rm -f "$dmg"
    hdiutil convert -quiet "$rw" -format UDZO -imagekey zlib-level=9 -o "$dmg"
    # Check the compressed image's files, leaving Finder out of it.
    verify_mount="$work/verify"
    mkdir "$verify_mount"
    hdiutil attach -quiet -readonly -nobrowse -noautoopen -mountpoint "$verify_mount" "$dmg"
    for item in "${expected[@]}"; do
        [ -e "$verify_mount/$item" ] || { echo "error: the DMG lacks $item" >&2; exit 1; }
    done
    for item in .fseventsd .Trashes .Spotlight-V100; do
        [ ! -e "$verify_mount/$item" ] || { echo "error: the DMG contains $item" >&2; exit 1; }
    done
    [ "$(GetFileInfo -aC "$verify_mount")" = 1 ] || { echo "error: the DMG's volume icon is not set" >&2; exit 1; }
    hdiutil detach -quiet "$verify_mount"
    verify_mount=""
    if [ "$identity" != "-" ]; then
        codesign --sign "$identity" --timestamp "$dmg"
    fi

    if [ -n "$notary" ]; then
        echo "==> Notarizing the DMG"
        notarize "$dmg"
        xcrun stapler staple "$dmg"
        spctl --assess --type open --context context:primary-signature -v "$dmg"
    fi
fi

echo "Wrote $pkg"
if [ -n "$feed" ] && [ -n "$feed_key" ]; then
    echo "updates: $feed"
else
    echo "updates: off (no feed or no key in this build)"
fi
if [ "$want_dmg" = 1 ]; then
    echo "Wrote $dmg"
fi
if [ "$identity" = "-" ]; then
    echo "note: signed ad hoc; Gatekeeper blocks the app on other Macs"
fi
if [ -z "$installer_identity" ]; then
    echo "note: the installer package is unsigned"
fi
if [ "$want_dmg" = 1 ] && [ "$dmg_layout" = skip ]; then
    echo "note: the DMG has no window layout or background picture (INPUTWEAVE_DMG_LAYOUT=skip)"
elif [ "$want_dmg" != 1 ] && [ -n "$dmg_layout" ]; then
    echo "note: INPUTWEAVE_DMG_LAYOUT only applies with INPUTWEAVE_DMG=1; no DMG was made"
fi
