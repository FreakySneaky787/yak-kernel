#!/usr/bin/env bash
# After a kernel wrote to build/disk.img: is it still a clean FAT32 volume, and do the Linux tools read what the
# kernel wrote? Run by `make test` once QEMU has ended.
#   ./check-disk.sh tests   the files the unit tests leave behind (fat32.rs, files_for_the_host_to_check)
#   ./check-disk.sh shell   the file the boot check types into the shell
set -euo pipefail
cd "$(dirname "$0")"
IMG=build/disk.img
export MTOOLS_SKIP_CHECK=1

# -n: only look, change nothing. Exit status 0 = no problem found.
if ! fsck.fat -n "$IMG" > build/fsck.log 2>&1; then
    echo "[disk] FAIL - fsck.fat found problems on $IMG:"
    cat build/fsck.log
    exit 1
fi

# same <file on the volume> <file with the expected bytes>
same() {
    if ! mtype -i "$IMG" "::$1" | cmp -s - "$2"; then
        echo "[disk] FAIL - mtype ::$1 differs from what the kernel should have written"
        exit 1
    fi
}

EXPECTED=build/expected-file
case "$1" in
tests)
    printf 'Written by YAK, read by mtools.\nAnd appended.\n' > "$EXPECTED"
    same /kept.txt "$EXPECTED"
    python3 -I -c 'import sys; sys.stdout.buffer.write(bytes(i % 251 for i in range(100000)))' > "$EXPECTED"
    same /keptdir/big.bin "$EXPECTED"
    ;;
shell)
    printf 'hello disk\nand more\n' > "$EXPECTED"
    same /hi.txt "$EXPECTED"
    ;;
esac
echo "[disk] PASS - fsck.fat finds no problem, and mtools reads what the kernel wrote ($1)"
