#!/bin/bash
# Checks the provisioning profile that grants the agent the entitlement for
# its virtual keyboard and mouse, and writes the entitlements the agent's
# helper bundle is signed with:
#
#   agent-entitlements.sh <profile> <team id> <bundle id> <identity> <output plist> [--decoded]
#
# The lock screen and the login window take only input that looks like
# hardware, which the agent sends through a virtual HID device; creating
# one needs com.apple.developer.hid.virtual.device, a managed entitlement
# Apple grants per team (docs/architecture.md §2). A restricted
# entitlement must come with a profile that allows it, or macOS refuses to
# start the executable, so the profile is checked here. It must:
#   - grant the entitlement, belong to the team and name this bundle (or
#     every bundle of the team);
#   - not have expired;
#   - include the certificate of <identity>, the signing identity's name as
#     `security find-identity` lists it, or its SHA-1;
#   - for a Developer ID Application certificate (releases), be a Developer
#     ID profile, for every Mac (ProvisionsAllDevices), not one for a list
#     of devices;
#   - for an Apple Development certificate (trying a build on this Mac
#     with the team's development entitlement), be a development profile
#     whose devices include this Mac's provisioning UDID
#     (`system_profiler SPHardwareDataType`). Such a build runs on those
#     Macs only.
#
# embed-agent.sh runs this when INPUTWEAVE_AGENT_PROFILE names a profile.
# --decoded takes the profile as the property list inside it, as
# `security cms -D` prints it, to try the checks without a real profile;
# INPUTWEAVE_PROVISIONING_UDID then stands in for this Mac's.
set -euo pipefail

if [ $# -lt 5 ] || [ $# -gt 6 ]; then
    echo "usage: $0 <profile> <team id> <bundle id> <identity> <output plist> [--decoded]" >&2
    exit 2
fi
profile="$1"
team="$2"
bundle="$3"
identity="$4"
out="$5"
decoded="${6:-}"
entitlement=com.apple.developer.hid.virtual.device

fail() {
    echo "error: $profile: $1" >&2
    exit 1
}

[ -f "$profile" ] || fail "no such provisioning profile"
[ -n "$team" ] || fail "no team to check it against (INPUTWEAVE_TEAM_ID)"
[ -n "$identity" ] && [ "$identity" != "-" ] || fail "no signing identity to check it against"
work="$(mktemp -d "${TMPDIR:-/tmp}/inputweave-profile.XXXXXX")"
trap 'rm -rf "$work"' EXIT
plist="$work/profile.plist"
case "$decoded" in
    --decoded) cp "$profile" "$plist" ;;
    "") security cms -D -i "$profile" -o "$plist" 2>/dev/null || fail "not a provisioning profile" ;;
    *) echo "error: the sixth argument is --decoded or nothing, not '$decoded'" >&2; exit 2 ;;
esac
get() {
    /usr/libexec/PlistBuddy -c "Print :$1" "$plist" 2>/dev/null || true
}
# Top-level keys only: plutil reads a key path with dots as nested keys.
raw() {
    plutil -extract "$1" raw -o - "$plist" 2>/dev/null || true
}

[ "$(get "Entitlements:$entitlement")" = true ] || fail "does not grant $entitlement"
[ "$(get "TeamIdentifier:0")" = "$team" ] || fail "belongs to team '$(get "TeamIdentifier:0")', not $team"
app_id="$(get "Entitlements:com.apple.application-identifier")"
case "$app_id" in
    "$team.$bundle" | "$team.*") ;;
    *) fail "is for '$app_id', not $team.$bundle" ;;
esac

# Dates in ISO 8601, which compare as strings.
expires="$(raw ExpirationDate)"
[ -n "$expires" ] || fail "has no expiration date"
now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
[[ "$now" < "$expires" ]] || fail "expired on $expires"

# The identity's certificate, by SHA-1.
if [[ "$identity" =~ ^[0-9A-Fa-f]{40}$ ]]; then
    signing_sha1="$identity"
else
    signing_sha1="$(security find-identity -v -p codesigning |
        awk -v name="\"$identity\"" 'index($0, name) { print $2; exit }')"
    [ -n "$signing_sha1" ] || fail "cannot find the signing identity '$identity' in the keychain"
fi
signing_sha1="$(tr '[:lower:]' '[:upper:]' <<<"$signing_sha1")"
found=""
index=0
while cert="$(raw "DeveloperCertificates.$index")" && [ -n "$cert" ]; do
    sha1="$(base64 -D <<<"$cert" | shasum -a 1 | cut -d ' ' -f 1 | tr '[:lower:]' '[:upper:]')"
    if [ "$sha1" = "$signing_sha1" ]; then
        found="$cert"
        break
    fi
    index=$((index + 1))
done
[ -n "$found" ] || fail "does not include the certificate of '$identity' ($signing_sha1)"
# What kind of certificate signs, from its common name.
command -v openssl >/dev/null || fail "cannot tell what kind of certificate signs: openssl is missing"
common_name="$(base64 -D <<<"$found" | openssl x509 -inform DER -noout -subject -nameopt multiline 2>/dev/null |
    sed -n 's/^ *commonName *= //p')"
[ -n "$common_name" ] || fail "cannot read the name of the certificate of '$identity' ($signing_sha1)"

devices() {
    plutil -extract ProvisionedDevices xml1 -o - "$plist" 2>/dev/null |
        sed -n 's:.*<string>\(.*\)</string>.*:\1:p'
}
case "$common_name" in
    "Developer ID Application:"*)
        # Any device list, even an empty one.
        if plutil -extract ProvisionedDevices xml1 -o /dev/null "$plist" 2>/dev/null; then
            fail "lists the devices it is for; a Developer ID profile is for every Mac"
        fi
        [ "$(raw ProvisionsAllDevices)" = true ] || fail "is not a Developer ID profile (no ProvisionsAllDevices)"
        ;;
    "Apple Development:"* | "Mac Developer:"*)
        [ "$(raw ProvisionsAllDevices)" != true ] || fail "is for every Mac; an Apple Development identity needs a development profile"
        udid="${INPUTWEAVE_PROVISIONING_UDID:-}"
        if [ -z "$udid" ]; then
            udid="$(system_profiler SPHardwareDataType 2>/dev/null | sed -n 's/^ *Provisioning UDID: //p')"
        fi
        [ -n "$udid" ] || fail "cannot read this Mac's provisioning UDID"
        grep -qixF -- "$udid" <<<"$(devices)" ||
            fail "does not list this Mac ($udid); add it under Devices and make the profile again"
        ;;
    *) fail "is signed for '$common_name', neither a Developer ID Application nor an Apple Development certificate" ;;
esac

rm -f "$out"
/usr/libexec/PlistBuddy \
    -c "Add :$entitlement bool true" \
    -c "Add :com.apple.application-identifier string $team.$bundle" \
    -c "Add :com.apple.developer.team-identifier string $team" \
    "$out" >/dev/null
