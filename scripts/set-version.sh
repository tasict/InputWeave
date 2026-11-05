#!/bin/sh
# Sets InputWeave's version everywhere it is written down, together:
#
#   scripts/set-version.sh <major.minor.patch>
#
# - `version` under [workspace.package] in every cargo workspace of the
#   repository that had the old version (the root one, apps/linux, and any
#   other workspace kept in step with them), and their Cargo.lock files;
# - apps/macos/project.yml's MARKETING_VERSION, and CURRENT_PROJECT_VERSION,
#   the build number derived from it (apps/macos/scripts/build-number.sh);
# - the website's download section as it reads before (or without) its
#   script asking GitHub for the newest release: the version and the
#   installers' names on both home pages.
#
# Builds check that these agree (apps/macos/scripts/embed-agent.sh, the
# Linux app's tests). What needs words, the metainfo's <release> entry and
# docs/release-notes/<version>.md, is left to write by hand; the script
# says so.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
new=${1:?usage: set-version.sh <major.minor.patch>}
build=$("$here/apps/macos/scripts/build-number.sh" "$new")
old=$("$here/scripts/workspace-version.sh")

# Rewrites a file in place through sed, keeping its mode.
edit() {
    file=$1
    shift
    sed "$@" "$file" >"$file.set-version"
    cat "$file.set-version" >"$file"
    rm -f "$file.set-version"
}

# Build output, dist/ and hidden folders (git's, editors', other checkouts
# kept inside this one) hold no workspace of the repository.
manifests=$(cd "$here" && find . \( -path ./target -o -path ./dist -o -path './.*' -o -path './*/target' \) -prune \
    -o -name Cargo.toml -print | sort)
# Every manifest first, then every lock file: a workspace's lock also
# records the versions of the path dependencies it takes from another
# workspace (apps/linux's iw-proto and iw-keymap come from the root one),
# so it is only right once all of them carry the new version.
changed=""
for manifest in $manifests; do
    path=$here/${manifest#./}
    current=$("$here/scripts/workspace-version.sh" "$path" 2>/dev/null || true)
    [ "$current" = "$old" ] || continue
    edit "$path" "/^\[workspace\.package\]/,/^\[/ s/^version = \"$old\"\$/version = \"$new\"/"
    echo "${manifest#./}: $old -> $("$here/scripts/workspace-version.sh" "$path")"
    changed="$changed $manifest"
done
for manifest in $changed; do
    path=$here/${manifest#./}
    if [ -f "$(dirname "$path")/Cargo.lock" ]; then
        cargo update --workspace --offline --quiet --manifest-path "$path"
        lock=$(dirname "${manifest#./}")/Cargo.lock
        echo "${lock#./}: updated"
    fi
done

project=$here/apps/macos/project.yml
edit "$project" -e "s/^    MARKETING_VERSION: .*/    MARKETING_VERSION: $new/" \
    -e "s/^    CURRENT_PROJECT_VERSION: .*/    CURRENT_PROJECT_VERSION: $build/"
grep -q "^    MARKETING_VERSION: $new\$" "$project" && grep -q "^    CURRENT_PROJECT_VERSION: $build\$" "$project" || {
    echo "set-version.sh: apps/macos/project.yml has no MARKETING_VERSION or CURRENT_PROJECT_VERSION to set" >&2
    exit 1
}
echo "apps/macos/project.yml: MARKETING_VERSION $new, CURRENT_PROJECT_VERSION $build"

for page in "$here/site/index.html" "$here/site/zh-Hant/index.html"; do
    edit "$page" -E \
        -e "s/(data-latest=\"version\">)[0-9]+\.[0-9]+\.[0-9]+</\1$new</g" \
        -e "s/InputWeave-[0-9]+\.[0-9]+\.[0-9]+(\.pkg|-x64\.msi)/InputWeave-$new\1/g" \
        -e "s/inputweave_[0-9]+\.[0-9]+\.[0-9]+_(amd64|arm64)\.deb/inputweave_${new}_\1.deb/g"
    echo "${page#"$here"/}: the download section names $new"
done

echo "Still to write by hand:"
grep -q "<release version=\"$new\"" "$here/packaging/linux/cc.ctsite.inputweave.metainfo.xml" ||
    echo "  packaging/linux/cc.ctsite.inputweave.metainfo.xml: a <release version=\"$new\" date=\"…\"> entry"
[ -f "$here/docs/release-notes/$new.md" ] || echo "  docs/release-notes/$new.md"
