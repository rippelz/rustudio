#!/bin/sh
# Usage: tools/linux/package.sh BINARY OUTPUT.tar.gz
# Packages an already-built Linux executable; does not invoke Cargo.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
[ "$#" = 2 ] || { echo "usage: $0 BINARY OUTPUT.tar.gz" >&2; exit 1; }
[ -x "$1" ] || { echo "not executable: $1" >&2; exit 1; }
temp=$(mktemp -d)
stage="$temp/rustudio-linux-x86_64"
mkdir -p "$stage"
trap 'rm -rf "$temp"' EXIT HUP INT TERM
install -m 755 "$1" "$stage/rustudio"
install -m 755 "$repo/install.sh" "$stage/install.sh"
cp "$repo/api-dump.json" "$repo/README.md" "$stage/"
for license in "$repo"/LICENSE*; do
    [ ! -f "$license" ] || cp "$license" "$stage/"
done
mkdir -p "$stage/assets/icons" "$stage/assets/linux"
cp "$repo"/assets/icons/rustudio-*.png "$repo/assets/icons/rustudio.svg" "$stage/assets/icons/"
cp "$repo/assets/linux/rustudio.desktop" "$stage/assets/linux/"
mkdir -p "$(dirname "$2")"
tar -czf "$2" -C "$temp" rustudio-linux-x86_64
