#!/usr/bin/env bash
# Cargo runner (see kernel/.cargo/config.toml): packs the kernel ELF together with Limine into a bootable ISO
# and starts QEMU. Three modes:
#   run    `cargo run`: QEMU window, serial port on this terminal
#   test   `cargo test`: headless; the test kernel ends QEMU through the isa-debug-exit device,
#          whose exit status says whether all tests passed
#   check  KERNEL_MODE=check: headless; passes when the serial output contains every line of expected.txt
#          (or of the file named by EXPECTED)
# KERNEL_CMDLINE (optional) becomes the kernel's command line, e.g. KERNEL_CMDLINE=fault=page
# KEYS (optional, check mode): keys to type once the serial output contains the line KEYS_AFTER - QEMU `sendkey`
#   names, separated by spaces, e.g. KEYS="shift-h i ret"
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
if [[ -n "${KERNEL_CMDLINE:-}" ]]; then
    echo "    cmdline: $KERNEL_CMDLINE" >> "$ISO_ROOT/boot/limine/limine.conf"
fi
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
# -cpu qemu64,-apic: a CPU without local APIC, so the PIC's interrupts go straight to the CPU (chapter 6)
QEMU=(qemu-system-x86_64 -M q35 -cpu qemu64,-apic -m 128M -cdrom "$ISO" -boot d -no-reboot
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
    EXPECTED="${EXPECTED:-expected.txt}"
    # QEMU's monitor reads its commands from a FIFO, so this script can type on the emulated keyboard;
    # <> opens it for reading and writing at once, which does not wait for a reader on the other side
    rm -f build/monitor.fifo
    mkfifo build/monitor.fifo
    exec 3<>build/monitor.fifo
    # QEMU's own messages, e.g. "terminating on signal 15" when we stop it, go to build/qemu.log
    "${QEMU[@]}" -display none -serial "file:$LOG" -monitor stdio <&3 >build/monitor.log 2>build/qemu.log &
    QEMU_PID=$!
    trap 'kill $QEMU_PID 2>/dev/null || true' EXIT
    all_lines_seen() {
        while IFS= read -r line; do
            grep -qF -- "$line" "$LOG" 2>/dev/null || return 1
        done < "$EXPECTED"
    }
    qemu_running() { kill -0 "$QEMU_PID" 2>/dev/null; }
    type_keys() {
        for key in $KEYS; do
            echo "sendkey $key" >&3
            sleep 0.2 # sendkey holds a key down for 100 ms
        done
    }
    keys_typed=no
    for _ in $(seq 1 "$TIMEOUT"); do
        sleep 1
        if [[ -n "${KEYS:-}" && $keys_typed == no ]] && grep -qF -- "$KEYS_AFTER" "$LOG" 2>/dev/null; then
            type_keys
            keys_typed=yes
        fi
        if all_lines_seen || ! qemu_running; then break; fi
    done
    if all_lines_seen; then
        echo "[check] PASS - the serial output contains every line of $EXPECTED"
        exit 0
    fi
    # with -no-reboot, QEMU ends by itself when the machine resets - after a triple fault
    qemu_running || echo "[check] QEMU ended by itself: the machine reset (a triple fault?)"
    echo "[check] FAIL - expected every line of $EXPECTED:"
    cat "$EXPECTED"
    echo "[check] the kernel printed (build/serial.log):"
    cat "$LOG"
    [[ -z "${KEYS:-}" || $keys_typed == yes ]] || echo "[check] the keys were not typed: no line \"$KEYS_AFTER\" came"
    if [[ -s build/qemu.log ]]; then echo "QEMU said (build/qemu.log):"; cat build/qemu.log; fi
    exit 1
    ;;
esac
