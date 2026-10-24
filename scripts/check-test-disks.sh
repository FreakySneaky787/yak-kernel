#!/usr/bin/env bash
# Runs after `cargo test`: validates the disk images the kernel tests wrote to
# with independent Linux tools.
#  - fsck.fat -n: the FAT32 structures must be consistent (no errors at all)
#  - mtools: files written by the kernel must have exactly the expected content
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/build/test-disks"
export MTOOLS_SKIP_CHECK=1
fail=0

for img in "$DIR"/*.img; do
    if out=$(fsck.fat -n "$img" 2>&1); then
        echo "[disk-check] fsck ok: $(basename "$img")"
    else
        echo "[disk-check] FAIL fsck: $(basename "$img")"; echo "$out"; fail=1
    fi
done

# The library unit tests write these files (see kernel/src/fs/mod.rs)
lib_img=$(ls -t "$DIR"/kernel-*.img 2>/dev/null | head -1)
if [[ -n "$lib_img" ]]; then
    got=$(mtype -i "$lib_img" "::/from kernel/written by KernelTry.txt")
    if [[ "$got" == $'persistent data\nsecond line' ]]; then
        echo "[disk-check] mtools sees the kernel's text file"
    else
        echo "[disk-check] FAIL text file content: '$got'"; fail=1
    fi
    if mcopy -i "$lib_img" ::/big.bin - | python3 -I -c '
import sys
data = sys.stdin.buffer.read()
expected = bytes(i % 251 for i in range(300_000))
sys.exit(0 if data == expected else 1)'; then
        echo "[disk-check] mtools reads big.bin (300000 bytes) correctly"
    else
        echo "[disk-check] FAIL big.bin differs"; fail=1
    fi
else
    echo "[disk-check] FAIL no library test disk found"; fail=1
fi
# The userspace integration test copies a file with the user program `cp`
user_img=$(ls -t "$DIR"/userspace-*.img 2>/dev/null | head -1)
if [[ -n "$user_img" ]] && cmp -s <(mtype -i "$user_img" ::/copied.txt) <(mtype -i "$user_img" ::/docs/hello.txt); then
    echo "[disk-check] mtools sees the file copied by /bin/cp"
else
    echo "[disk-check] FAIL /copied.txt from the userspace test"; fail=1
fi
exit $fail
