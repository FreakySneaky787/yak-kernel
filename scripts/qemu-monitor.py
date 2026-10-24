#!/usr/bin/env python3
"""Bootet den Kernel headless und führt Befehle im QEMU-Monitor aus.

Nutzung: scripts/qemu-monitor.py <kernel-elf> [wartezeit_s] -- <befehl> [<befehl> ...]
Beispiel: scripts/qemu-monitor.py kernel/target/x86_64-unknown-none/debug/kernel 3 -- "info pic" "info lapic"
"""
import os
import re
import socket
import subprocess
import sys
import tempfile
import time


def main() -> int:
    if "--" not in sys.argv:
        print(__doc__)
        return 1
    sep = sys.argv.index("--")
    elf = sys.argv[1]
    wait = float(sys.argv[2]) if sep > 2 else 3.0
    commands = sys.argv[sep + 1:]
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    sock_path = os.path.join(tempfile.mkdtemp(), "mon.sock")

    proc = subprocess.Popen(
        [os.path.join(root, "scripts/run-qemu.sh"), elf,
         "-display", "none", "-monitor", f"unix:{sock_path},server,nowait"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        env=dict(os.environ, KERNEL_MODE="run"))
    try:
        time.sleep(wait)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(sock_path)
        for c in commands:
            s.sendall(c.encode() + b"\n")
            time.sleep(0.5)
        time.sleep(0.5)
        s.setblocking(False)
        out = b""
        try:
            while chunk := s.recv(65536):
                out += chunk
        except BlockingIOError:
            pass
        # Monitor-Echo und Steuersequenzen entfernen
        text = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", out.decode(errors="replace"))
        print("\n".join(l for l in text.splitlines() if l.strip() and not l.startswith("(qemu)")))
        return 0
    finally:
        proc.terminate()
        proc.wait(timeout=5)


if __name__ == "__main__":
    sys.exit(main())
