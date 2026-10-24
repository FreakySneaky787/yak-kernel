#!/usr/bin/env python3
"""End-to-end test: boots the real kernel and drives the shell over the serial port.

Usage: scripts/e2e-shell.py <kernel-elf> [extra QEMU arguments...]

Each step types a line (exactly like a user on a serial terminal, including
escape sequences for arrow keys and Tab), waits for the next prompt, and checks
the output between the two prompts. Each session ends with `poweroff`.

Two sessions run on the same scratch disk: the second one boots again and
checks that files written in the first one survived. Finally `fsck.fat`
validates the disk image.
"""
import os
import re
import subprocess
import sys
import threading
import time

ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
PROMPT = re.compile(r"kernel:(/[^>\n]*)> ")
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
    ("ps\r", ["TID", "main", "idle"], []),
    ("demo 3\r", ["worker 1", "worker 2", "worker 3", "found 3245 primes", "all workers done"], []),
    ("lspci\r", ["SATA controller", "host bridge"], []),
    ("disk\r", ["QEMU HARDDISK", "64 MiB", "Atapi"], []),
    ("hexdump 0\r", ["FAT32", "55 aa"], []),
    ("history\r", ["echo hello from e2e", "version"], []),
    ("ls /\r", ["README.txt", "docs/", "etc/"], []),
    ("cat /docs/hello.txt\r", ["Hello from a file on a real (virtual) SATA disk."], []),
    ("cd docs\r", [], ["error"]),
    ("pwd\r", ["/docs"], []),
    ("mkdir \"My Notes\"\r", [], ["error"]),
    ("write \"My Notes/today.txt\" written before the reboot\r", [], ["error"]),
    ("append \"My Notes/today.txt\" Grüße äöü\r", [], ["error"]),
    ("cat \"My Notes/today.txt\"\r", ["written before the reboot", "Grüße äöü"], []),
    ("ls\r", ["hello.txt", "My Notes/"], ["error"]),  # /bin/ls in the shell's cwd
    ("cp \"My Notes/today.txt\" \"/My copy.txt\"\r", [], ["error"]),  # NUL-separated args keep the spaces
    ("cat \"/My copy.txt\"\r", ["written before the reboot", "Grüße äöü"], ["error"]),
    ("cd ..\r", [], ["error"]),
    ("df\r", ["KERNELTRY", "free"], []),
    ("date\r", ["20"], []),
    ("cat /nope\r", ["no such file or directory"], []),
    ("does-not-exist\r", ["unknown command 'does-not-exist'"], []),
    ('echo "unterminated\r', ["unterminated"], []),
    ("echo Grüße äöü\r", ["Grüße äöü"], []),
    # user space: programs from /bin on the disk, running in ring 3
    ("ls /bin\r", ["hello", "fault", "upper", "cat", "cp", "ls", "fstest"], []),
    ("fstest\r", ["fstest: all checks passed"], ["FAIL"]),
    ("hello e2e world\r", ["Hello from user space! I am process", "arg 1: e2e", "arg 2: world", "Bye!"], ["error"]),
    ("upp\t\rhello Welt äöü\r", ["Type a line: hello Welt äöü", "HELLO WELT ÄÖÜ"], ["error"]),  # completion + stdin
    ("fault null\r", ["fault was killed: page fault at 0x0"], []),
    ("fault kernel\r", ["fault was killed: page fault at 0xffffffff80000000"], ["must never happen"]),
    ("run /bin/fault exit 3\r", ["fault exited with code 3"], []),
    ("echo kernel still alive\r", ["kernel still alive"], ["error"]),
]


# Second boot on the same disk: the data must still be there
PERSIST_STEPS = [
    ("cat \"/docs/My Notes/today.txt\"\r", ["written before the reboot", "Grüße äöü"], ["error"]),
    ("ls /docs\r", ["My Notes/", "hello.txt"], []),
    ("cat \"/My copy.txt\"\r", ["Grüße äöü"], ["error"]),
    ("rm \"/My copy.txt\"\r", [], ["error"]),
    ("rm \"/docs/My Notes/today.txt\"\r", [], ["error"]),
    ("rm \"/docs/My Notes\"\r", [], ["error"]),
    ("write /persist.txt survived\r", [], ["error"]),
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

    def wait_for_prompt(self, start, timeout):
        """Returns (start, end) of the next prompt after `start`."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self.lock:
                m = PROMPT.search(self.buf, start)
                if m:
                    return m.start(), m.end()
            time.sleep(0.05)
        raise TimeoutError(f"no prompt within {timeout}s")

    def text(self, start, end=None):
        with self.lock:
            return self.buf[start:end]


def run_session(root, elf, extra_args, disk, steps, title):
    """Boots the kernel once, runs `steps`, powers off. Returns the number of failures."""
    print(f"--- {title} ---")
    proc = subprocess.Popen(
        [os.path.join(root, "scripts/run-qemu.sh"), elf, "-display", "none", *extra_args],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        env=dict(os.environ, KERNEL_MODE="run", KERNEL_DISK=disk))
    reader = Reader(proc.stdout)
    reader.start()
    failures = 0
    try:
        pos = reader.wait_for_prompt(0, 30)[1]
        for keys, expected, forbidden in steps:
            proc.stdin.write(keys.encode())
            proc.stdin.flush()
            nxt_start, nxt_end = reader.wait_for_prompt(pos, 20)
            output = reader.text(pos, nxt_start)
            missing = [e for e in expected if e not in output]
            present = [f for f in forbidden if f in output]
            label = keys.replace("\r", "").replace("\x1b", "^[").replace("\t", "<Tab>")
            if missing or present:
                failures += 1
                print(f"FAIL  {label!r}: missing {missing}, unexpected {present}\n----\n{output}\n----")
            else:
                print(f"ok    {label!r}")
            pos = nxt_end

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
    return failures


def main() -> int:
    elf, extra_args = sys.argv[1], sys.argv[2:]
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    # Never touch the user's persistent build/disk.img: use a fresh scratch disk
    disk = os.path.join(root, "build", "e2e-disk.img")
    subprocess.run([os.path.join(root, "scripts/mkdisk.sh"), disk, "64"], check=True, stderr=subprocess.DEVNULL)

    failures = run_session(root, elf, extra_args, disk, STEPS, "session 1")
    failures += run_session(root, elf, extra_args, disk, PERSIST_STEPS, "session 2 (after reboot, same disk)")

    fsck = subprocess.run(["fsck.fat", "-n", disk], capture_output=True, text=True)
    if fsck.returncode == 0:
        print("ok    fsck.fat: disk image is consistent")
    else:
        failures += 1
        print(f"FAIL  fsck.fat:\n{fsck.stdout}{fsck.stderr}")
    persisted = subprocess.run(["mtype", "-i", disk, "::/persist.txt"], capture_output=True, text=True,
                               env=dict(os.environ, MTOOLS_SKIP_CHECK="1"))
    if persisted.stdout == "survived\n":
        print("ok    mtools reads /persist.txt written by the kernel")
    else:
        failures += 1
        print(f"FAIL  mtools /persist.txt: {persisted.stdout!r} {persisted.stderr!r}")

    total = len(STEPS) + len(PERSIST_STEPS) + 4
    print(f"\n{total - failures}/{total} steps passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
