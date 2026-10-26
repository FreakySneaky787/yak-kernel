#!/usr/bin/env bash
# Cargo runner (see kernel/.cargo/config.toml): packs the kernel ELF together with Limine into a bootable ISO
# and starts QEMU. Three modes:
#   run    `cargo run`: QEMU window, serial port on this terminal
#   test   `cargo test`: headless; the test kernel ends QEMU through the isa-debug-exit device,
#          whose exit status says whether all tests passed
#   check  KERNEL_MODE=check: headless; passes when the serial output contains every line of expected.txt
set -euo pipefail

ELF="$(realpath "$1")"
cd "$(dirname "$0")" # the chapter folder: limine.conf, expected.txt and build/ are here
LIMINE="${LIMINE_DIR:-vendor/limine}"

# `cargo test` builds the test kernel as kernel-<16 hex digits>, `cargo run` the normal one as kernel
if [[ -z "${KERNEL_MODE:-}" ]]; then
    if [[ "$(basename "$ELF")" =~ -[0-9a-f]{16}$ ]]; then KERNEL_MODE=test; else KERNEL_MODE=run; fi
fi

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

# isa-debug-exit: a write of value v to port 0xF4 ends QEMU with exit status (v << 1) | 1
QEMU=(qemu-system-x86_64 -M q35 -m 128M -cdrom "$ISO" -boot d -no-reboot
      -device isa-debug-exit,iobase=0xf4,iosize=0x04)
TIMEOUT="${KERNEL_TEST_TIMEOUT:-30}"

case "$KERNEL_MODE" in
run)
    exec "${QEMU[@]}" -serial stdio
    ;;
test)
    set +e
    timeout --foreground "$TIMEOUT" "${QEMU[@]}" -display none -serial stdio
    code=$?
    set -e
    case $code in
        33)  echo "[test] PASS"; exit 0 ;;  # 0x10: all tests passed
        124) echo "[test] FAIL: timeout after ${TIMEOUT}s - a test hangs?"; exit 1 ;;
        *)   echo "[test] FAIL: QEMU exit status $code"; exit 1 ;;  # 35 = 0x11: a test failed
    esac
    ;;
check)
    # the normal kernel halts when it is done, so we watch its output and stop QEMU ourselves
    LOG=build/serial.log
    "${QEMU[@]}" -display none -serial "file:$LOG" &
    QEMU_PID=$!
    trap 'kill $QEMU_PID 2>/dev/null || true' EXIT
    all_lines_seen() {
        while IFS= read -r line; do
            grep -qF -- "$line" "$LOG" 2>/dev/null || return 1
        done < expected.txt
    }
    for _ in $(seq 1 "$TIMEOUT"); do
        sleep 1
        if all_lines_seen; then
            echo "[check] PASS - the serial output contains every line of expected.txt"
            exit 0
        fi
    done
    echo "[check] FAIL - expected every line of expected.txt:"
    cat expected.txt
    echo "[check] the kernel printed (build/serial.log):"
    cat "$LOG"
    exit 1
    ;;
esac
