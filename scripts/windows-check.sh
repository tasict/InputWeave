#!/bin/sh
# Runs clippy for a Windows target from macOS or Linux (see
# scripts/windows-check/README.md). Install the target first with
# `rustup target add <target>`.
#
#   scripts/windows-check.sh [target]    default: x86_64-pc-windows-msvc
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
target="${1:-x86_64-pc-windows-msvc}"
var="$(echo "$target" | tr '-' '_')"
export "CFLAGS_$var=-I$here/windows-check/include"
export "AR_$var=$here/windows-check/fake-lib.sh"
cd "$here/.."

# blake3 would assemble with MSVC's tools; its pure Rust build has the
# same API. inputweave and, on Windows, iw-winsvc (and so iw-platform)
# depend on it.
cargo clippy --target "$target" -p inputweave --all-targets --features blake3/pure -- -D warnings
cargo clippy --target "$target" --workspace --exclude inputweave --all-targets --features blake3/pure -- -D warnings
