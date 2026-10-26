#!/usr/bin/env python3
"""Soak test: boots the real kernel on a scratch disk and runs `sh /soak.sh <rounds>`
(rootfs/soak.sh) over the serial port.

Usage: scripts/soak.py <kernel-elf> <rounds> [extra QEMU arguments...]

Passes when the script reports 0 failures, the kernel neither panics nor
kills /bin/sh, `poweroff` ends QEMU and fsck.fat finds the disk consistent.
Every round must finish within ROUND_TIMEOUT seconds.
"""
import os
import re
import subprocess
import sys
import threading
import time

ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
ROUND_TIMEOUT = 120
BAD = ["KERNEL PANIC", "DOUBLE FAULT", "(sh) killed", "soak: FAIL"]


def main() -> int:
    elf, rounds = sys.argv[1], int(sys.argv[2])
    extra = sys.argv[3:]
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    disk = os.path.join(root, "build", "soak-disk.img")
    subprocess.run([os.path.join(root, "scripts/mkdisk.sh"), disk, "64"], check=True, stderr=subprocess.DEVNULL)

    proc = subprocess.Popen(
        [os.path.join(root, "scripts/run-qemu.sh"), elf, "-display", "none", *extra],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        env=dict(os.environ, KERNEL_MODE="run", KERNEL_DISK=disk))
    out = bytearray()
    lock = threading.Lock()

    def reader():
        for chunk in iter(lambda: proc.stdout.read1(4096), b""):
            with lock:
                out.extend(chunk)
            sys.stdout.buffer.write(chunk)
            sys.stdout.flush()

    threading.Thread(target=reader, daemon=True).start()

    def text():
        with lock:
            return ANSI.sub("", out.decode(errors="replace"))

    def wait_for(pattern, timeout):
        end = time.time() + timeout
        while time.time() < end:
            t = text()
            if any(b in t for b in BAD) or proc.poll() is not None:
                return None
            m = re.search(pattern, t)
            if m:
                return m
            time.sleep(0.2)
        return None

    def send(line):
        proc.stdin.write(line.encode() + b"\r")
        proc.stdin.flush()

    start = time.time()
    problem = None
    if not wait_for(r"sh:/> ", 30):
        problem = "no /bin/sh prompt after boot"
    else:
        send(f"sh /soak.sh {rounds}")
        for r in range(1, rounds + 1):
            if not wait_for(rf"soak: round {r} of {rounds} done", ROUND_TIMEOUT):
                problem = f"round {r} did not finish within {ROUND_TIMEOUT}s"
                break
        else:
            m = wait_for(r"soak: finished \d+ rounds with (\d+) failures", ROUND_TIMEOUT)
            if not m or m.group(1) != "0":
                problem = "the script reported failures"
            else:
                wait_for(r"sh:/> $", 10)
                send("poweroff")
                try:
                    proc.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    problem = "poweroff did not end QEMU"
    found = [b for b in BAD if b in text()]
    if found:
        problem = f"found {found}"
    if proc.poll() is None:
        proc.terminate()
        proc.wait()

    fsck = subprocess.run(["fsck.fat", "-n", disk], capture_output=True, text=True)
    if not problem and fsck.returncode != 0:
        problem = "fsck.fat: " + fsck.stdout.strip()
    minutes = (time.time() - start) / 60
    if problem:
        print(f"\nsoak: FAIL after {minutes:.1f} min: {problem}")
        return 1
    print(f"\nsoak: PASS – {rounds} rounds in {minutes:.1f} min, disk consistent")
    return 0


if __name__ == "__main__":
    sys.exit(main())
