#!/bin/sh
# lib.exe stand-in for type-checking only: archives the objects with ar.
out=""
files=""
for a in "$@"; do
  case "$a" in
    -out:*|/OUT:*|/out:*) out="${a#*:}" ;;
    -*) ;;
    *) files="$files $a" ;;
  esac
done
exec /usr/bin/ar crs "$out" $files
