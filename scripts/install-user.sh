#!/usr/bin/env bash
# Copies the built user programs (build/user/bin, see `make user`) into /bin
# of a FAT32 disk image, replacing older versions. Other files stay untouched.
#
# Usage: scripts/install-user.sh <image>
set -euo pipefail

IMG="$1"
BIN="$(cd "$(dirname "$0")/.." && pwd)/build/user/bin"
export MTOOLS_SKIP_CHECK=1

if ! compgen -G "$BIN/*" >/dev/null; then
    exit 0 # nothing built yet
fi
mmd -i "$IMG" ::/bin 2>/dev/null || true # exists already
mcopy -o -i "$IMG" "$BIN"/* ::/bin/
