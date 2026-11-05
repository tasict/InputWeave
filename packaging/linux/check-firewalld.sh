#!/bin/sh
# Reads the package's firewalld service with firewalld's own tool, in a
# throwaway container of the native build image, which has systemd
# installed already: firewalld pulls systemd, whose postinst fails in a
# fresh image on older host kernels such as Linux 5.10.
# `scripts/linux-container.sh check-deb`
# runs it with this folder at /pkg.
set -eu
apt-get update -qq >/dev/null 2>&1
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends firewalld >/dev/null 2>&1
install -m644 /pkg/inputweave.firewalld.xml /usr/lib/firewalld/services/inputweave.xml
firewall-offline-cmd --info-service=inputweave
