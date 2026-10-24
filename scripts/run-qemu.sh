#!/usr/bin/env bash
# Cargo runner: receives the path of the built kernel ELF, packs it together with
# Limine into a bootable ISO and starts QEMU.
#
# Modes (via environment variables):
#   KERNEL_MODE=run    (default for `cargo run`) window + serial on stdio
#   KERNEL_MODE=test   (automatic for `cargo test` binaries, see below)
#                      headless, timeout, isa-debug-exit status is evaluated
#   KERNEL_DEBUG=1     QEMU waits for gdb (-s -S)
#   KERNEL_DISK=<img>  use this disk image instead of the default one
set -euo pipefail

ELF="$1"; shift || true
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIMINE="$ROOT/vendor/limine"
BUILD="$ROOT/build"
TIMEOUT="${KERNEL_TEST_TIMEOUT:-30}"

# Test binaries built by `cargo test` carry a hash in their name (kernel-<16 hex>),
# the `cargo run` binary is just called `kernel`. Where they are stored depends on
# the cargo version (deps/ or build/<pkg>/<hash>/out/), see docs/BUGS.md.
if [[ -z "${KERNEL_MODE:-}" ]]; then
    if [[ "$(basename "$ELF")" =~ -[0-9a-f]{16}$ ]]; then KERNEL_MODE=test; else KERNEL_MODE=run; fi
fi

if [[ ! -f "$LIMINE/limine-bios-cd.bin" ]]; then
    echo "Limine is missing: run make limine" >&2
    exit 1
fi

NAME="$(basename "$ELF")"
ISO_ROOT="$BUILD/iso_root-$NAME"
ISO="$BUILD/$NAME.iso"

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

# Hard disk (SATA, AHCI port 0):
#   run mode:  the persistent build/disk.img (created from rootfs/ on first use), or $KERNEL_DISK
#   test mode: a fresh image per test binary, so tests can write to it freely
if [[ -n "${KERNEL_DISK:-}" ]]; then
    DISK="$KERNEL_DISK"
elif [[ "$KERNEL_MODE" == test ]]; then
    DISK="$BUILD/test-disks/$NAME.img"
    "$ROOT/scripts/mkdisk.sh" "$DISK" 64 2>/dev/null
else
    DISK="$BUILD/disk.img"
    [[ -f "$DISK" ]] || "$ROOT/scripts/mkdisk.sh" "$DISK" 64 2>/dev/null
    # keep /bin on the persistent disk in sync with the freshly built programs
    "$ROOT/scripts/install-user.sh" "$DISK"
fi

QEMU_ARGS=(
    -M q35 -m 256M -smp 4
    -cdrom "$ISO"
    -boot d
    -no-reboot
    -device isa-debug-exit,iobase=0xf4,iosize=0x04
    -drive "file=$DISK,format=raw,if=none,id=disk0"
    -device ide-hd,drive=disk0,bus=ide.0
)
[[ "${KERNEL_DEBUG:-0}" == 1 ]] && QEMU_ARGS+=(-s -S)

if [[ "$KERNEL_MODE" == test ]]; then
    set +e
    timeout --foreground "$TIMEOUT" qemu-system-x86_64 "${QEMU_ARGS[@]}" \
        -serial stdio -display none "$@"
    code=$?
    set -e
    # isa-debug-exit: QEMU exit status = (value << 1) | 1  ->  0x10 -> 33 = pass
    case $code in
        33)  echo "[run-qemu] PASS"; exit 0 ;;
        124) echo "[run-qemu] FAIL: timeout after ${TIMEOUT}s (hang?)" >&2; exit 1 ;;
        *)   echo "[run-qemu] FAIL: QEMU exit status $code" >&2; exit 1 ;;
    esac
else
    exec qemu-system-x86_64 "${QEMU_ARGS[@]}" -serial stdio "$@"
fi
