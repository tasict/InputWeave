#!/bin/sh
# Builds InputWeave's Debian package for one architecture:
# the agent and ctl
# (/usr/bin/inputweave, the root workspace) and the GTK app
# (/usr/bin/inputweave-app, apps/linux), their systemd user unit, desktop
# entries, D-Bus service, icons, metainfo, man pages and the package
# repository's source. Writes
# dist/inputweave_<version>_<arch>.deb and runs lintian on it.
#
#   packaging/linux/build-deb.sh amd64|arm64
#
# Runs in the Linux containers (scripts/linux-container/README.md): amd64 in
# inputweave-linux, arm64 in inputweave-linux-arm64, which has the arm64
# GTK and libadwaita the app links against and dpkg-shlibdeps reads.
# `scripts/linux-container.sh deb` builds both. CARGO_TARGET_DIR is the root
# workspace's target directory; INPUTWEAVE_APP_TARGET_DIR the app's (by
# default $CARGO_TARGET_DIR/app, as the containers' convention has it).
set -eu
umask 022

arch=${1:-}
case $arch in
amd64) triple=x86_64-unknown-linux-gnu ;;
arm64) triple=aarch64-unknown-linux-gnu ;;
*)
    sed -n '2,16p' "$0" >&2
    exit 2
    ;;
esac

here=$(cd "$(dirname "$0")/../.." && pwd)
pkg=$here/packaging/linux
target=${CARGO_TARGET_DIR:-$here/target}
app_target=${INPUTWEAVE_APP_TARGET_DIR:-$target/app}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
[ -n "$version" ] || { echo "cannot read the workspace version from Cargo.toml" >&2; exit 1; }
# The app's workspace has its own version, which must be the same: the app
# judges the agent by it when the agent reports no build digest.
app_version=$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' \
    "$here/apps/linux/Cargo.toml" | head -1)
[ "$app_version" = "$version" ] || {
    echo "apps/linux/Cargo.toml has version ${app_version:-none}, the workspace $version" >&2
    exit 1
}
grep -q "<release version=\"$version\"" "$pkg/cc.ctsite.inputweave.metainfo.xml" || {
    echo "the metainfo has no release $version" >&2
    exit 1
}

# Symbols go: the package carries no debug information.
export CARGO_PROFILE_RELEASE_STRIP=symbols
echo "building inputweave $version for $arch"
cargo build --manifest-path "$here/Cargo.toml" --release --locked \
    --target "$triple" --target-dir "$target" -p inputweave
cargo build --manifest-path "$here/apps/linux/Cargo.toml" --release --locked \
    --target "$triple" --target-dir "$app_target" -p inputweave-app

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
root=$work/src/debian/inputweave
doc=$root/usr/share/doc/inputweave

install -Dm755 "$target/$triple/release/inputweave" "$root/usr/bin/inputweave"
install -Dm755 "$app_target/$triple/release/inputweave-app" "$root/usr/bin/inputweave-app"
install -Dm755 "$pkg/start-agent" "$root/usr/libexec/inputweave/start-agent"
install -Dm644 "$pkg/app-cc.ctsite.inputweave@.service" \
    "$root/usr/lib/systemd/user/app-cc.ctsite.inputweave@.service"
install -Dm644 "$pkg/app-cc.ctsite.inputweave.service" \
    "$root/usr/lib/systemd/user/app-cc.ctsite.inputweave.service"
install -Dm644 "$pkg/cc.ctsite.inputweave.desktop" \
    "$root/usr/share/applications/cc.ctsite.inputweave.desktop"
install -Dm644 "$pkg/cc.ctsite.inputweave-agent.desktop" \
    "$root/etc/xdg/autostart/cc.ctsite.inputweave-agent.desktop"
# The firewall's rules for InputWeave, which the package does not apply:
# `sudo ufw allow InputWeave`, or firewalld's service.
install -Dm644 "$pkg/inputweave.ufw" "$root/etc/ufw/applications.d/inputweave"
install -Dm644 "$pkg/inputweave.firewalld.xml" \
    "$root/usr/lib/firewalld/services/inputweave.xml"
install -Dm644 "$pkg/cc.ctsite.inputweave.service" \
    "$root/usr/share/dbus-1/services/cc.ctsite.inputweave.service"
install -Dm644 "$pkg/cc.ctsite.inputweave.metainfo.xml" \
    "$root/usr/share/metainfo/cc.ctsite.inputweave.metainfo.xml"

# The package repository on InputWeave's GitHub Releases, so later
# versions arrive with the system's software updates. Its signing key is
# embedded in the source file (apt 2.3.10 and later read Signed-By that
# way) instead of being a keyring of its own under /usr/share/keyrings:
# the source file is a conffile, kept after `apt remove` until purged,
# and a keyring under /usr would go with the package, leaving apt to
# fail on a source whose key is gone.
key=$pkg/inputweave-archive-key.asc
grep -q '^-----BEGIN PGP PUBLIC KEY BLOCK-----$' "$key" || { echo "$key is not a public key block" >&2; exit 1; }
! grep -q PRIVATE "$key" || { echo "$key holds a private key" >&2; exit 1; }
mkdir -p "$root/etc/apt/sources.list.d"
{
    echo "# InputWeave's package repository, at the website's address for the latest"
    echo "# release (which leads to where the releases are hosted): new versions"
    echo "# of InputWeave come with the system's software updates. To stop that,"
    echo "# add a line \"Enabled: no\"; upgrades keep the change."
    echo "Types: deb"
    echo "URIs: https://inputweave.ctsite.cc/latest/"
    echo "Suites: ./"
    echo "Signed-By:"
    sed -e 's/^$/./' -e 's/^/ /' "$key"
} > "$root/etc/apt/sources.list.d/inputweave.sources"
chmod 644 "$root/etc/apt/sources.list.d/inputweave.sources"
# lintian's package-installs-apt-sources is an error because a package in
# Debian must not add sources. This one is distributed by its authors
# outside Debian, and the source is how its updates arrive, as with other
# vendors' packages; a conffile keeps the administrator's changes and goes
# on purge, which a source written by postinst would have to reimplement.
mkdir -p "$root/usr/share/lintian/overrides"
{
    echo "# Distributed by its authors outside Debian; this source is how its updates arrive."
    echo "inputweave: package-installs-apt-sources [etc/apt/sources.list.d/inputweave.sources]"
} > "$root/usr/share/lintian/overrides/inputweave"
chmod 644 "$root/usr/share/lintian/overrides/inputweave"

# The app icon at each size, as drawn for the macOS app, and the SVG.
icons=$here/apps/macos/InputWeave/Assets.xcassets/AppIcon.appiconset
for pair in 16:icon_16x16 32:icon_32x32 64:icon_32x32@2x 128:icon_128x128 \
    256:icon_256x256 512:icon_512x512; do
    size=${pair%%:*}
    install -Dm644 "$icons/${pair#*:}.png" \
        "$root/usr/share/icons/hicolor/${size}x$size/apps/cc.ctsite.inputweave.png"
done
install -Dm644 "$here/docs/brand/app-icon.svg" \
    "$root/usr/share/icons/hicolor/scalable/apps/cc.ctsite.inputweave.svg"

mkdir -p "$root/usr/share/man/man1"
for page in inputweave inputweave-app; do
    gzip -9n < "$pkg/$page.1" > "$root/usr/share/man/man1/$page.1.gz"
done

install -Dm644 "$pkg/debian/copyright" "$doc/copyright"
# The crates linked into the two programs, for the copyright file: each
# once (a crate seen again is marked "(*)"), without the workspaces' own.
: > "$work/crates"
for manifest in "$here/Cargo.toml" "$here/apps/linux/Cargo.toml"; do
    cargo tree --manifest-path "$manifest" --locked --target "$triple" \
        -e normal --prefix none --format '{p} {l}' >> "$work/crates" || {
        echo "cargo tree failed for $manifest" >&2
        exit 1
    }
done
{
    echo "Rust crates linked into /usr/bin/inputweave and /usr/bin/inputweave-app"
    echo "(name, version, license), from cargo tree for $triple:"
    echo
    sed 's/ (\*)$//' "$work/crates" | grep -v '(/' | sort -u
} > "$doc/crates.txt"
chmod 644 "$doc/crates.txt"
# A native package's changelog.
when=$(git -C "$here" log -1 --format=%ct 2>/dev/null || date +%s)
{
    echo "inputweave ($version) unstable; urgency=medium"
    echo
    echo "  * InputWeave $version for Linux."
    echo
    echo " -- InputWeave contributors <support@ctsite.cc>  $(date -u -R -d "@$when")"
} | gzip -9n > "$doc/changelog.gz"
chmod 644 "$doc/changelog.gz"

# Shared library dependencies, as the libraries' own packages state them.
printf 'Source: inputweave\n\nPackage: inputweave\nArchitecture: any\n' > "$work/src/debian/control"
depends=$(cd "$work/src" && DEB_HOST_ARCH=$arch dpkg-shlibdeps -O \
    -edebian/inputweave/usr/bin/inputweave -edebian/inputweave/usr/bin/inputweave-app |
    sed -n 's/^shlibs:Depends=//p')
[ -n "$depends" ] || { echo "dpkg-shlibdeps found no dependencies" >&2; exit 1; }

mkdir -p "$root/DEBIAN"
for script in postinst postrm; do
    install -m755 "$pkg/debian/$script" "$root/DEBIAN/$script"
done
printf '%s\n' /etc/xdg/autostart/cc.ctsite.inputweave-agent.desktop \
    /etc/ufw/applications.d/inputweave /etc/apt/sources.list.d/inputweave.sources \
    > "$root/DEBIAN/conffiles"
size=$(du -sk --exclude=DEBIAN "$root" | cut -f1)
cat > "$root/DEBIAN/control" <<EOF
Package: inputweave
Version: $version
Architecture: $arch
Maintainer: InputWeave contributors <support@ctsite.cc>
Installed-Size: $size
Depends: $depends, init-system-helpers (>= 1.66)
Recommends: xdg-desktop-portal (>= 1.18), xdg-desktop-portal-gnome | xdg-desktop-portal-kde
Suggests: gnome-shell-extension-appindicator
Section: utils
Priority: optional
Description: share one keyboard and mouse across computers
 InputWeave lets the computers on a desk share one keyboard and mouse:
 the pointer moves off the edge of one screen onto the computer next to
 it. It also carries the clipboard and sends files between them, with
 InputWeave on Linux, macOS and Windows.
 .
 The background service runs as each user's systemd service and starts
 with the desktop session. On Wayland it uses the desktop's remote
 desktop and input capture portals, which ask the user first.
EOF
(cd "$root" && find . -type f ! -path './DEBIAN/*' -printf '%P\0' | sort -z | xargs -0 md5sum) \
    > "$root/DEBIAN/md5sums"

mkdir -p "$here/dist"
deb=$here/dist/inputweave_${version}_$arch.deb
dpkg-deb --root-owner-group -Zxz --build "$root" "$deb"
echo "lintian:"
lintian --fail-on error --display-level '>=warning' "$deb"
echo "built $deb"
