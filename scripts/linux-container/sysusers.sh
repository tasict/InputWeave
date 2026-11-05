#!/bin/sh
# Stands in for systemd-sysusers in the container: package scripts call it
# to create their system users and groups, and the real one fails on
# Linux 5.10 host kernels (see the Dockerfile). Only the u and g lines of sysusers.d
# files are honoured, with shadow's tools.
files=
for arg in "$@"; do
    case $arg in
    -*) ;;
    /*) files="$files $arg" ;;
    *) files="$files /usr/lib/sysusers.d/$arg" ;;
    esac
done
[ -n "$files" ] || files=$(ls /usr/lib/sysusers.d/*.conf 2>/dev/null)
for f in $files; do
    [ -f "$f" ] || continue
    grep -E '^[ug][[:space:]]' "$f" | while read -r type name _; do
        case $type in
        g) getent group "$name" >/dev/null || groupadd -r "$name" ;;
        u)
            getent passwd "$name" >/dev/null && continue
            getent group "$name" >/dev/null || groupadd -r "$name"
            useradd -r -M -g "$name" -d /nonexistent -s /usr/sbin/nologin "$name"
            ;;
        esac
    done
done
exit 0
