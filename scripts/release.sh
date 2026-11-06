#!/bin/bash
# Publishes a version of InputWeave as a GitHub release, with the update
# feeds and the package repository that installed copies read
# (docs/architecture.md §12):
#
#   scripts/release.sh [--dry-run [--allow-unnotarized] [--previous DIR]] [options]
#
# It takes the installers from dist/ (the names are a contract):
#   InputWeave-<v>.pkg            notarized and stapled
#   InputWeave-<v>-x64.msi        with InputWeave-<v>-x64.msi.json beside it,
#                                 which package.ps1 writes (not published)
#   inputweave_<v>_amd64.deb, inputweave_<v>_arm64.deb
# where <v> is the Cargo workspace version, checks them, and writes, into
# dist/release-<v>/:
#   appcast.xml                   Sparkle's feed (EdDSA signature of the pkg)
#   update-windows.json           the Windows app's feed (Ed25519 of the MSI)
#   Packages, Packages.gz, Release, InRelease, Release.gpg
#                                 the APT flat repository, signed with OpenPGP
#   SHA256SUMS                    every uploaded file's SHA-256
# Signatures and checksums are made over the files as they are uploaded.
# The release notes come from docs/release-notes/<v>.md.
#
# --extra-installer FILE:FEED (any number of times) publishes another
# installer of this version with the release, as it is: it is signed with
# the same Ed25519 key as the Windows installer, and FEED (a .json name)
# describes it in update-windows.json's form, its url under
# <site>/download/v<v>/, so a program that updates itself the Windows
# app's way can read <site>/latest/FEED. Both are in SHA256SUMS. The
# website's download page lists only the installers above.
#
# The apps, the feeds and the .deb's package source name only the website's
# stable addresses (--site): <site>/latest/<file> for the latest release's
# files, <site>/download/v<v>/<file> for one release's and
# <site>/releases/v<v> for its page. The site's _redirects leads them to
# GitHub Releases, so the host can change without a new app; the script
# checks that the deployed site leads them to this repository's releases
# before it creates the release (scripts/site-deploy.sh deploys the site).
#
# The release also carries the previous release's two .deb files, checked
# against that release's SHA256SUMS: apt reads the repository's index and
# fetches the packages it names later, through releases/latest, which may
# by then lead to this release. Only one release back: an index two or
# more releases old names packages latest no longer holds, until the next
# `apt update` (docs/architecture.md §12.4).
#
# With gh, it creates release v<v> as a draft, uploads every file, checks
# what GitHub holds against the local files, and publishes it, marking it
# the latest release only when no published release has a higher version,
# so releases/latest moves once, to a complete release, and never back.
# Run again, it carries on where it stopped: files already uploaded with
# the same SHA-256 are kept, and a draft's files that differ are replaced.
# A published release's files are never replaced; if one differs, it stops.
#
# The generated files are kept in dist/release-<v>/ with everything they
# were made from (the installers, the previous release's packages, the
# notes, the repository, the keys, this script), so a rerun uploads the
# same bytes (OpenPGP signatures carry the time they were made). When any
# of those changed, or --date names another time, they are made again,
# unless the release is already published.
#
# Options:
#   --dry-run             check and generate everything and print what would
#                         be uploaded; nothing touches GitHub or dist/
#   --allow-unnotarized   with --dry-run only: accept a pkg that is not
#                         notarized (an ad hoc test build), with a warning
#   --previous DIR        with --dry-run only: a folder holding a previous
#                         release's SHA256SUMS and its two .deb files, to
#                         attach as a real run attaches those from GitHub
#   --stage DIR           where the generated files are kept (default
#                         dist/release-<v>; a temporary folder for --dry-run)
#   --repo OWNER/NAME     the GitHub repository (INPUTWEAVE_GITHUB_REPO,
#                         default tasict/InputWeave); the site's _redirects
#                         must lead to it
#   --site URL            the website the apps and the .deb read through
#                         (INPUTWEAVE_SITE_URL, default
#                         https://inputweave.ctsite.cc); they must name it
#   --target REF          the commit or branch the tag is made on when it
#                         does not exist yet (default main)
#   --dist DIR            where the installers are (default dist/)
#   --extra-installer FILE:FEED
#                         another installer and the feed written for it
#                         (above)
#   --date YYYY-MM-DDTHH:MM:SSZ
#                         the publication time the feeds state, in UTC:
#                         when the release is made, not a planned one, so a
#                         time more than five minutes ahead is refused
#                         (default: when the generated files are first made)
#
# Environment:
#   INPUTWEAVE_SPARKLE_ACCOUNT  the keychain account of the Ed25519 key
#                               Sparkle's generate_keys made (default
#                               cc.ctsite.inputweave). macOS asks once
#                               whether sign_update may use it; choose
#                               Always Allow, or the script waits on the
#                               dialog.
#   INPUTWEAVE_SPARKLE_KEY_FILE that key as `generate_keys -x` exported it,
#                               used instead of the keychain when set
#   INPUTWEAVE_SIGN_UPDATE      Sparkle's sign_update (default: on PATH, or
#                               in Xcode's DerivedData after a build)
#   INPUTWEAVE_APT_GNUPGHOME    the GnuPG home holding the repository key
#                               (default: GNUPGHOME, or GnuPG's own)
#   INPUTWEAVE_APT_KEY          its fingerprint (default the repository key,
#                               DA65B795D2F3984F0B305013C3AF4DF76FC87E0E)
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
script="$repo_dir/scripts/release.sh"
github_repo="${INPUTWEAVE_GITHUB_REPO:-tasict/InputWeave}"
site_url="${INPUTWEAVE_SITE_URL:-https://inputweave.ctsite.cc}"
target=main
dist="$repo_dir/dist"
dry_run=0
allow_unnotarized=0
previous_dir=""
stage=""
release_date=""
extras=()
sparkle_account="${INPUTWEAVE_SPARKLE_ACCOUNT:-cc.ctsite.inputweave}"
apt_home="${INPUTWEAVE_APT_GNUPGHOME:-${GNUPGHOME:-}}"
apt_key="${INPUTWEAVE_APT_KEY:-DA65B795D2F3984F0B305013C3AF4DF76FC87E0E}"

die() { echo "release: $*" >&2; exit 1; }
warn() { echo "release: warning: $*" >&2; }
say() { echo "==> $*"; }

while [ $# -gt 0 ]; do
    case "$1" in
    --dry-run) dry_run=1 ;;
    --allow-unnotarized) allow_unnotarized=1 ;;
    --previous) previous_dir="$(cd "${2:?--previous needs a folder}" && pwd)"; shift ;;
    --stage) stage="${2:?--stage needs a folder}"; shift ;;
    --repo) github_repo="${2:?--repo needs OWNER/NAME}"; shift ;;
    --site) site_url="${2:?--site needs a URL}"; shift ;;
    --target) target="${2:?--target needs a ref}"; shift ;;
    --dist) dist="$(cd "${2:?--dist needs a folder}" && pwd)"; shift ;;
    --date) release_date="${2:?--date needs YYYY-MM-DDTHH:MM:SSZ}"; shift ;;
    --extra-installer) extras+=("${2:?--extra-installer needs FILE:FEED}"); shift ;;
    -h | --help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument $1 (see --help)" ;;
    esac
    shift
done
[ "$allow_unnotarized" = 0 ] || [ "$dry_run" = 1 ] || die "--allow-unnotarized works only with --dry-run"
[ -z "$previous_dir" ] || [ "$dry_run" = 1 ] || die "--previous works only with --dry-run; a real run takes the previous release from GitHub"
case "$github_repo" in */*) ;; *) die "--repo is OWNER/NAME, not '$github_repo'" ;; esac
[[ "$site_url" =~ ^https://[a-z0-9.-]+$ ]] || die "--site is https://HOST, not '$site_url'"

work="$(mktemp -d "${TMPDIR:-/tmp}/inputweave-release.XXXXXX")"
trap 'rm -rf "$work"' EXIT
tool="$work/tool.py"
cat >"$tool" <<'PY'
"""release.sh's helpers: reading .deb files and GitHub's release list,
comparing versions, and writing the feeds and the package repository."""
import base64, gzip, hashlib, html, io, json, lzma, os, re, sys, tarfile
from datetime import datetime, timezone
from email.utils import format_datetime


def die(message):
    sys.exit("release: " + message)


def digest(path, name="sha256"):
    h = hashlib.new(name)
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def version_key(text):
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", text)
    return tuple(int(x) for x in m.groups()) if m else None


def deb_members(path):
    """A .deb's control.tar and data.tar, each opened only when asked for."""
    data = open(path, "rb").read()
    if not data.startswith(b"!<arch>\n"):
        die(f"{os.path.basename(path)} is not a Debian package")
    pos, raw = 8, {}
    while pos + 60 <= len(data):
        name = data[pos:pos + 16].decode().strip().rstrip("/")
        size = int(data[pos + 48:pos + 58].decode().strip())
        base, _, compression = name.partition(".tar")
        if base in ("control", "data"):
            raw[base] = (name, compression, data[pos + 60:pos + 60 + size])
        pos += 60 + size + (size & 1)
    if "control" not in raw or "data" not in raw:
        die(f"{os.path.basename(path)} lacks control.tar or data.tar")

    def member(base):
        name, compression, body = raw[base]
        if compression == ".xz":
            body = lzma.decompress(body)
        elif compression == ".gz":
            body = gzip.decompress(body)
        elif compression == ".zst":
            die(f"{name} in {os.path.basename(path)} is zstd, which this Python cannot read "
                "(build-deb.sh makes xz)")
        elif compression != "":
            die(f"{name} in {os.path.basename(path)} has an unknown compression")
        return tarfile.open(fileobj=io.BytesIO(body))
    return member


def fields_of(control):
    return dict(re.findall(r"^([A-Za-z-]+): ?(.*)$", control, re.M))


def cmd_deb(path, arch, version, uri, control_out):
    """Checks a package's name, version, architecture and its source's URI,
    and keeps its control file for Packages, so each .deb is read once."""
    member = deb_members(path)
    control = member("control").extractfile("./control").read().decode("utf-8")
    fields = fields_of(control)
    got = (fields.get("Package"), fields.get("Version"), fields.get("Architecture"))
    if got != ("inputweave", version, arch):
        die(f"{os.path.basename(path)} is {' '.join(map(str, got))}, not inputweave {version} {arch}")
    try:
        source = member("data").extractfile("./etc/apt/sources.list.d/inputweave.sources").read().decode()
    except KeyError:
        die(f"{os.path.basename(path)} does not install the package repository's source")
    uris = re.findall(r"^URIs: *(\S+)", source, re.M)
    if uris != [uri]:
        die(f"{os.path.basename(path)}'s source names {uris}, not {uri}")
    open(control_out, "w", encoding="utf-8").write(control)


def cmd_msi_info(path, version, build, sha256, size):
    """Checks the facts package.ps1 wrote beside the MSI against this release."""
    info = json.load(open(path))
    product = str(info.get("product_version", ""))
    if re.fullmatch(r"\d+\.\d+\.\d+\.0", product):
        product = product[:-2]
    checks = [("product_version", product, version), ("build", str(info.get("build")), build),
              ("sha256", str(info.get("sha256", "")).lower(), sha256), ("size", str(info.get("size")), size)]
    for field, got, want in checks:
        if got != want:
            die(f"{os.path.basename(path)} says {field} {got}, not {want}")


def cmd_check_date(date):
    """The publication time: well formed, and not in the future."""
    try:
        when = datetime.strptime(date, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    except ValueError:
        die(f"--date is YYYY-MM-DDTHH:MM:SSZ, not '{date}'")
    if (when - datetime.now(timezone.utc)).total_seconds() > 300:
        die(f"--date {date} is in the future; it is the time the release is published")


def cmd_highest_deb(folder):
    """The highest version among a folder's inputweave_<v>_amd64.deb files."""
    found = [m.group(1) for m in (re.fullmatch(r"inputweave_(\d+\.\d+\.\d+)_amd64\.deb", n)
                                  for n in os.listdir(folder)) if m]
    if found:
        print(max(found, key=version_key))


def cmd_release_setting(project, key):
    """A build setting of project.yml's Release configuration."""
    in_configs = in_release = False
    for line in open(project, encoding="utf-8"):
        if re.match(r"^  configs:\s*$", line):
            in_configs = True
            continue
        if in_configs and re.match(r"^    Release:\s*$", line):
            in_release = True
            continue
        if in_release:
            m = re.match(r"^      ([A-Z_]+): *(.*?)\s*$", line)
            if m:
                if m.group(1) == key:
                    print(m.group(2).strip('"'))
                    return
                continue
            break
    die(f"apps/macos/project.yml has no Release setting {key}")


def releases(path):
    return [json.loads(line) for line in open(path) if line.strip()]


def cmd_releases(path, what, *args):
    rs = releases(path)
    if what == "matching":
        for r in rs:
            if r["tag_name"] == args[0]:
                print(f'{r["id"]} {"draft" if r["draft"] else "published"} {r.get("name") or ""}')
    elif what == "asset":
        tag, name, field = args
        for r in rs:
            if r["tag_name"] == tag:
                for a in r["assets"]:
                    if a["name"] == name:
                        print(a.get(field) or "")
    elif what == "count":
        print(sum(1 for r in rs if r["tag_name"] == args[0]))
    elif what == "highest":
        below = version_key(args[0]) if args else None
        best = None
        for r in rs:
            key = version_key(r["tag_name"])
            if r["draft"] or r.get("prerelease") or key is None:
                continue
            if below is not None and key >= below:
                continue
            if best is None or key > best:
                best = key
        if best is not None:
            print(".".join(map(str, best)))
    else:
        die(f"unknown query {what}")


def cmd_newer(a, b):
    """Exits 0 when version a is higher than b."""
    sys.exit(0 if version_key(a) > version_key(b) else 1)


def cmd_sum(sums, name):
    for line in open(sums):
        parts = line.split()
        if len(parts) == 2 and parts[1] == name:
            print(parts[0])
            return
    die(f"{os.path.basename(sums)} lists no {name}")


def cmd_bytes(b64, out, prefix=""):
    open(out, "wb").write(bytes.fromhex(prefix) + base64.b64decode(b64))


def render_notes(notes_md):
    """A small Markdown subset (## headings, "- " items, paragraphs, `code`,
    **bold**, [links](url)) as HTML for Sparkle and plain text for Windows."""
    lines = open(notes_md, encoding="utf-8").read().splitlines()
    if lines and lines[0].startswith("# "):
        lines = lines[1:]

    def inline_html(text):
        text = html.escape(text, quote=True)
        text = re.sub(r"`([^`]+)`", r"<code>\1</code>", text)
        text = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", text)
        return re.sub(r"\[([^\]]+)\]\((https?://[^)\s]+)\)", r'<a href="\2">\1</a>', text)

    def inline_text(text):
        text = re.sub(r"`([^`]+)`", r"\1", text)
        text = re.sub(r"\*\*([^*]+)\*\*", r"\1", text)
        return re.sub(r"\[([^\]]+)\]\((https?://[^)\s]+)\)", r"\1 (\2)", text)

    blocks, para, items = [], [], []

    def flush():
        if para:
            blocks.append(("p", " ".join(para)))
            para.clear()
        if items:
            blocks.append(("ul", list(items)))
            items.clear()

    for line in lines:
        s = line.strip()
        if not s:
            flush()
        elif s.startswith("#"):
            flush()
            blocks.append(("h", s.lstrip("#").strip()))
        elif s.startswith("- ") or s.startswith("* "):
            if para:
                flush()
            items.append(s[2:].strip())
        elif items and line.startswith("  "):
            items[-1] += " " + s
        else:
            if items:
                flush()
            para.append(s)
    flush()
    out_html, out_text = [], []
    for kind, body in blocks:
        if kind == "h":
            out_html.append(f"<h3>{inline_html(body)}</h3>")
            out_text.append(inline_text(body))
        elif kind == "p":
            out_html.append(f"<p>{inline_html(body)}</p>")
            out_text.append(inline_text(body))
        else:
            out_html.append("<ul>" + "".join(f"<li>{inline_html(i)}</li>" for i in body) + "</ul>")
            out_text.append("\n".join("- " + inline_text(i) for i in body))
    return "\n".join(lines).strip() + "\n", "\n".join(out_html), "\n\n".join(out_text)


def digests(path):
    """MD5 and SHA-256 in one read."""
    md5, sha = hashlib.md5(), hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            md5.update(block)
            sha.update(block)
    return md5.hexdigest(), sha.hexdigest()


def cmd_generate(dist, stage, version, build, site, date, minimum_macos, notes_md,
                 pkg, pkg_sig, msi, msi_sig, msi_sha256, deb_amd64, control_amd64,
                 deb_arm64, control_arm64):
    build = int(build)
    when = datetime.strptime(date, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    download = f"{site}/download/v{version}/"
    notes, notes_html, notes_text = render_notes(notes_md)
    open(os.path.join(stage, "release-notes.md"), "w", encoding="utf-8").write(notes)

    # appcast.xml: this version only; the feed is served from the newest release.
    pkg_path = os.path.join(dist, pkg)
    if "]]>" in notes_html:
        die("the release notes contain ]]>")
    appcast = f"""<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>InputWeave</title>
    <link>https://inputweave.ctsite.cc/</link>
    <description>Updates of InputWeave for macOS</description>
    <language>en</language>
    <item>
      <title>InputWeave {html.escape(version)}</title>
      <pubDate>{format_datetime(when, usegmt=True)}</pubDate>
      <link>{html.escape(site)}/releases/v{html.escape(version)}</link>
      <sparkle:version>{build}</sparkle:version>
      <sparkle:shortVersionString>{html.escape(version)}</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>{html.escape(minimum_macos)}</sparkle:minimumSystemVersion>
      <description><![CDATA[{notes_html}]]></description>
      <enclosure url="{html.escape(download + pkg)}" length="{os.path.getsize(pkg_path)}" type="application/octet-stream" sparkle:installationType="package" sparkle:edSignature="{html.escape(pkg_sig)}"/>
    </item>
  </channel>
</rss>
"""
    open(os.path.join(stage, "appcast.xml"), "w", encoding="utf-8").write(appcast)

    # update-windows.json (docs/architecture.md §12.3).
    msi_path = os.path.join(dist, msi)
    feed = {
        "version": version,
        "build": build,
        "url": download + msi,
        "size": os.path.getsize(msi_path),
        "sha256": msi_sha256,
        "ed25519": msi_sig,
        "released_at": date,
        "minimum_os": "10.0.19045",
        "notes": notes_text,
    }
    with open(os.path.join(stage, "update-windows.json"), "w", encoding="utf-8") as f:
        json.dump(feed, f, indent=2, ensure_ascii=False)
        f.write("\n")

    # The APT flat repository: Packages names this version's two packages.
    stanzas = []
    for deb, control_file in ((deb_amd64, control_amd64), (deb_arm64, control_arm64)):
        path = os.path.join(dist, deb)
        control = open(control_file, encoding="utf-8").read().rstrip("\n")
        md5, sha = digests(path)
        stanzas.append(control + "\n"
                       f"Filename: ./{deb}\n"
                       f"Size: {os.path.getsize(path)}\n"
                       f"MD5sum: {md5}\n"
                       f"SHA256: {sha}\n")
    packages = "\n".join(stanzas).encode("utf-8")
    open(os.path.join(stage, "Packages"), "wb").write(packages)
    with open(os.path.join(stage, "Packages.gz"), "wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0) as gz:
            gz.write(packages)
    index = []
    for name in ("Packages", "Packages.gz"):
        path = os.path.join(stage, name)
        index.append((name, os.path.getsize(path), digest(path, "md5"), digest(path)))
    release = (
        "Origin: InputWeave\n"
        "Label: InputWeave\n"
        f"Version: {version}\n"
        f"Date: {format_datetime(when, usegmt=True)}\n"
        "Architectures: amd64 arm64\n"
        "Acquire-By-Hash: no\n"
        f"Description: InputWeave {version} for Ubuntu and Debian\n"
        "MD5Sum:\n" + "".join(f" {m} {s} {n}\n" for n, s, m, _ in index) +
        "SHA256:\n" + "".join(f" {h} {s} {n}\n" for n, s, _, h in index)
    )
    open(os.path.join(stage, "Release"), "w", encoding="utf-8").write(release)


def cmd_extra_feed(stage, feed, path, version, build, site, date, notes_md, signature):
    """--extra-installer's feed: update-windows.json's form, for another installer."""
    _, _, notes_text = render_notes(notes_md)
    entry = {
        "version": version,
        "build": int(build),
        "url": f"{site}/download/v{version}/{os.path.basename(path)}",
        "size": os.path.getsize(path),
        "sha256": digest(path),
        "ed25519": signature,
        "released_at": date,
        "notes": notes_text,
    }
    with open(os.path.join(stage, feed), "w", encoding="utf-8") as f:
        json.dump(entry, f, indent=2, ensure_ascii=False)
        f.write("\n")


def cmd_check_feeds(stage, *extra_feeds):
    import xml.dom.minidom
    xml.dom.minidom.parse(os.path.join(stage, "appcast.xml"))
    for feed in ("update-windows.json",) + extra_feeds:
        json.load(open(os.path.join(stage, feed)))


COMMANDS = {
    "deb": cmd_deb, "msi-info": cmd_msi_info, "check-date": cmd_check_date,
    "highest-deb": cmd_highest_deb, "release-setting": cmd_release_setting,
    "releases": cmd_releases, "newer": cmd_newer, "sum": cmd_sum, "bytes": cmd_bytes,
    "generate": cmd_generate, "check-feeds": cmd_check_feeds, "extra-feed": cmd_extra_feed,
}
COMMANDS[sys.argv[1]](*sys.argv[2:])
PY
tool() { python3 -I "$tool" "$@"; }

[ -z "$release_date" ] || tool check-date "$release_date"
version="$("$repo_dir/scripts/workspace-version.sh")"
build="$("$repo_dir/apps/macos/scripts/build-number.sh" "$version")"
tag="v$version"
pkg="InputWeave-$version.pkg"
msi="InputWeave-$version-x64.msi"
deb_amd64="inputweave_${version}_amd64.deb"
deb_arm64="inputweave_${version}_arm64.deb"
installers=("$pkg" "$msi" "$deb_amd64" "$deb_arm64")
generated=(appcast.xml update-windows.json Packages Packages.gz Release InRelease Release.gpg)
notes_md="$repo_dir/docs/release-notes/$version.md"
# What the apps and the .deb read, and where the site leads it.
latest_base="$site_url/latest"
github_latest="https://github.com/$github_repo/releases/latest/download"
github_download="https://github.com/$github_repo/releases/download"
feed_url="$latest_base/appcast.xml"
# The feed and key the macOS app's release builds carry (project.yml).
project_feed="$(tool release-setting "$repo_dir/apps/macos/project.yml" INPUTWEAVE_APPCAST_URL)"
sparkle_public_key="$(tool release-setting "$repo_dir/apps/macos/project.yml" INPUTWEAVE_SPARKLE_PUBLIC_KEY)"
[ "$project_feed" = "$feed_url" ] ||
    die "apps/macos/project.yml's release builds check $project_feed, not $feed_url"

if [ -z "$stage" ]; then
    if [ "$dry_run" = 1 ]; then stage="$work/stage"; else stage="$dist/release-$version"; fi
fi
mkdir -p "$stage"
stage="$(cd "$stage" && pwd)"

# Each local file is hashed once: the installers run to tens of megabytes.
# Files downloaded from GitHub are hashed afresh (sha256_fresh).
mkdir -p "$work/sha256"
sha256_fresh() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
sha256() {
    cached="$work/sha256/$(printf '%s' "$1" | shasum -a 1 | cut -c 1-40)"
    [ -f "$cached" ] || sha256_fresh "$1" >"$cached"
    cat "$cached"
}
size_of() { stat -f %z "$1"; }

say "InputWeave $version (build $build) for $github_repo"

# ---------------------------------------------------------------- the files
for file in "${installers[@]}"; do
    [ -f "$dist/$file" ] || die "dist/ has no $file"
    [ -s "$dist/$file" ] || die "dist/$file is empty"
done
others="$(cd "$dist" && ls -1 | grep -E '\.(pkg|msi|deb|dmg)$' | grep -vxF -e "$pkg" -e "$msi" -e "$deb_amd64" -e "$deb_arm64" || true)"
[ -z "$others" ] || warn "dist/ also holds installers of other versions, which are not uploaded: $(echo "$others" | tr '\n' ' ')"

# --extra-installer FILE:FEED: the file as given, the feed a new name.
extra_files=()
extra_feeds=()
for pair in ${extras[@]+"${extras[@]}"}; do
    file="${pair%:*}"
    feed="${pair##*:}"
    [ "$file" != "$pair" ] || die "--extra-installer is FILE:FEED, not '$pair'"
    [ -f "$file" ] && [ -s "$file" ] || die "--extra-installer: $file is not a file with something in it"
    file="$(cd "$(dirname "$file")" && pwd)/$(basename "$file")"
    name="$(basename "$file")"
    [[ "$name" =~ ^[A-Za-z0-9._+-]+$ ]] || die "--extra-installer: $name is not a plain file name"
    [[ "$name" == *"$version"* ]] || die "--extra-installer: $name does not name version $version"
    [[ "$feed" =~ ^[A-Za-z0-9._-]+\.json$ ]] || die "--extra-installer: the feed $feed is not NAME.json"
    for taken in "${installers[@]}" "${generated[@]}" SHA256SUMS ${extra_files[@]+"${extra_files[@]##*/}"} ${extra_feeds[@]+"${extra_feeds[@]}"}; do
        [ "$name" != "$taken" ] && [ "$feed" != "$taken" ] || die "--extra-installer: $name or $feed is already one of the release's files"
    done
    extra_files+=("$file")
    extra_feeds+=("$feed")
done
generated+=(${extra_feeds[@]+"${extra_feeds[@]}"})

say "Checking $pkg"
pkg_ok=1
if ! pkgutil --check-signature "$dist/$pkg" >"$work/pkgutil.txt" 2>&1; then
    pkg_ok=0
elif ! grep -q "Developer ID Installer" "$work/pkgutil.txt" ||
    ! grep -q "trusted by the Apple notary service" "$work/pkgutil.txt"; then
    pkg_ok=0
fi
if [ "$pkg_ok" = 1 ]; then
    assessment="$(spctl --assess --type install -vv "$dist/$pkg" 2>&1 || true)"
    case "$assessment" in *"source=Notarized Developer ID"*) ;; *) pkg_ok=0 ;; esac
fi
if [ "$pkg_ok" = 1 ]; then
    xcrun stapler validate "$dist/$pkg" >/dev/null 2>&1 || pkg_ok=0
fi
if [ "$pkg_ok" = 0 ]; then
    if [ "$allow_unnotarized" = 1 ]; then
        warn "$pkg is not signed with a Developer ID Installer identity, notarized and stapled (accepted for this dry run)"
    else
        cat "$work/pkgutil.txt" >&2
        die "$pkg must be signed with a Developer ID Installer identity, notarized and stapled"
    fi
fi
pkgutil --expand-full "$dist/$pkg" "$work/pkg" >/dev/null
info="$(find "$work/pkg" -path '*/Payload/InputWeave.app/Contents/Info.plist' | head -n 1)"
[ -n "$info" ] || die "$pkg holds no InputWeave.app"
plist() { /usr/libexec/PlistBuddy -c "Print :$1" "$info" 2>/dev/null || true; }
[ "$(plist CFBundleShortVersionString)" = "$version" ] || die "$pkg's app is version $(plist CFBundleShortVersionString), not $version"
[ "$(plist CFBundleVersion)" = "$build" ] || die "$pkg's app is build $(plist CFBundleVersion), not $build"
if [ "$(plist SUFeedURL)" != "$feed_url" ] || [ "$(plist SUPublicEDKey)" != "$sparkle_public_key" ]; then
    msg="$pkg's app does not check $feed_url with the release key (SUFeedURL '$(plist SUFeedURL)'), so it would never update"
    if [ "$dry_run" = 1 ]; then warn "$msg"; else die "$msg"; fi
fi
minimum_macos="$(plist LSMinimumSystemVersion)"
[ -n "$minimum_macos" ] || minimum_macos=15.0

say "Checking $msi"
# A Windows Installer database is an OLE compound file.
[ "$(head -c 8 "$dist/$msi" | xxd -p)" = d0cf11e0a1b11ae1 ] || die "$msi is not a Windows Installer package"
# This Mac cannot read an MSI, so package.ps1 writes beside it the
# ProductVersion the MSI carries, the build number, its SHA-256 and size;
# copy both from the build machine. The sidecar is not published.
[ -f "$dist/$msi.json" ] || die "dist/ has no $msi.json, which apps/windows/scripts/package.ps1 writes beside the MSI"
tool msi-info "$dist/$msi.json" "$version" "$build" "$(sha256 "$dist/$msi")" "$(size_of "$dist/$msi")"

say "Checking $deb_amd64 and $deb_arm64"
tool deb "$dist/$deb_amd64" amd64 "$version" "$latest_base/" "$work/control-amd64"
tool deb "$dist/$deb_arm64" arm64 "$version" "$latest_base/" "$work/control-arm64"

say "Checking $site_url's release addresses"
# The deployed site must lead each address the apps and apt use to this
# repository's releases; scripts/site-deploy.sh deploys site/_redirects.
site_problem=""
for pair in "$latest_base/appcast.xml|$github_latest/appcast.xml" \
    "$site_url/download/v$version/$pkg|$github_download/v$version/$pkg" \
    "$site_url/releases/v$version|https://github.com/$github_repo/releases/tag/v$version"; do
    answer="$(curl -sS -o /dev/null -w '%{http_code} %{redirect_url}' "${pair%%|*}" || true)"
    [ "$answer" = "302 ${pair#*|}" ] || site_problem="${pair%%|*} answers '$answer', not a 302 to ${pair#*|}"
done
if [ -z "$site_problem" ]; then
    echo "    they lead to $github_repo's releases"
elif [ "$dry_run" = 1 ]; then
    warn "$site_problem"
else
    die "$site_problem; deploy the site (scripts/site-deploy.sh)"
fi

# ------------------------------------------------- signing tools and keys
sign_update="${INPUTWEAVE_SIGN_UPDATE:-}"
if [ -z "$sign_update" ]; then
    sign_update="$(command -v sign_update || true)"
fi
if [ -z "$sign_update" ]; then
    sign_update="$(ls -1t "$HOME"/Library/Developer/Xcode/DerivedData/*/SourcePackages/artifacts/sparkle/Sparkle/bin/sign_update 2>/dev/null | head -n 1 || true)"
fi
[ -n "$sign_update" ] && [ -x "$sign_update" ] || die "cannot find Sparkle's sign_update (INPUTWEAVE_SIGN_UPDATE, or build the macOS app once)"
sign_key=(--account "$sparkle_account")
if [ -n "${INPUTWEAVE_SPARKLE_KEY_FILE:-}" ]; then
    [ -f "$INPUTWEAVE_SPARKLE_KEY_FILE" ] || die "INPUTWEAVE_SPARKLE_KEY_FILE names no file"
    sign_key=(--ed-key-file "$INPUTWEAVE_SPARKLE_KEY_FILE")
fi
gpg_cmd=(gpg --batch --yes --pinentry-mode loopback --passphrase "")
if [ -n "$apt_home" ]; then
    gpg_cmd+=(--homedir "$apt_home")
fi
command -v gpg >/dev/null || die "gpg is not installed"
where="${apt_home:-the default GnuPG home}"
"${gpg_cmd[@]}" --list-secret-keys "$apt_key" >/dev/null 2>&1 ||
    die "the repository key $apt_key is not in $where (INPUTWEAVE_APT_GNUPGHOME)"

# ------------------------------------------------------- the release notes
[ -f "$notes_md" ] || die "docs/release-notes/$version.md is missing"
if grep -qE 'TODO|\[TBD\]' "$notes_md"; then
    if [ "$dry_run" = 1 ]; then warn "docs/release-notes/$version.md still has TODO or [TBD]"; else die "docs/release-notes/$version.md still has TODO or [TBD]"; fi
fi

# ---------------------------------------------- what GitHub already holds
releases="$work/releases.jsonl"
: >"$releases"
release_state=none
refresh() {
    gh api "repos/$github_repo/releases?per_page=100" --paginate \
        --jq '.[] | {id, tag_name, name, draft, prerelease, assets: [.assets[] | {name, size, digest}]}' >"$releases"
    matching="$(tool releases "$releases" matching "$tag")"
    if [ "$(tool releases "$releases" count "$tag")" -gt 1 ]; then
        echo "$matching" | sed 's/^/    /' >&2
        die "more than one release has the tag $tag (above: id, state, name); delete the extra ones first"
    fi
    case "$matching" in
    *" draft "* | *" draft") release_state=draft ;;
    *" published"*) release_state=published ;;
    *) release_state=none ;;
    esac
}

# GitHub's list of releases lags its writes by a few seconds: after
# creating, uploading to or publishing a release, wait until the list
# shows it (the release in state $1, and every file of $assets when
# uploaded is given).
refresh_until() {
    for attempt in 1 2 3 4 5 6 7 8 9 10 11 12; do
        refresh
        if [ "$release_state" = "$1" ]; then
            [ "${2-}" = uploaded ] || return 0
            missing=""
            for path in "${assets[@]}"; do
                [ -n "$(tool releases "$releases" asset "$tag" "$(basename "$path")" size)" ] || missing=1
            done
            [ -n "$missing" ] || return 0
        fi
        sleep 5
    done
    return 1
}
if [ "$dry_run" = 0 ]; then
    command -v gh >/dev/null || die "gh is not installed"
    gh auth status >/dev/null 2>&1 || die "gh is not signed in (gh auth login)"
    gh api "repos/$github_repo" >/dev/null || die "cannot read the repository $github_repo"
    refresh
    say "Release $tag on GitHub: $release_state"
fi

# ----------------------- the previous release's packages, for apt's index
previous=""
previous_files=()
if [ "$dry_run" = 0 ]; then
    previous="$(tool releases "$releases" highest "$version")"
    if [ -n "$previous" ]; then
        previous_dir="$work/previous"
        say "Fetching v$previous's packages"
        gh release download "v$previous" --repo "$github_repo" --dir "$previous_dir" \
            --pattern SHA256SUMS --pattern "inputweave_${previous}_amd64.deb" \
            --pattern "inputweave_${previous}_arm64.deb" >/dev/null ||
            die "cannot download v$previous's SHA256SUMS and .deb files"
    fi
elif [ -n "$previous_dir" ]; then
    previous="$(tool highest-deb "$previous_dir")"
    [ -n "$previous" ] || die "--previous $previous_dir holds no inputweave_<version>_amd64.deb"
fi
if [ -n "$previous" ]; then
    tool newer "$version" "$previous" || die "the previous release v$previous is not older than $version"
    for arch in amd64 arm64; do
        file="inputweave_${previous}_$arch.deb"
        [ -f "$previous_dir/$file" ] || die "v$previous has no $file"
        [ -f "$previous_dir/SHA256SUMS" ] || die "v$previous has no SHA256SUMS"
        [ "$(sha256 "$previous_dir/$file")" = "$(tool sum "$previous_dir/SHA256SUMS" "$file")" ] ||
            die "v$previous's $file does not match its SHA256SUMS"
        previous_files+=("$file")
    done
    echo "    carrying v$previous's ${previous_files[*]}"
elif [ "$dry_run" = 1 ]; then
    echo "    a real run also carries the previous release's .deb files (--previous to try that)"
fi

# --------------------- generated files: made once from everything they use
key="$work/stage.key"
{
    for file in "${installers[@]}"; do echo "installer $(sha256 "$dist/$file") $file"; done
    for i in ${extra_files[@]+"${!extra_files[@]}"}; do
        echo "extra $(sha256 "${extra_files[$i]}") $(basename "${extra_files[$i]}") ${extra_feeds[$i]}"
    done
    for file in ${previous_files[@]+"${previous_files[@]}"}; do echo "previous $(sha256 "$previous_dir/$file") $file"; done
    echo "notes $(sha256 "$notes_md")"
    echo "repo $github_repo"
    echo "site $site_url"
    echo "feed $feed_url"
    echo "sparkle-key $sparkle_public_key"
    echo "apt-key $apt_key"
    echo "minimum-macos $minimum_macos"
    echo "script $(sha256 "$script")"
} >"$key"
stored_date=""
[ -f "$stage/release-date" ] && stored_date="$(cat "$stage/release-date")"
regenerate=0
why=""
if ! cmp -s "$key" "$stage/stage.key" 2>/dev/null; then
    regenerate=1
    [ -f "$stage/stage.key" ] && why="what they were made from changed"
fi
if [ -n "$release_date" ] && [ "$release_date" != "$stored_date" ]; then
    regenerate=1
    [ -n "$stored_date" ] && why="--date differs from $stored_date"
fi
for file in "${generated[@]}" SHA256SUMS release-notes.md; do
    [ -f "$stage/$file" ] || regenerate=1
done
if [ "$regenerate" = 1 ] && [ "$release_state" = published ]; then
    die "$tag is published; its files would be made again (${why:-$stage is incomplete}), so nothing is changed"
fi
[ -z "$why" ] || warn "making the files in $stage again: $why"

if [ "$regenerate" = 1 ]; then
    # Only the files this script writes there: --stage may name any folder.
    for file in "${generated[@]}" SHA256SUMS release-notes.md description.md stage.key release-date; do
        rm -f "$stage/$file"
    done
    release_date="${release_date:-${stored_date:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}}"
    say "Signing $pkg and $msi"
    pkg_sig="$("$sign_update" "${sign_key[@]}" -p "$dist/$pkg")"
    msi_sig="$("$sign_update" "${sign_key[@]}" -p "$dist/$msi")"
    extra_sigs=()
    for file in ${extra_files[@]+"${extra_files[@]}"}; do
        say "Signing $(basename "$file")"
        extra_sigs+=("$("$sign_update" "${sign_key[@]}" -p "$file")")
    done
    # Checked against the public key the apps carry: sign_update --verify
    # derives the public key from the same private key, so signing with
    # the wrong key would pass it, and every installed copy would refuse
    # the update. OpenSSL 3 verifies Ed25519; without it, the keychain
    # account's public key is compared instead.
    ed_openssl=""
    for candidate in openssl /opt/homebrew/opt/openssl@3/bin/openssl /usr/local/opt/openssl@3/bin/openssl; do
        help_text="$("$candidate" pkeyutl -help 2>&1 || true)"
        if case "$help_text" in *rawin*) true ;; *) false ;; esac; then
            ed_openssl="$candidate"
            break
        fi
    done
    if [ -n "$ed_openssl" ]; then
        tool bytes "$sparkle_public_key" "$work/sparkle-public.der" 302a300506032b6570032100
        for pair in "$pkg:$pkg_sig" "$msi:$msi_sig"; do
            tool bytes "${pair#*:}" "$work/signature"
            "$ed_openssl" pkeyutl -verify -pubin -inkey "$work/sparkle-public.der" -keyform DER -rawin \
                -in "$dist/${pair%%:*}" -sigfile "$work/signature" >/dev/null 2>&1 ||
                die "the signature of ${pair%%:*} does not verify with the apps' public key $sparkle_public_key"
        done
        for i in ${extra_files[@]+"${!extra_files[@]}"}; do
            tool bytes "${extra_sigs[$i]}" "$work/signature"
            "$ed_openssl" pkeyutl -verify -pubin -inkey "$work/sparkle-public.der" -keyform DER -rawin \
                -in "${extra_files[$i]}" -sigfile "$work/signature" >/dev/null 2>&1 ||
                die "the signature of $(basename "${extra_files[$i]}") does not verify with the apps' public key $sparkle_public_key"
        done
    elif [ "${sign_key[0]}" = --account ]; then
        generate_keys="$(dirname "$sign_update")/generate_keys"
        [ "$("$generate_keys" --account "$sparkle_account" -p 2>/dev/null)" = "$sparkle_public_key" ] ||
            die "the keychain account $sparkle_account does not hold the key whose public key the apps carry"
    else
        die "checking INPUTWEAVE_SPARKLE_KEY_FILE's signatures needs OpenSSL 3 (brew install openssl@3)"
    fi

    say "Writing the feeds and the package repository"
    tool generate "$dist" "$stage" "$version" "$build" "$site_url" "$release_date" "$minimum_macos" \
        "$notes_md" "$pkg" "$pkg_sig" "$msi" "$msi_sig" "$(sha256 "$dist/$msi")" \
        "$deb_amd64" "$work/control-amd64" "$deb_arm64" "$work/control-arm64"
    for i in ${extra_files[@]+"${!extra_files[@]}"}; do
        tool extra-feed "$stage" "${extra_feeds[$i]}" "${extra_files[$i]}" "$version" "$build" "$site_url" \
            "$release_date" "$notes_md" "${extra_sigs[$i]}"
    done

    say "Signing the package repository with $apt_key"
    "${gpg_cmd[@]}" --local-user "$apt_key" --digest-algo SHA512 --clearsign \
        --output "$stage/InRelease" "$stage/Release"
    "${gpg_cmd[@]}" --local-user "$apt_key" --digest-algo SHA512 --armor --detach-sign \
        --output "$stage/Release.gpg" "$stage/Release"
    # Checked with the key the .deb carries, so the repository and the
    # packages' source agree.
    gpg --batch --dearmor <"$repo_dir/packaging/linux/inputweave-archive-key.asc" >"$work/shipped-key.gpg"
    gpgv --keyring "$work/shipped-key.gpg" "$stage/InRelease" 2>/dev/null ||
        die "InRelease does not verify with packaging/linux/inputweave-archive-key.asc"
    gpgv --keyring "$work/shipped-key.gpg" "$stage/Release.gpg" "$stage/Release" 2>/dev/null ||
        die "Release.gpg does not verify with packaging/linux/inputweave-archive-key.asc"

    {
        for file in "${installers[@]}"; do echo "$(sha256 "$dist/$file")  $file"; done
        for file in ${extra_files[@]+"${extra_files[@]}"}; do echo "$(sha256 "$file")  $(basename "$file")"; done
        for file in ${previous_files[@]+"${previous_files[@]}"}; do echo "$(sha256 "$previous_dir/$file")  $file"; done
        for file in "${generated[@]}"; do echo "$(sha256 "$stage/$file")  $file"; done
    } >"$stage/SHA256SUMS"
    echo "$release_date" >"$stage/release-date"
    cp "$key" "$stage/stage.key"
else
    say "Keeping the files $stage already holds"
fi
tool check-feeds "$stage" ${extra_feeds[@]+"${extra_feeds[@]}"} || die "appcast.xml, update-windows.json or an --extra-installer feed does not parse"

# The release's description: the notes and every file's checksum.
{
    cat "$stage/release-notes.md"
    echo
    echo "## Checksums"
    echo
    echo '```'
    cat "$stage/SHA256SUMS"
    echo '```'
} >"$work/description.md"

# Upload order: the installers, the previous release's packages, then the
# feeds and the repository, which name them.
assets=()
for file in "${installers[@]}"; do assets+=("$dist/$file"); done
for file in ${extra_files[@]+"${extra_files[@]}"}; do assets+=("$file"); done
for file in ${previous_files[@]+"${previous_files[@]}"}; do assets+=("$previous_dir/$file"); done
for file in "${generated[@]}" SHA256SUMS; do assets+=("$stage/$file"); done

echo
printf '%-34s %12s  %s\n' FILE BYTES SHA-256
for path in "${assets[@]}"; do
    printf '%-34s %12s  %s\n' "$(basename "$path")" "$(size_of "$path")" "$(sha256 "$path")"
done
echo

if [ "$dry_run" = 1 ]; then
    cp "$work/description.md" "$stage/description.md"
    if [ "$stage" = "$work/stage" ]; then
        keep="${TMPDIR:-/tmp}/inputweave-release-$version-dry-run"
        rm -rf "$keep"
        cp -R "$stage" "$keep"
    else
        keep="$stage"
    fi
    say "Dry run: would create $tag on $github_repo as a draft (tag on $target if it does not exist),"
    echo "    upload the ${#assets[@]} files above, check them on GitHub, then publish it,"
    echo "    as the latest release unless a published one has a higher version."
    echo "    The generated files and the release description are in $keep"
    exit 0
fi

# ------------------------------------------------------------ on GitHub
remote_sha256() {
    # GitHub reports a digest for assets uploaded since mid-2025; without
    # one, the asset is downloaded and hashed.
    d="$(tool releases "$releases" asset "$tag" "$1" digest)"
    case "$d" in
    sha256:*) echo "${d#sha256:}" ;;
    *)
        rm -rf "$work/download"
        gh release download "$tag" --repo "$github_repo" --pattern "$1" --dir "$work/download" >/dev/null
        sha256_fresh "$work/download/$1"
        ;;
    esac
}

# The highest published version other than this one, before publishing.
highest_other="$(tool releases "$releases" highest)"
[ "$highest_other" != "$version" ] || highest_other="$(tool releases "$releases" highest "$version")"

if [ "$release_state" = none ]; then
    say "Creating $tag as a draft"
    gh release create "$tag" --repo "$github_repo" --draft --target "$target" \
        --title "InputWeave $version" --notes-file "$work/description.md" >/dev/null
    refresh_until draft || die "GitHub does not list the draft $tag just made"
elif [ "$release_state" = draft ]; then
    gh release edit "$tag" --repo "$github_repo" --notes-file "$work/description.md" >/dev/null
fi

for path in "${assets[@]}"; do
    name="$(basename "$path")"
    remote_size="$(tool releases "$releases" asset "$tag" "$name" size)"
    if [ -n "$remote_size" ]; then
        if [ "$remote_size" = "$(size_of "$path")" ] && [ "$(remote_sha256 "$name")" = "$(sha256 "$path")" ]; then
            echo "    $name: already there"
            continue
        fi
        [ "$release_state" = draft ] || die "$tag is published with a different $name; it is not replaced"
        echo "    $name: replacing the draft's different copy"
        gh release delete-asset "$tag" "$name" --repo "$github_repo" --yes >/dev/null
    else
        echo "    $name: uploading"
    fi
    gh release upload "$tag" "$path" --repo "$github_repo" >/dev/null
done

say "Checking what GitHub holds"
refresh_until "$release_state" uploaded || die "GitHub does not list every file uploaded to $tag"
for path in "${assets[@]}"; do
    name="$(basename "$path")"
    remote_size="$(tool releases "$releases" asset "$tag" "$name" size)"
    [ "$remote_size" = "$(size_of "$path")" ] || die "GitHub's $name is ${remote_size:-missing}, not $(size_of "$path") bytes"
    [ "$(remote_sha256 "$name")" = "$(sha256 "$path")" ] || die "GitHub's $name has another SHA-256"
done

if [ "$release_state" = draft ]; then
    if [ -z "$highest_other" ] || tool newer "$version" "$highest_other"; then
        say "Publishing $tag as the latest release"
        gh release edit "$tag" --repo "$github_repo" --draft=false --latest >/dev/null
    else
        say "Publishing $tag; v$highest_other stays the latest release"
        gh release edit "$tag" --repo "$github_repo" --draft=false --latest=false >/dev/null
    fi
    refresh_until published || die "GitHub does not list $tag as published"
fi

say "Checking releases/latest"
highest="$(tool releases "$releases" highest)"
[ -n "$highest" ] || die "GitHub lists no published release"
for attempt in 1 2 3 4 5 6; do
    location="$(curl -sS -o /dev/null -w '%{redirect_url}' "$github_latest/appcast.xml" || true)"
    case "$location" in */download/v$highest/appcast.xml) break ;; esac
    sleep 5
done
case "$location" in
*/download/v$highest/appcast.xml) echo "    releases/latest leads to v$highest, the highest version" ;;
*) die "releases/latest/download/appcast.xml leads to '$location', not v$highest, the highest version" ;;
esac
if [ "$highest" = "$version" ]; then
    # Through the site, as the apps and apt read them.
    for name in appcast.xml update-windows.json InRelease; do
        curl -sSfL "$latest_base/$name" -o "$work/latest-$name" || die "$latest_base/$name cannot be read"
        [ "$(sha256_fresh "$work/latest-$name")" = "$(sha256 "$stage/$name")" ] || die "$latest_base serves another $name"
    done
    echo "    $latest_base serves this release's feeds and package index"
fi
say "Published https://github.com/$github_repo/releases/tag/$tag"
