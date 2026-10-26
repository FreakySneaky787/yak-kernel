#!/usr/bin/env bash
# Packs the kernel ELF together with Limine into a bootable ISO and starts QEMU.
#   run-qemu.sh <kernel-elf> run    QEMU window, serial port on this terminal
#   run-qemu.sh <kernel-elf> test   headless; passes when the serial output contains every line of expected.txt
set -euo pipefail

ELF="$1"
MODE="${2:-run}"
LIMINE="${LIMINE_DIR:-vendor/limine}"
ISO_ROOT=build/iso_root
ISO=build/kernel.iso

rm -rf "$ISO_ROOT"
mkdir -p "$ISO_ROOT/boot/limine" "$ISO_ROOT/EFI/BOOT"
cp "$ELF" "$ISO_ROOT/boot/kernel"
cp limine.conf "$ISO_ROOT/boot/limine/"
cp "$LIMINE"/limine-bios.sys "$LIMINE"/limine-bios-cd.bin "$LIMINE"/limine-uefi-cd.bin "$ISO_ROOT/boot/limine/"
cp "$LIMINE/BOOTX64.EFI" "$ISO_ROOT/EFI/BOOT/"

# One ISO that boots with BIOS (El Torito) and UEFI
xorriso -as mkisofs -R -r -J \
    -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
    -hfsplus -apm-block-size 2048 \
    --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image \
    --protective-msdos-label \
    "$ISO_ROOT" -o "$ISO" 2>/dev/null

QEMU=(qemu-system-x86_64 -M q35 -m 128M -cdrom "$ISO" -boot d -no-reboot)

if [[ "$MODE" == run ]]; then
    exec "${QEMU[@]}" -serial stdio
fi

# test: the kernel halts when it is done, so we watch its output and stop QEMU ourselves
LOG=build/serial.log
"${QEMU[@]}" -display none -serial "file:$LOG" &
QEMU_PID=$!
trap 'kill $QEMU_PID 2>/dev/null || true' EXIT

all_lines_seen() {
    while IFS= read -r line; do
        grep -qF -- "$line" "$LOG" 2>/dev/null || return 1
    done < expected.txt
}

for _ in $(seq 1 "${KERNEL_TEST_TIMEOUT:-30}"); do
    sleep 1
    if all_lines_seen; then
        echo "[test] PASS - the serial output contains every line of expected.txt"
        exit 0
    fi
done
echo "[test] FAIL - expected every line of expected.txt:"
cat expected.txt
echo "[test] the kernel printed (build/serial.log):"
cat "$LOG"
exit 1
