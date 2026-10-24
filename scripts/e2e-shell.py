#!/usr/bin/env python3
"""End-to-end test: boots the real kernel and drives the shell over the serial port.

Usage: scripts/e2e-shell.py <kernel-elf> [extra QEMU arguments...]

Each step types a line (exactly like a user on a serial terminal, including
escape sequences for arrow keys and Tab), waits for the next prompt, and checks
the output between the two prompts. The last step powers the machine off and
expects QEMU to exit.
"""
import os
import re
import subprocess
import sys
import threading
import time

ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
PROMPT = "kernel> "
LEFT = "\x1b[D"
RIGHT = "\x1b[C"
UP = "\x1b[A"

# (keys to type, substrings that must appear in the output, substrings that must not)
STEPS = [
    ("echo hello from e2e\r", ["hello from e2e"], ["error"]),
    ('echo "quoted   words" plain\r', ["quoted   words plain"], []),
    ("eco" + LEFT + "h" + RIGHT + " edited\r", ["edited"], ["unknown command"]),  # insert in the middle: eco -> echo
    ("ver\t\r", ["KernelTry"], ["unknown command"]),  # tab completion of 'version'
    (UP + "\r", ["KernelTry"], []),  # history: run 'version' again
    ("help\r", ["Available commands", "poweroff", "memmap"], []),
    ("mem\r", ["Physical:", "MiB usable", "Heap:"], []),
    ("memmap\r", ["usable", "kernel and modules"], []),
    ("uptime\r", ["up 00:", "ticks at 100 Hz"], []),
    ("cpu\r", ["Vendor:", "Flags:", "lm"], []),
    ("acpi\r", ["APIC", "I/O APIC", "ISA IRQ 0 -> GSI 2"], []),
    ("layout us\r", ["keyboard layout: Us"], []),
    ("layout de\r", ["keyboard layout: De"], []),
    ("sleep 100\r", ["slept 100 ms"], []),
    ("int3\r", ["breakpoint handled"], []),
    ("history\r", ["echo hello from e2e", "version"], []),
    ("does-not-exist\r", ["unknown command 'does-not-exist'"], []),
    ('echo "unterminated\r', ["unterminated"], []),
    ("echo Grüße äöü\r", ["Grüße äöü"], []),
]


class Reader(threading.Thread):
    """Collects QEMU's stdout (the serial port) in the background."""

    def __init__(self, stream):
        super().__init__(daemon=True)
        self.stream = stream
        self.buf = ""
        self.lock = threading.Lock()

    def run(self):
        raw = b""
        while chunk := self.stream.read1(4096):
            raw += chunk
            # Strip escape sequences on the whole buffer: a sequence (or a UTF-8
            # character) may be split across two chunks.
            text = ANSI.sub("", raw.decode("utf-8", errors="replace"))
            with self.lock:
                self.buf = text

    def wait_for(self, text, start, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self.lock:
                idx = self.buf.find(text, start)
                if idx >= 0:
                    return idx
            time.sleep(0.05)
        raise TimeoutError(f"did not see {text!r} within {timeout}s")

    def text(self, start, end=None):
        with self.lock:
            return self.buf[start:end]


def main() -> int:
    elf, extra_args = sys.argv[1], sys.argv[2:]
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    proc = subprocess.Popen(
        [os.path.join(root, "scripts/run-qemu.sh"), elf, "-display", "none", *extra_args],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        env=dict(os.environ, KERNEL_MODE="run"))
    reader = Reader(proc.stdout)
    reader.start()
    failures = 0
    try:
        pos = reader.wait_for(PROMPT, 0, 30) + len(PROMPT)
        for keys, expected, forbidden in STEPS:
            proc.stdin.write(keys.encode())
            proc.stdin.flush()
            nxt = reader.wait_for(PROMPT, pos, 15)
            output = reader.text(pos, nxt)
            missing = [e for e in expected if e not in output]
            present = [f for f in forbidden if f in output]
            label = keys.replace("\r", "").replace("\x1b", "^[").replace("\t", "<Tab>")
            if missing or present:
                failures += 1
                print(f"FAIL  {label!r}: missing {missing}, unexpected {present}\n----\n{output}\n----")
            else:
                print(f"ok    {label!r}")
            pos = nxt + len(PROMPT)

        proc.stdin.write(b"poweroff\r")
        proc.stdin.flush()
        try:
            code = proc.wait(timeout=10)
            print(f"ok    'poweroff' (QEMU exited with {code})")
        except subprocess.TimeoutExpired:
            failures += 1
            print("FAIL  'poweroff': QEMU still running")
    except TimeoutError as e:
        failures += 1
        print(f"FAIL  {e}\n---- output so far ----\n{reader.text(0)[-3000:]}")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()

    total = len(STEPS) + 1
    print(f"\n{total - failures}/{total} steps passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
