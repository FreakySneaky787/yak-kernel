#!/usr/bin/env python3
"""Boots the kernel headless, waits, and saves a screenshot as PNG.

Usage: scripts/screenshot.py <kernel-elf> <output.png> [wait_s] [keys...]
Keys (optional) are sent through the QEMU monitor after the wait time
(`sendkey` names, e.g. h e l p ret), followed by another short wait.
Serial output is written to <output>.serial.txt.
"""
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib


def ppm_to_png(ppm: bytes, out_path: str) -> None:
    # P6 header: "P6\n<w> <h>\n<max>\n"
    parts = ppm.split(b"\n", 3)
    w, h = map(int, parts[1].split())
    pixels = parts[3]
    raw = b"".join(b"\x00" + pixels[y * w * 3:(y + 1) * w * 3] for y in range(h))

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")
    with open(out_path, "wb") as f:
        f.write(png)


def monitor(sock_path: str, cmd: str) -> None:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(sock_path)
    s.sendall(cmd.encode() + b"\n")
    time.sleep(0.3)
    s.close()


def main() -> int:
    elf, out = sys.argv[1], sys.argv[2]
    wait = float(sys.argv[3]) if len(sys.argv) > 3 else 3.0
    keys = sys.argv[4:]
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    tmp = tempfile.mkdtemp()
    sock = os.path.join(tmp, "mon.sock")
    ppm = os.path.join(tmp, "screen.ppm")
    serial_log = open(out + ".serial.txt", "w")

    env = dict(os.environ, KERNEL_MODE="run")
    proc = subprocess.Popen(
        [os.path.join(root, "scripts/run-qemu.sh"), elf,
         "-display", "none", "-monitor", f"unix:{sock},server,nowait"],
        stdout=serial_log, stderr=subprocess.STDOUT, env=env)
    try:
        for _ in range(50):
            if os.path.exists(sock):
                break
            time.sleep(0.1)
        time.sleep(wait)
        for k in keys:
            monitor(sock, f"sendkey {k}")
        if keys:
            time.sleep(1.0)
        monitor(sock, f"screendump {ppm}")
        time.sleep(0.5)
        with open(ppm, "rb") as f:
            ppm_to_png(f.read(), out)
        print(f"Screenshot: {out}")
        return 0
    finally:
        proc.terminate()
        proc.wait(timeout=5)


if __name__ == "__main__":
    sys.exit(main())
