#!/usr/bin/env bash
# Creates a FAT32 disk image (no partition table, "superfloppy") and copies
# the contents of a directory into it, plus the user programs into /bin.
#
# Usage: scripts/mkdisk.sh <image> [size_mib] [source_dir]
set -euo pipefail

IMG="$1"
SIZE_MIB="${2:-64}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${3:-$ROOT/rootfs}"

mkdir -p "$(dirname "$IMG")"
rm -f "$IMG"
truncate -s "${SIZE_MIB}M" "$IMG"
# FAT32 requires at least 65525 clusters: with 64 MiB that means 1 sector (512 bytes) per cluster
mkfs.fat -F 32 -s 1 -n YAK "$IMG" >/dev/null
if [[ -d "$SRC" ]] && compgen -G "$SRC/*" >/dev/null; then
    MTOOLS_SKIP_CHECK=1 mcopy -s -i "$IMG" "$SRC"/* ::/
fi
"$ROOT/scripts/install-user.sh" "$IMG"
