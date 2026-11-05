#!/bin/sh
# Prints the Cargo workspace's version, `version` under [workspace.package]
# in Cargo.toml: the version of the agent, the apps and every installer.
#
#   scripts/workspace-version.sh [Cargo.toml]
#
# The one place the build and release scripts read it from;
# scripts/set-version.sh changes it.
set -eu

manifest=${1:-$(cd "$(dirname "$0")/.." && pwd)/Cargo.toml}
version=$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\([0-9][0-9.]*\)"$/\1/p' "$manifest" | head -n 1)
if [ -z "$version" ]; then
    echo "workspace-version.sh: no version under [workspace.package] in $manifest" >&2
    exit 1
fi
echo "$version"
