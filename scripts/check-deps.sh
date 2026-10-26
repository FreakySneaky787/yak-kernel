#!/usr/bin/env bash
# `make deps`: checks that every host tool the build and the tests need is installed,
# and prints the install command for the missing ones (Fedora and Debian/Ubuntu).
# Exit status 0 = all required tools found, 1 = something required is missing.
#
#   scripts/check-deps.sh             check this machine
#   scripts/check-deps.sh --selftest  hide one tool and check that it is reported
#
# OVMF (UEFI firmware, optional) is looked up at $OVMF, which the Makefile passes in.
set -u

# tool  fedora-package  debian-package  required|optional
TOOLS="
make                make            make            required
git                 git             git             required
gcc                 gcc             gcc             required
ld.bfd              binutils        binutils        required
python3             python3         python3         required
xorriso             xorriso         xorriso         required
qemu-system-x86_64  qemu-system-x86 qemu-system-x86 required
mkfs.fat            dosfstools      dosfstools      required
fsck.fat            dosfstools      dosfstools      required
mcopy               mtools          mtools          required
mtype               mtools          mtools          required
gdb                 gdb             gdb             optional
"

if [ "${1:-}" = "--selftest" ]; then
    # A PATH with links to every tool this machine has, except xorriso.
    dir=$(mktemp -d)
    trap 'rm -rf "$dir"' EXIT
    while read -r tool _; do
        [ -n "$tool" ] && [ "$tool" != xorriso ] && p=$(command -v "$tool") && ln -s "$p" "$dir/$tool"
    done <<< "$TOOLS"
    for extra in rustup cargo; do p=$(command -v "$extra") && ln -s "$p" "$dir/$extra"; done
    out=$(PATH="$dir" OVMF="$dir/none.fd" "$BASH" "$0" 2>&1)
    status=$?
    if [ $status -eq 1 ] && [[ "$out" == *"missing: xorriso"* ]]; then
        echo "check-deps selftest: ok (hidden xorriso reported, exit status 1)"
        exit 0
    fi
    echo "check-deps selftest: FAILED (exit status $status)"
    echo "$out"
    exit 1
fi

distro=unknown
if [ -r /etc/os-release ]; then
    ids=$(. /etc/os-release && echo "${ID:-} ${ID_LIKE:-}")
    case " $ids " in
        *" fedora "* | *" rhel "*) distro=fedora ;;
        *" debian "* | *" ubuntu "*) distro=debian ;;
    esac
fi

missing_required=0
packages=""
add_package() { [[ " $packages " == *" $1 "* ]] || packages="$packages $1"; }

while read -r tool fedora debian kind; do
    [ -z "$tool" ] && continue
    if command -v "$tool" > /dev/null 2>&1; then
        echo "ok       $tool"
        continue
    fi
    echo "missing: $tool ($kind)"
    [ "$kind" = required ] && missing_required=1
    if [ "$distro" = fedora ]; then add_package "$fedora"; else add_package "$debian"; fi
done <<< "$TOOLS"

if [ -n "${OVMF:-}" ] && [ -f "$OVMF" ]; then
    echo "ok       OVMF ($OVMF)"
else
    echo "missing: OVMF UEFI firmware (optional, for run-uefi / e2e-uefi; or set OVMF=<file>)"
    if [ "$distro" = fedora ]; then add_package edk2-ovmf; else add_package ovmf; fi
fi

# Rust comes from rustup, which installs the toolchain pinned in rust-toolchain.toml
# on the first cargo call inside the repository.
if command -v rustup > /dev/null 2>&1 && command -v cargo > /dev/null 2>&1; then
    echo "ok       rustup + cargo (toolchain: rust-toolchain.toml)"
else
    echo "missing: rustup (required) – install from https://rustup.rs, not from the distribution"
    missing_required=1
fi

# mdBook builds the website (`make book`); optional, not a distribution package
if command -v mdbook > /dev/null 2>&1 || [ -x "$HOME/.cargo/bin/mdbook" ]; then
    echo "ok       mdbook"
else
    echo "missing: mdbook (optional, for make book) – cargo install mdbook --locked --version 0.4.52"
fi

if [ -n "$packages" ]; then
    case $distro in
        fedora) echo "install with: sudo dnf install$packages" ;;
        debian) echo "install with: sudo apt install$packages" ;;
        *) echo "packages (Fedora/Debian names may differ):$packages" ;;
    esac
fi
exit $missing_required
