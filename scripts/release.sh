#!/usr/bin/env bash
# Builds the files of a release into a directory: the bootable ISO, the disk
# image with /bin (gzip), the start script and SHA256SUMS. Then boots the
# result once (scripts/check-release.py) - a release that does not start is
# never written out as finished.
#
# Usage: scripts/release.sh [out_dir]      (default build/release)
#   TAG=v1.2.3 scripts/release.sh          also checks that the tag matches the version
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/build/release}"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/kernel/Cargo.toml" | head -1)"
USER_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/user/Cargo.toml" | head -1)"
if [[ "$VERSION" != "$USER_VERSION" ]]; then
    echo "release: kernel/Cargo.toml says $VERSION, user/Cargo.toml $USER_VERSION" >&2
    exit 1
fi
if [[ -n "${TAG:-}" && "$TAG" != "v$VERSION" ]]; then
    echo "release: tag $TAG does not match version $VERSION" >&2
    exit 1
fi

make -C "$ROOT" build
rm -rf "$OUT"
mkdir -p "$OUT"
NAME="yak-$VERSION"
"$ROOT/scripts/mkiso.sh" "$ROOT/kernel/target/x86_64-unknown-none/debug/kernel" "$OUT/$NAME.iso"
rm -rf "$OUT/iso_root-$NAME"
"$ROOT/scripts/mkdisk.sh" "$OUT/$NAME-disk.img" 64
cp "$ROOT/scripts/run-yak.sh" "$OUT/"

python3 -I "$ROOT/scripts/check-release.py" "$OUT"

gzip -9 "$OUT/$NAME-disk.img"
(cd "$OUT" && sha256sum "$NAME.iso" "$NAME-disk.img.gz" run-yak.sh > SHA256SUMS)
echo "release: $OUT"
ls -l "$OUT"
