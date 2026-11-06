#!/usr/bin/env bash
# Makes the test disk of chapters 18 and 19: a 64 MiB FAT32 volume (no partition table, one 512-byte sector per cluster)
# with known files - made by the Linux tools mkfs.fat and mtools, so the kernel reads what other systems write.
#   ./mkdisk.sh build/disk.img
set -euo pipefail
IMG="$1"
FILES="$(dirname "$IMG")/disk-files"
rm -rf "$IMG" "$FILES"
mkdir -p "$FILES"
export MTOOLS_SKIP_CHECK=1 # mtools would complain that the volume is "too small" for FAT32

mkfs.fat -F 32 -s 1 -n YAKDISK -C "$IMG" $((64 * 1024)) >/dev/null
printf 'Hello from the disk!\n' > "$FILES/hello.txt"
printf 'This file has a name longer than 8.3.\n' > "$FILES/a long file name.txt"
printf 'Notes, one directory down.\n' > "$FILES/notes.txt"
printf 'The end of the path.\n' > "$FILES/end.txt"
printf 'A long name for the removal test.\n' > "$FILES/to be removed.txt"
: > "$FILES/empty.txt"
# 100 000 bytes, byte i = i % 253: 196 clusters, and no two neighbouring clusters alike
python3 -I -c 'import sys; sys.stdout.buffer.write(bytes(i % 253 for i in range(100000)))' > "$FILES/big.bin"

mcopy -i "$IMG" "$FILES/hello.txt" "$FILES/a long file name.txt" "$FILES/empty.txt" "$FILES/big.bin" ::/
mmd -i "$IMG" ::/docs ::/docs/deeper ::/many ::/trash
mcopy -i "$IMG" "$FILES/notes.txt" ::/docs/
mcopy -i "$IMG" "$FILES/end.txt" ::/docs/deeper/
mcopy -i "$IMG" "$FILES/to be removed.txt" ::/trash/ # chapter 18 deletes it
# 40 files with long names: their entries need more than one cluster of the directory
for i in $(seq 1 40); do
    printf 'file %d\n' "$i" > "$FILES/file number $i.txt"
done
mcopy -i "$IMG" "$FILES"/file\ number\ *.txt ::/many/
