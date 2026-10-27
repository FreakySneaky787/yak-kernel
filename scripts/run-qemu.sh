#!/usr/bin/env bash
# Cargo runner: receives the path of the built kernel ELF, packs it together with
# Limine into a bootable ISO (scripts/mkiso.sh) and starts QEMU.
#
# Modes (via environment variables):
#   KERNEL_MODE=run    (default for `cargo run`) window + serial on stdio
#   KERNEL_MODE=test   (automatic for `cargo test` binaries, see below)
#                      headless, timeout, isa-debug-exit status is evaluated
#   KERNEL_DEBUG=1     QEMU waits for gdb (-s -S)
#   KERNEL_DISK=<img>  use this disk image instead of the default one
#   KERNEL_SMP=<n>     number of CPU cores (default 4)
#   KERNEL_CPU=<model> QEMU CPU model (default max; qemu64 has no SMEP/SMAP/XSAVE/AVX)
#   KERNEL_MEM=<size>  RAM (default 256M; see "Supported environment" in README.md for the minimum)
#   KERNEL_NETDEV_OPTS extra options for the user-mode network (e.g. hostfwd=udp::5555-:7)
set -euo pipefail

ELF="$1"; shift || true
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/build"
TIMEOUT="${KERNEL_TEST_TIMEOUT:-30}"

# Test binaries built by `cargo test` carry a hash in their name (kernel-<16 hex>),
# the `cargo run` binary is just called `kernel`. Where they are stored depends on
# the cargo version (deps/ or build/<pkg>/<hash>/out/), see docs/BUGS.md.
if [[ -z "${KERNEL_MODE:-}" ]]; then
    if [[ "$(basename "$ELF")" =~ -[0-9a-f]{16}$ ]]; then KERNEL_MODE=test; else KERNEL_MODE=run; fi
fi

NAME="$(basename "$ELF")"
ISO="$BUILD/$NAME.iso"
mkdir -p "$BUILD"
"$ROOT/scripts/mkiso.sh" "$ELF" "$ISO"

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
    # "max": every feature TCG emulates (SMEP, SMAP, XSAVE, AVX/AVX2 ...)
    -M q35 -m "${KERNEL_MEM:-256M}" -smp "${KERNEL_SMP:-4}" -cpu "${KERNEL_CPU:-max}"
    -cdrom "$ISO"
    -boot d
    -no-reboot
    -device isa-debug-exit,iobase=0xf4,iosize=0x04
    -drive "file=$DISK,format=raw,if=none,id=disk0"
    -device ide-hd,drive=disk0,bus=ide.0
    # network: QEMU's user-mode NAT (we are 10.0.2.15, the gateway is 10.0.2.2)
    -netdev "user,id=net0${KERNEL_NETDEV_OPTS:+,$KERNEL_NETDEV_OPTS}"
    -device e1000,netdev=net0
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
