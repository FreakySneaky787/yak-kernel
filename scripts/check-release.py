#!/usr/bin/env python3
"""Boots the files of a release (scripts/release.sh) the way a user would -
with run-yak.sh, nothing from the build tree - and checks the system works:
the shell from the disk image starts, `uname` shows the release's version,
the programs in /bin run, `poweroff` ends QEMU. BIOS always, UEFI too if
OVMF is installed. QEMU's -snapshot keeps the released disk image untouched.

Usage: scripts/check-release.py <release_dir>
"""
import os
import re
import subprocess
import sys
import threading
import time

ANSI = re.compile(rb"\x1b\[[0-9;=]*[A-Za-z]")
OVMF_PATHS = [
    "/usr/share/OVMF/OVMF_CODE_4M.fd",
    "/usr/share/OVMF/OVMF_CODE.fd",
    "/usr/share/edk2/ovmf/OVMF_CODE.fd",
]


def boot(release, version, firmware_args):
    proc = subprocess.Popen(
        [os.path.join(release, "run-yak.sh"), "--headless", *firmware_args, "-snapshot"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    output = bytearray()

    def reader():
        for chunk in iter(lambda: proc.stdout.read1(4096), b""):
            output.extend(chunk)

    threading.Thread(target=reader, daemon=True).start()

    def wait_for(text, seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            if text in ANSI.sub(b"", bytes(output)):
                return True
            time.sleep(0.2)
        return False

    failures = []
    steps = [
        (None, b"Type 'help'", 90),
        (b"uname\r", f"YAK {version} x86_64".encode(), 20),
        (b"ls /bin | wc\r", None, 20),  # checked below: the number of programs
        (b"echo \"check $(hello | wc -l)\"\r", b"check ", 20),
    ]
    for keys, expect, seconds in steps:
        before = len(output)
        if keys:
            time.sleep(0.5)
            proc.stdin.write(keys)
            proc.stdin.flush()
        if expect and not wait_for(expect, seconds):
            failures.append(f"no {expect!r} after {keys!r}")
            break
        if expect is None:
            time.sleep(3)
            text = ANSI.sub(b"", bytes(output[before:])).decode(errors="replace")
            numbers = [int(n) for n in re.findall(r"^\s*(\d+)\s+\d+\s+\d+\s*$", text, re.M)]
            if not numbers or numbers[-1] < 30:
                failures.append(f"/bin looks empty: {text!r}")
    proc.stdin.write(b"poweroff\r")
    proc.stdin.flush()
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        failures.append("poweroff did not end QEMU")
    if failures:
        print(ANSI.sub(b"", bytes(output)).decode(errors="replace")[-3000:])
    return failures


def main():
    release = sys.argv[1]
    isos = [m.group(1) for f in os.listdir(release) if (m := re.fullmatch(r"yak-(.+)\.iso", f))]
    if len(isos) != 1:
        sys.exit(f"check-release: expected one yak-<version>.iso in {release}, found {len(isos)}")
    version = isos[0]
    runs = [("BIOS", [])]
    ovmf = next((p for p in OVMF_PATHS if os.path.exists(p)), None)
    if ovmf:
        runs.append(("UEFI", ["--uefi", ovmf]))
    failed = False
    for name, args in runs:
        failures = boot(release, version, args)
        for f in failures:
            print(f"FAIL  release {version} ({name}): {f}")
        if not failures:
            print(f"ok    release {version} boots ({name}): shell, uname, /bin, $(...), poweroff")
        failed |= bool(failures)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
