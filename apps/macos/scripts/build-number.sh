#!/bin/sh
# Prints the build number of a version: major * 10000 + minor * 100 +
# patch, so 0.1.0 is 100 and 1.0.0 is 10000.
#
#   apps/macos/scripts/build-number.sh <major.minor.patch>
#
# It is the macOS app's CFBundleVersion (project.yml's
# CURRENT_PROJECT_VERSION, which scripts/set-version.sh sets and
# embed-agent.sh checks), which Sparkle compares to tell which version is
# newer, and the `build` of the Windows update feed. Deriving it from
# the version keeps it growing with every release without a counter to
# remember. Minor and patch must stay below 100 for that, so versions that
# do not fit are refused rather than numbered out of order.
set -eu

version=${1:?usage: build-number.sh <major.minor.patch>}
case $version in
*[!0-9.]* | .* | *. | *..*)
    echo "build-number.sh: '$version' is not major.minor.patch" >&2
    exit 1
    ;;
esac
old_ifs=$IFS
IFS=.
# shellcheck disable=SC2086
set -- $version
IFS=$old_ifs
if [ $# -ne 3 ]; then
    echo "build-number.sh: '$version' is not major.minor.patch" >&2
    exit 1
fi
# Without leading zeros, which the shell would read as octal.
major=$(echo "$1" | sed 's/^0*\([0-9]\)/\1/')
minor=$(echo "$2" | sed 's/^0*\([0-9]\)/\1/')
patch=$(echo "$3" | sed 's/^0*\([0-9]\)/\1/')
if [ "$minor" -gt 99 ] || [ "$patch" -gt 99 ] || [ "$major" -gt 9999 ]; then
    echo "build-number.sh: $version does not fit major * 10000 + minor * 100 + patch" >&2
    exit 1
fi
echo $((major * 10000 + minor * 100 + patch))
