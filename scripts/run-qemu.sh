#!/usr/bin/env bash
# Cargo-Runner: bekommt den Pfad zum gebauten Kernel-ELF, packt ihn mit Limine
# in eine bootfähige ISO und startet QEMU.
#
# Modi (über Umgebungsvariablen):
#   KERNEL_MODE=run    (Default bei `cargo run`) Fenster + Serial auf stdio
#   KERNEL_MODE=test   (automatisch, wenn das ELF aus deps/ kommt = `cargo test`)
#                      headless, Timeout, isa-debug-exit wird ausgewertet
#   KERNEL_DEBUG=1     QEMU wartet auf gdb (-s -S)
set -euo pipefail

ELF="$1"; shift || true
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIMINE="$ROOT/vendor/limine"
BUILD="$ROOT/build"
TIMEOUT="${KERNEL_TEST_TIMEOUT:-30}"

# Testbinaries von `cargo test` tragen einen Hash im Namen (kernel-<16 hex>),
# das Binary von `cargo run` heißt nur `kernel`. Der Ablageort hängt von der
# Cargo-Version ab (deps/ oder build/<pkg>/<hash>/out/), siehe docs/BUGS.md.
if [[ -z "${KERNEL_MODE:-}" ]]; then
    if [[ "$(basename "$ELF")" =~ -[0-9a-f]{16}$ ]]; then KERNEL_MODE=test; else KERNEL_MODE=run; fi
fi

if [[ ! -f "$LIMINE/limine-bios-cd.bin" ]]; then
    echo "Limine fehlt: make limine" >&2
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

QEMU_ARGS=(
    -M q35 -m 256M
    -cdrom "$ISO"
    -boot d
    -no-reboot
    -device isa-debug-exit,iobase=0xf4,iosize=0x04
)
[[ "${KERNEL_DEBUG:-0}" == 1 ]] && QEMU_ARGS+=(-s -S)

if [[ "$KERNEL_MODE" == test ]]; then
    set +e
    timeout --foreground "$TIMEOUT" qemu-system-x86_64 "${QEMU_ARGS[@]}" \
        -serial stdio -display none "$@"
    code=$?
    set -e
    # isa-debug-exit: QEMU-Exitcode = (wert << 1) | 1  →  0x10 → 33 = bestanden
    case $code in
        33)  echo "[run-qemu] PASS"; exit 0 ;;
        124) echo "[run-qemu] FAIL: Timeout nach ${TIMEOUT}s (Hänger?)" >&2; exit 1 ;;
        *)   echo "[run-qemu] FAIL: QEMU-Exitcode $code" >&2; exit 1 ;;
    esac
else
    exec qemu-system-x86_64 "${QEMU_ARGS[@]}" -serial stdio "$@"
fi
