#!/usr/bin/env bash
# Builds the website (site/) into a folder ready to publish, for
# Cloudflare Pages (scripts/site-deploy.sh) or a local preview:
#
#   scripts/site-build.sh <out-dir>
#
# - Copies site/ (with _redirects, the stable release addresses) and the
#   images it shows from docs/ (screenshots, the app icon), the app's 32 px
#   icon as the PNG favicon, and THIRD_PARTY_NOTICES.md as
#   third-party-notices.txt, which the Windows app's About opens.
# - Stamps every reference into assets/ with ?v=<first 8 hex digits of the
#   file's SHA-256>, in the pages and in the stylesheet, so a cache in front
#   of the site (Cloudflare keeps assets for hours) never serves an old file
#   with a new page.
# - Fails when a page links to a file or an #anchor that isn't there.
#
# Needs bash and python3.
# The out-dir must be new, empty, or a previous output of this script.
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 <out-dir>" >&2
    exit 2
fi

repo="$(cd "$(dirname "$0")/.." && pwd)"
out="$1"
marker=".site-build"

if [ -e "$out" ]; then
    if [ ! -d "$out" ]; then
        echo "error: $out exists and is not a folder" >&2
        exit 1
    fi
    if [ -f "$out/$marker" ]; then
        find "$out" -mindepth 1 -delete
    elif [ -n "$(ls -A "$out")" ]; then
        echo "error: $out is not empty and was not made by this script" >&2
        exit 1
    fi
else
    mkdir -p "$out"
fi

cp -R "$repo/site/." "$out/"
touch "$out/$marker"
cp "$repo/THIRD_PARTY_NOTICES.md" "$out/third-party-notices.txt"

# Images that live with the app and its docs.
mkdir -p "$out/assets/img"
for name in arrangement arrangement-dark panel panel-dark; do
    cp "$repo/docs/screenshots/$name.png" "$out/assets/img/$name.png"
done
cp "$repo/docs/brand/app-icon-1024.png" "$out/assets/img/app-icon-1024.png"
cp "$repo/apps/macos/InputWeave/Assets.xcassets/AppIcon.appiconset/icon_32x32.png" "$out/assets/img/favicon-32.png"

python3 - "$out" <<'PY'
import hashlib
import html.parser
import os
import posixpath
import re
import sys
from urllib.parse import urlsplit

out = sys.argv[1]
problems = []


def digest(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()[:8]


def is_local(url):
    if not url or url.startswith(("#", "data:", "mailto:", "tel:", "//")):
        return False
    return not re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", url)


def resolve(page_rel, url):
    """The file a relative URL on page_rel names, relative to out."""
    path = urlsplit(url).path
    base = posixpath.dirname(page_rel)
    target = posixpath.normpath(posixpath.join(base, path)) if path else page_rel
    if target in (".", ""):
        target = ""
    full = os.path.join(out, target)
    if path.endswith("/") or os.path.isdir(full):
        target = posixpath.join(target, "index.html") if target else "index.html"
    elif not os.path.exists(full) and os.path.isfile(full + ".html"):
        # Pages serves page.html at page, so the site links to pages without .html.
        target += ".html"
    return target


def stamp(page_rel, url):
    target = resolve(page_rel, url)
    if not target.startswith("assets/"):
        return url
    full = os.path.join(out, target)
    if not os.path.isfile(full):
        return url  # reported by the link check
    parts = urlsplit(url)
    return parts.path + "?v=" + digest(full) + (("#" + parts.fragment) if parts.fragment else "")


# 1. Stylesheets first: their own references, then they get their hash.
url_re = re.compile(r"url\(\s*([\"']?)([^\"')]+)\1\s*\)")
for dirpath, _, files in os.walk(os.path.join(out, "assets")):
    for name in files:
        if not name.endswith(".css"):
            continue
        full = os.path.join(dirpath, name)
        rel = os.path.relpath(full, out).replace(os.sep, "/")
        text = open(full, encoding="utf-8").read()

        def css_sub(m):
            url = m.group(2)
            if not is_local(url):
                return m.group(0)
            target = resolve(rel, url)
            if not os.path.isfile(os.path.join(out, target)):
                problems.append(f"{rel}: url({url}) names a missing file")
                return m.group(0)
            return "url(" + m.group(1) + stamp(rel, url) + m.group(1) + ")"

        text = url_re.sub(css_sub, text)
        open(full, "w", encoding="utf-8").write(text)


# 2. Pages: collect ids, then stamp and check every reference.
class Page(html.parser.HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids = set()
        self.refs = []  # (attr, value)

    def handle_starttag(self, tag, attrs):
        for k, v in attrs:
            if k in ("id", "name") and v and tag != "meta":
                self.ids.add(v)
            if v is None:
                continue
            if k in ("href", "src") and not (tag == "link" and dict(attrs).get("rel") in ("canonical", "alternate")):
                self.refs.append((k, v))
            elif k == "srcset":
                for item in v.split(","):
                    item = item.strip()
                    if item:
                        self.refs.append(("srcset", item.split()[0]))


pages = {}
for dirpath, _, files in os.walk(out):
    for name in files:
        if name.endswith(".html"):
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, out).replace(os.sep, "/")
            text = open(full, encoding="utf-8").read()
            p = Page()
            p.feed(text)
            pages[rel] = (text, p)

for rel, (text, p) in pages.items():
    for attr, url in p.refs:
        if url.startswith("#"):
            frag = url[1:]
            if frag and frag not in p.ids:
                problems.append(f"{rel}: {url} names no element on the page")
            continue
        if not is_local(url):
            continue
        target = resolve(rel, url)
        if not os.path.isfile(os.path.join(out, target)):
            problems.append(f"{rel}: {attr}={url} names a missing file ({target})")
            continue
        frag = urlsplit(url).fragment
        if frag and target.endswith(".html"):
            ids = pages[target][1].ids if target in pages else set()
            if frag not in ids:
                problems.append(f"{rel}: {url} names no element in {target}")

    attr_re = re.compile(r"""(\s(?:href|src|srcset)=")([^"]*)(")""")

    def page_sub(m):
        value = m.group(2)
        if m.group(1).strip().startswith("srcset"):
            items = []
            for item in value.split(","):
                bits = item.strip().split()
                if bits and is_local(bits[0]):
                    bits[0] = stamp(rel, bits[0])
                items.append(" ".join(bits))
            return m.group(1) + ", ".join(items) + m.group(3)
        if is_local(value):
            return m.group(1) + stamp(rel, value) + m.group(3)
        return m.group(0)

    open(os.path.join(out, rel), "w", encoding="utf-8").write(attr_re.sub(page_sub, text))

if problems:
    print("site-build: broken references:", file=sys.stderr)
    for line in problems:
        print("  " + line, file=sys.stderr)
    sys.exit(1)
print(f"site-build: {len(pages)} pages written to {out}")
PY
