#!/usr/bin/env bash
# Starts YAK from the files of a release (no Rust, no build): needs only
# qemu-system-x86_64 (Fedora/Debian/Ubuntu package qemu-system-x86).
# The disk image is unpacked on the first start and keeps your files.
#
# Usage: ./run-yak.sh [--uefi <OVMF_CODE.fd>] [--headless] [more QEMU options]
#   --uefi      boot through UEFI firmware (e.g. /usr/share/OVMF/OVMF_CODE_4M.fd on
#               Debian/Ubuntu, /usr/share/edk2/ovmf/OVMF_CODE.fd on Fedora); default BIOS
#   --headless  no window: the serial console is your terminal (type there; Ctrl+A X quits)
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ISO="$(ls "$DIR"/yak-*.iso 2>/dev/null | head -1)"
if [[ -z "$ISO" ]]; then
    echo "run-yak.sh: no yak-<version>.iso next to this script" >&2
    exit 1
fi
DISK="${ISO%.iso}-disk.img"
if [[ ! -f "$DISK" ]]; then
    if [[ ! -f "$DISK.gz" ]]; then
        echo "run-yak.sh: $(basename "$DISK").gz is missing" >&2
        exit 1
    fi
    gunzip -k "$DISK.gz"
fi
command -v qemu-system-x86_64 >/dev/null || {
    echo "run-yak.sh: qemu-system-x86_64 not found (package qemu-system-x86)" >&2
    exit 1
}

EXTRA=()
DISPLAY_ARGS=(-serial stdio)
while [[ $# -gt 0 ]]; do
    case "$1" in
        --uefi) EXTRA+=(-drive "if=pflash,format=raw,readonly=on,file=$2"); shift 2 ;;
        --headless) DISPLAY_ARGS=(-display none -serial mon:stdio); shift ;;
        *) EXTRA+=("$1"); shift ;;
    esac
done

# the machine YAK is tested on (README.md, "Supported environment")
exec qemu-system-x86_64 -M q35 -m 256M -smp 4 -cpu max \
    -cdrom "$ISO" -boot d -no-reboot \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -drive "file=$DISK,format=raw,if=none,id=disk0" -device ide-hd,drive=disk0,bus=ide.0 \
    -netdev user,id=net0 -device e1000,netdev=net0 \
    "${DISPLAY_ARGS[@]}" "${EXTRA[@]}"
