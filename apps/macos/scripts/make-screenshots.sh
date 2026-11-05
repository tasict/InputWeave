#!/bin/bash
# Renders the app's views with sample data (the InputWeaveSnapshots scheme)
# and copies the ones the READMEs and the user guide show into
# docs/screenshots/:
#
#   apps/macos/scripts/make-screenshots.sh
#
# The images there are build products: change the views or the sample data
# in apps/macos/InputWeaveSnapshots, then run this again. The *-dark images
# are for GitHub's dark theme (<picture> with prefers-color-scheme). The
# onboarding welcome page is left out on purpose: the test runner has no app
# icon, so that snapshot shows a generic one.
set -euo pipefail

repo="$(cd "$(dirname "$0")/../../.." && pwd)"
# Where Snapshots.swift writes, FileManager.default.temporaryDirectory.
snapshots="${TMPDIR:-$(getconf DARWIN_USER_TEMP_DIR)}"
snapshots="${snapshots%/}/InputWeaveSnapshots"
derived="$HOME/Library/Developer/Xcode/DerivedData/InputWeave-snap"
out="$repo/docs/screenshots"
curated=(
    panel.png
    panel-controlling.png
    panel-dark.png
    arrangement.png
    arrangement-dark.png
    arrangement-pictures.png
    arrangement-pictures-dark.png
    pairing-code.png
    add-device.png
    status-icon-2x.png
    about.png
    about-dark.png
)

cd "$repo"
xcodegen generate --spec apps/macos/project.yml
# Old renders must not stand in for ones this run failed to write.
rm -rf "$snapshots"
xcodebuild -project apps/macos/InputWeave.xcodeproj -scheme InputWeaveSnapshots \
    -derivedDataPath "$derived" test

mkdir -p "$out"
for name in "${curated[@]}"; do
    if [ ! -f "$snapshots/$name" ]; then
        echo "make-screenshots: $snapshots/$name was not rendered" >&2
        exit 1
    fi
    cp "$snapshots/$name" "$out/$name"
    echo "docs/screenshots/$name"
done
