#!/usr/bin/env bash
# Builds the website with scripts/site-build.sh and deploys it to its
# Cloudflare Pages project, which serves inputweave.ctsite.cc:
#
#   scripts/site-deploy.sh [--dry-run]
#
# The site carries _redirects, the stable addresses the apps' update feeds
# and the Linux package source use (docs/architecture.md §12), so deploying
# it also changes where those lead.
#
# Needs Node (npx runs a pinned wrangler) and a wrangler that is logged in
# to the Cloudflare account that owns the project (`npx wrangler login`).
#
# Environment:
#   INPUTWEAVE_PAGES_PROJECT  the Pages project (default inputweave)
set -euo pipefail

wrangler="wrangler@4.158.0"
project="${INPUTWEAVE_PAGES_PROJECT:-inputweave}"
dry_run=0
case "${1-}" in
    "") ;;
    --dry-run) dry_run=1 ;;
    *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
esac

repo="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

"$repo/scripts/site-build.sh" "$work/site"
# The marker only tells site-build.sh the folder is its own.
rm -f "$work/site/.site-build"

commit="$(git -C "$repo" rev-parse HEAD)"
subject="$(git -C "$repo" log -1 --format=%s)"
dirty=false
[ -z "$(git -C "$repo" status --porcelain -- site scripts/site-build.sh docs/screenshots docs/brand THIRD_PARTY_NOTICES.md)" ] || dirty=true

if [ "$dry_run" = 1 ]; then
    echo "Dry run: would deploy to the Pages project $project (commit $commit, dirty: $dirty):"
    (cd "$work/site" && find . -type f | sed 's|^\./||' | sort)
    exit 0
fi

(cd "$work" && npx --yes "$wrangler" pages deploy site \
    --project-name "$project" --branch main \
    --commit-hash "$commit" --commit-message "$subject" --commit-dirty="$dirty")
