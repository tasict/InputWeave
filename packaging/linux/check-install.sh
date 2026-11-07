#!/bin/sh
# Installs the amd64 package into a fresh Ubuntu or Debian container and
# checks it: dependencies, both programs, the unit enabled for every user,
# the desktop entries and metainfo, then remove, reinstall and purge.
# `scripts/linux-container.sh check-deb` runs it in ubuntu:24.04 and
# debian:trixie, with dist/ at /deb and this folder at /pkg.
#
#   sh /pkg/check-install.sh inputweave_<version>_amd64.deb
#
# dbus-x11 goes first, so GTK's dconf takes it rather than pull systemd,
# whose postinst fails on older host kernels such as Linux 5.10; real
# desktops have systemd.
set -u
deb=/deb/${1:?the package file name}
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq --no-install-recommends dbus-x11 >/dev/null 2>&1
echo "== recommends"
apt-get install -s "$deb" | grep -E "^Inst (inputweave|xdg-desktop-portal)|^E:"
echo "== install"
apt-get install -y -qq --no-install-recommends "$deb" >/tmp/apt.log 2>&1
echo "apt: $?"
grep -E "^E:|dpkg: error" /tmp/apt.log
inputweave --version
inputweave-app --version
echo "== enabled for every user"
ls /etc/systemd/user/graphical-session.target.wants/
echo "== desktop entries and metainfo"
apt-get install -y -qq --no-install-recommends appstream desktop-file-utils >/dev/null 2>&1
appstreamcli validate --no-net /usr/share/metainfo/cc.ctsite.inputweave.metainfo.xml
desktop-file-validate /usr/share/applications/cc.ctsite.inputweave.desktop \
    /etc/xdg/autostart/cc.ctsite.inputweave-agent.desktop && echo "desktop entries: valid"
# Dolphin's service menu is KDE's kind of desktop file (Type=Service), which
# desktop-file-validate does not take; it is only checked to be there, the
# same in both places, and to run the app.
cmp /usr/share/kio/servicemenus/cc.ctsite.inputweave-send.desktop \
    /usr/share/kservices5/ServiceMenus/cc.ctsite.inputweave-send.desktop &&
    grep -x 'Exec=inputweave-app --send %F' /usr/share/kio/servicemenus/cc.ctsite.inputweave-send.desktop
echo "== firewall profiles"
# ufw reads its profile as installed; the package applies no rule.
# firewalld pulls systemd, whose postinst fails on such kernels, so its
# service is only checked to be there; check-firewalld.sh reads it.
apt-get install -y -qq --no-install-recommends ufw >/dev/null 2>&1
ufw app info InputWeave
grep -c InputWeave /etc/ufw/user.rules 2>/dev/null || echo "rules for InputWeave: none"
ls /usr/lib/firewalld/services/inputweave.xml
echo "== package source"
# apt reads the source; --print-uris lists what an update would fetch
# without fetching it.
apt-get --print-uris update 2>&1 | grep -E "inputweave\.ctsite\.cc|^E:|^W:" | sed "s/ .*//"
# The key embedded in Signed-By must parse and be the repository's
# (packaging/linux/inputweave-archive-key.asc); a broken embedding fails.
apt-get install -y -qq --no-install-recommends gpg >/dev/null 2>&1
awk '/^Signed-By:/ { on = 1; next } on && /^ / { sub(/^ /, ""); if ($0 == ".") $0 = ""; print; next } on { exit }' \
    /etc/apt/sources.list.d/inputweave.sources > /tmp/embedded-key.asc
fingerprint=$(gpg --batch --show-keys --with-colons /tmp/embedded-key.asc 2>/dev/null | awk -F: '/^fpr/ { print $10; exit }')
if [ "$fingerprint" = DA65B795D2F3984F0B305013C3AF4DF76FC87E0E ]; then
    echo "embedded key: $fingerprint"
else
    echo "embedded key: wrong or unreadable (${fingerprint:-none})"
    exit 1
fi
echo "== remove"
apt-get remove -y -qq inputweave >/dev/null 2>&1
echo "apt: $?"
ls -l /etc/systemd/user/ /etc/systemd/user/graphical-session.target.wants/ | grep inputweave
ls /usr/bin/inputweave* 2>&1
# The ufw profile and the package source are conffiles: they stay until
# purged, and apt keeps reading the source, key and all.
ls /etc/ufw/applications.d/inputweave /usr/lib/firewalld/services/inputweave.xml \
    /etc/apt/sources.list.d/inputweave.sources 2>&1
apt-get --print-uris update 2>&1 | grep -cE "^E:|^W:" | sed "s/^/apt errors and warnings after remove: /"
echo "== reinstall"
apt-get install -y -qq --no-install-recommends "$deb" >/dev/null 2>&1
echo "apt: $?"
ls -l /etc/systemd/user/ /etc/systemd/user/graphical-session.target.wants/ | grep inputweave
echo "== purge"
apt-get purge -y -qq inputweave >/dev/null 2>&1
echo "apt: $?"
echo "left behind: $(find /etc /usr /var/lib/systemd -name '*inputweave*' 2>/dev/null | wc -l)"
ls /etc/ufw/applications.d/inputweave /etc/apt/sources.list.d/inputweave.sources 2>&1
# The checks print what they find; reaching the end is success, so the
# last ls's status (the files are expected to be gone) is not the script's.
exit 0
