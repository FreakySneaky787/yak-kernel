#!/usr/bin/env bash
# Packs a kernel ELF together with Limine into an ISO image that boots with
# BIOS and UEFI (used by run-qemu.sh and release.sh).
#
# Usage: scripts/mkiso.sh <kernel-elf> <iso>
set -euo pipefail

ELF="$1"
ISO="$2"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIMINE="$ROOT/vendor/limine"

if [[ ! -f "$LIMINE/limine-bios-cd.bin" ]]; then
    echo "Limine is missing: run make limine" >&2
    exit 1
fi

ISO_ROOT="$(dirname "$ISO")/iso_root-$(basename "$ISO" .iso)"
rm -rf "$ISO_ROOT"
mkdir -p "$ISO_ROOT/boot/limine" "$ISO_ROOT/EFI/BOOT"
cp "$ELF" "$ISO_ROOT/boot/kernel"
cp "$ROOT/limine.conf" "$ISO_ROOT/boot/limine/"
cp "$LIMINE"/limine-bios.sys "$LIMINE"/limine-bios-cd.bin "$LIMINE"/limine-uefi-cd.bin \
   "$ISO_ROOT/boot/limine/"
cp "$LIMINE/BOOTX64.EFI" "$ISO_ROOT/EFI/BOOT/"

xorriso -as mkisofs -R -r -J \
    -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
    -hfsplus -apm-block-size 2048 \
    --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image \
    --protective-msdos-label \
    "$ISO_ROOT" -o "$ISO" 2>/dev/null

# Optional (`make limine-tool`, needs gcc): also make the ISO bootable as a hard disk / USB image
if [[ -x "$LIMINE/limine" ]]; then
    "$LIMINE/limine" bios-install "$ISO" >/dev/null 2>&1
fi
