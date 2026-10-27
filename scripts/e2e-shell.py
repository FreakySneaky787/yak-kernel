#!/usr/bin/env python3
"""End-to-end test: boots the real kernel and drives the shell over the serial port.

Usage: scripts/e2e-shell.py <kernel-elf> [extra QEMU arguments...]

Each step types a line (exactly like a user on a serial terminal, including
escape sequences for arrow keys and Tab), waits for the next prompt, and checks
the output between the two prompts. A PAUSE inside the keys waits a moment
before typing the rest (e.g. Ctrl+C once a program runs). Each session ends
with `poweroff`.

The kernel starts /bin/sh as init, so the steps run against the user-space
shell (prompt "sh:/path> "); the last steps of session 1 leave it with `exit`
and continue in the kernel shell (prompt "kernel:/path> ").

Two sessions run on the same scratch disk: the second one boots again and
checks that files written in the first one survived. Finally `fsck.fat`
validates the disk image.
"""
import os
import re
import socket
import subprocess
import sys
import threading
import time

ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
PROMPT = re.compile(r"(sh|kernel):(/[^>\n]*)> ")
LEFT = "\x1b[D"
RIGHT = "\x1b[C"
UP = "\x1b[A"
DOWN = "\x1b[B"
HOME = "\x1b[H"
END = "\x1b[F"
CTRL_C = "\x03"
PAUSE = "\0"  # never typed: splits the keys of a step
CPUS = int(os.environ.get("KERNEL_SMP", "4"))  # same default as scripts/run-qemu.sh


def start_udp_echo():
    """A UDP echo server on the host's 127.0.0.1 (the guest reaches it as 10.0.2.2)."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))

    def serve():
        while True:
            data, addr = sock.recvfrom(2048)
            sock.sendto(data, addr)

    threading.Thread(target=serve, daemon=True).start()
    return sock.getsockname()[1]


UDP_PORT = start_udp_echo()


def start_tcp_server(handle):
    """A TCP server on the host's 127.0.0.1; `handle(conn)` serves one connection."""
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", 0))
    srv.listen(4)

    def serve():
        while True:
            conn, _ = srv.accept()
            with conn:
                handle(conn)

    threading.Thread(target=serve, daemon=True).start()
    return srv.getsockname()[1]


def tcp_echo(conn):
    while data := conn.recv(4096):
        conn.sendall(data)


BULK = bytes(b"0123456789abcdef"[i % 16] for i in range(100_000))


def tcp_bulk(conn):
    conn.recv(100)  # whatever the guest sends first
    conn.sendall(BULK)


TCP_ECHO_PORT = start_tcp_server(tcp_echo)
TCP_BULK_PORT = start_tcp_server(tcp_bulk)

def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# host -> guest: QEMU forwards these host ports to echod (7) and httpd (80) in the guest
FWD_PORT = free_port()
FWD_HTTP_PORT = free_port()


def start_fake_dns():
    """Answers A queries with 10.0.2.2 (the host as seen from the guest) - and
    misbehaves for some names, to test the client's error handling (M58):
    nx.* = no such name, fail.* = server failure, cname.* = an answer without
    an A record, noise.* = a stray reply and a truncated one before the real
    answer, silent.* = no answer at all (the client must give up)."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))

    def serve():
        while True:
            q, addr = sock.recvfrom(512)
            end = 12
            labels = []
            while q[end] != 0:
                labels.append(q[end + 1:end + 1 + q[end]].decode(errors="replace"))
                end += q[end] + 1
            first = labels[0] if labels else ""
            question = q[12:end + 5]
            a_record = b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04" + bytes([10, 0, 2, 2])
            ok = q[:2] + b"\x81\x80\x00\x01\x00\x01\x00\x00\x00\x00" + question + a_record
            if first == "silent":
                continue
            if first == "nx":
                sock.sendto(q[:2] + b"\x81\x83\x00\x01\x00\x00\x00\x00\x00\x00" + question, addr)
            elif first == "fail":
                sock.sendto(q[:2] + b"\x81\x82\x00\x01\x00\x00\x00\x00\x00\x00" + question, addr)
            elif first == "cname":
                cname = b"\xc0\x0c\x00\x05\x00\x01\x00\x00\x00\x3c\x00\x02\xc0\x0c"
                sock.sendto(q[:2] + b"\x81\x80\x00\x01\x00\x01\x00\x00\x00\x00" + question + cname, addr)
            elif first == "noise":
                wrong_id = bytes([q[0] ^ 0xFF, q[1]])
                sock.sendto(wrong_id + ok[2:], addr)  # someone else's answer
                sock.sendto(ok[:-3], addr)  # ours, but cut off
                sock.sendto(ok, addr)
            else:
                sock.sendto(ok, addr)

    threading.Thread(target=serve, daemon=True).start()
    return sock.getsockname()[1]


DNS_PORT = start_fake_dns()
HTTP_BODY = b"Hello from the host over HTTP\n"


def http_handler(conn):
    req = b""
    while b"\r\n\r\n" not in req and (chunk := conn.recv(1024)):
        req += chunk
    status = b"200 OK" if req.startswith(b"GET /hello ") else b"404 Not Found"
    body = HTTP_BODY if status == b"200 OK" else b"not here\n"
    conn.sendall(b"HTTP/1.0 " + status + b"\r\nContent-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body)


HTTP_PORT = start_tcp_server(http_handler)
GUEST_HTTP = {"ok": False}


def fetch_from_guest_httpd():
    """Retries until httpd in the guest serves /docs/hello.txt (it runs during one step)."""
    deadline = time.time() + 600
    while time.time() < deadline and not GUEST_HTTP["ok"]:
        try:
            with socket.create_connection(("127.0.0.1", FWD_HTTP_PORT), timeout=5) as s:
                s.sendall(b"GET /docs/hello.txt HTTP/1.0\r\n\r\n")
                got = b""
                while chunk := s.recv(65536):
                    got += chunk
                GUEST_HTTP["ok"] = got.startswith(b"HTTP/1.0 200 OK") and b"SATA disk" in got
        except OSError:
            pass
        time.sleep(0.5)
HOST_TO_GUEST = {"ok": False}


def host_to_guest_echo():
    """Retries until echod in the guest echoes 50 KB back (it runs during one step)."""
    data = bytes(i % 251 for i in range(50_000))
    deadline = time.time() + 600
    while time.time() < deadline and not HOST_TO_GUEST["ok"]:
        try:
            with socket.create_connection(("127.0.0.1", FWD_PORT), timeout=5) as s:
                s.sendall(data)
                s.shutdown(socket.SHUT_WR)
                got = b""
                while chunk := s.recv(65536):
                    got += chunk
                HOST_TO_GUEST["ok"] = got == data
        except OSError:
            pass
        time.sleep(0.5)

# (keys to type, substrings that must appear in the output, substrings that must not)
STEPS = [
    ("echo hello from e2e\r", ["hello from e2e"], ["error"]),
    ('echo "quoted   words" plain\r', ["quoted   words plain"], []),
    ("eco" + LEFT + "h" + RIGHT + " edited\r", ["edited"], ["unknown command"]),  # insert in the middle: eco -> echo
    ("ver\t\r", ["YAK"], ["unknown command"]),  # tab completion of 'version'
    (UP + "\r", ["YAK"], []),  # history: run 'version' again
    ("help\r", ["Available commands", "poweroff", "memmap"], []),
    ("mem\r", ["Physical:", "MiB usable", "Heap:"], []),
    ("memmap\r", ["usable", "kernel and modules"], []),
    ("uptime\r", ["up 00:", "ticks at 100 Hz"], []),
    ("cpu\r", ["Vendor:", "Flags:", "lm", f"CPUs:   {CPUS} online", "In use: SMEP on, SMAP on, AVX state on"], []),  # SMP (M25), M37
    ("acpi\r", ["APIC", "I/O APIC", "ISA IRQ 0 -> GSI 2"], []),
    ("layout us\r", ["keyboard layout: Us"], []),
    ("layout de\r", ["keyboard layout: De"], []),
    ("sleep 100\r", ["slept 100 ms"], []),
    ("int3\r", ["breakpoint handled"], []),
    ("ps\r", ["TID", "main", "idle", "user", "sh"], []),  # /bin/ps via proclist
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
    ("df\r", ["YAK", "free"], []),
    ("date\r", ["20"], []),
    ("cat /nope\r", ["no such file or directory"], []),
    ("does-not-exist\r", ["unknown command 'does-not-exist'"], []),
    ('echo "unterminated\rstill open"\r', ["> still open", "unterminated\r\nstill open"], ["error"]),  # the quote continues
    ("echo Grüße äöü\r", ["Grüße äöü"], []),
    # user space: programs from /bin on the disk, running in ring 3
    ("ls /bin\r", ["hello", "fault", "upper", "cat", "cp", "ls", "fstest", "sh"], []),
    ("fstest\r", ["fstest: all checks passed"], ["FAIL"]),
    ("heaptest\r", ["heaptest: all checks passed"], ["FAIL"]),
    ("spawntest\r", ["spawntest: all checks passed"], ["FAIL"]),
    ("sleep 300\r", ["slept 300 ms"], []),  # the orphan started by spawntest prints in the meantime
    ("sigtest\r", ["sigtest: all checks passed"], ["FAIL"]),  # M34: signals
    ("threadtest\r", ["threadtest: all checks passed"], ["FAIL"]),  # M36: threads
    ("sigtest wait-int\r" + PAUSE + CTRL_C, ["press Ctrl+C", "caught SIGINT"], ["killed"]),  # Ctrl+C = SIGINT
    ("fault sleep &\r", ["[1]"], ["error"]),
    ("kill -l\r", ["TERM", "USR1"], ["error"]),
    ("kill -USR1 %1\r", [], ["error"]),  # the end is reported at one of the next prompts
    ("wait\r", [], ["error"]),
    ("jobs\r", [], ["Running"]),
    ("fault spin\r" + PAUSE + CTRL_C, ["spinning forever", "fault was killed: interrupted"], []),
    ("fault sleep\r" + PAUSE + CTRL_C, ["sleeping forever", "fault was killed: interrupted"], []),
    ("upper\r" + PAUSE + CTRL_C, ["Type a line", "upper was killed: interrupted"], []),  # blocked in read
    ("write /fruits.txt cherry\r", [], ["error"]),
    ("append /fruits.txt apple\r", [], ["error"]),
    ("append /fruits.txt banana\r", [], ["error"]),
    ("sort /fruits.txt\r", ["apple\r\nbanana\r\ncherry"], ["error"]),
    ("sort\rzebra\rant\r\x04", ["ant\r\nzebra"], ["error"]),  # typed lines, ended by Ctrl+D (serial: \r\n)
    # pipes and redirection
    ("ls /bin | grep test | sort -r\r", ["spawntest", "pipetest", "heaptest", "fstest"], ["error"]),
    ("echo first > /pipe.txt\r", [], ["error"]),
    ("echo second >> /pipe.txt\r", [], ["error"]),
    ("cat < /pipe.txt | sort -r | wc\r", ["      2       2      13"], ["error"]),
    ("ps > /ps.txt\r", [], ["error"]),
    ("grep sh /ps.txt\r", ["sh"], ["error"]),
    ("fault spin | cat\r" + PAUSE + CTRL_C, ["fault was killed: interrupted"], []),
    ("pipetest\r", ["pipetest: all checks passed"], ["FAIL"]),
    ("uname\r", ["YAK ", " x86_64"], ["error"]),
    # M57: a line longer than a console row (160 columns), edited at its start
    ("echo " + "long" * 45 + "\x1b[H\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C" + "start-" + "\r", ["start-" + "long" * 45 + "\r\n"], ["error"]),
    # M56: case (and $(...) inside its word)
    ("case $(uname) in yak* | YAK*) echo is-yak ;; *) echo other ;; esac\r", ["\r\nis-yak\r\n"], ["other\r\n", "error"]),
    # M55: command substitution at the prompt
    ('echo "files: $(ls /docs) sys: $(uname | wc)"\r', ["files:  ", "hello.txt\r\n", "My Notes/ sys:       1       3"], ["error"]),
    # M41: every syscall with bad pointers, lengths, descriptors and numbers - the kernel must survive
    ("syscheck\r", ["syscheck: all checks passed"], ["FAIL", "PANIC", "killed"]),
    ("netinfo\r", ["IPv4     10.0.2.15", "link up", "DNS      10.0.2.3", "config   DHCP"], ["error"]),  # M27, M30
    ("time hello\r", ["Hello from user space", "real 0.1"], ["error"]),  # TSC clock (M28)
    ("ping 10.0.2.2 2\r", ["reply from 10.0.2.2: seq=1", "2 packets sent, 2 received"], ["error"]),
    (f"udpecho 10.0.2.2 {UDP_PORT} hello over UDP\r", [f"reply from 10.0.2.2:{UDP_PORT}: hello over UDP"], ["error"]),
    (f"nc 10.0.2.2 {TCP_ECHO_PORT} hello over TCP\r", ["hello over TCP"], ["error"]),  # TCP (M29)
    (f"echo go | nc 10.0.2.2 {TCP_BULK_PORT} | wc\r", ["  100000"], ["error"]),  # 100 KB from the host
    (f"host www.yak.test 10.0.2.2 {DNS_PORT}\r", ["www.yak.test has address 10.0.2.2"], ["error"]),  # M30
    # M58: a name server that misbehaves
    (f"host nx.yak.test 10.0.2.2 {DNS_PORT}\r", ["host: nx.yak.test: no such host"], ["has address"]),
    (f"host fail.yak.test 10.0.2.2 {DNS_PORT}\r", ["host: fail.yak.test: server error 2"], ["has address"]),
    (f"host cname.yak.test 10.0.2.2 {DNS_PORT}\r", ["no address in the answer"], ["has address"]),
    (f"host noise.yak.test 10.0.2.2 {DNS_PORT}\r", ["noise.yak.test has address 10.0.2.2"], ["error", "malformed"]),
    (f"time host silent.yak.test 10.0.2.2 {DNS_PORT}\r", ["host: silent.yak.test: timed out", "real 4."], ["has address"]),
    ("host --selftest\r", ["host selftest: all checks passed"], ["FAIL"]),
    (f"wget http://10.0.2.2:{HTTP_PORT}/hello\r", ["Hello from the host over HTTP", "200 OK"], ["error"]),
    (f"wget -O /fetched.txt http://10.0.2.2:{HTTP_PORT}/hello\r", ["200 OK"], ["error"]),
    ("cat /fetched.txt\r", ["Hello from the host over HTTP"], ["error"]),
    ("httpd 80 -n 1\r", ["GET /docs/hello.txt -> 200 OK"], ["error"]),  # the host fetches from the guest
    ("echod -n 1\r", ["echod: listening on port 7", "echoed 50000 bytes", "served 1 connection"], ["error"]),
    # shell features (M31)
    ("export WHO=world\r", [], ["error"]),
    ("echo hello $WHO, status $?\r", ["hello world, status 0"], ["error"]),
    ("env WHO\r", ["world"], ["error"]),
    ("ls /nothing 2>&1 | wc\r", ["      1       7"], []),  # "ls: /nothing: no such file or directory"
    # M35: shell programming
    ("for i in 1 2\rdo echo round $i\rdone\r", ["> do echo round", "round 1\r\nround 2"], ["error"]),
    ("n=0; while true; do n=$((n + 1)); sleep 20; done\r" + PAUSE + CTRL_C, [], ["error"]),  # Ctrl+C ends the loop
    ("echo stopped after $n rounds\r", ["stopped after"], ["stopped after 0 rounds"]),
    ("count() { echo \"$# args: $1\"; return 4; }; count a b || echo returned $?\r", ["2 args: a", "returned 4"], ["error"]),
    ("for f in /docs/*.txt; do [ -f $f ] && echo file $f; done\r", ["file /docs/hello.txt"], ["error"]),
    ("hello job &\r", ["[2] "], ["error"]),
    ("wait\r", ["[2] Done     hello job"], ["error"]),
    ("write /s.sh 'echo from a script: $1'\r", [], ["error"]),  # quoted: the interactive shell must not expand it
    ("sh /s.sh argument\r", ["from a script: argument"], ["error"]),
    ("fputest\r", ["fputest: all checks passed"], ["registers changed", "MXCSR changed", "is wrong", "error"]),  # SSE registers per thread (M24)
    # file operations as programs (M22)
    ("touch /t1.txt\r", [], ["error"]),
    ("mv /t1.txt /docs\r", [], ["error"]),
    ("stat /docs/t1.txt\r", ["type:      file", "size:      0 bytes"], ["error"]),
    ("rm /docs/t1.txt\r", [], ["error"]),
    ("ls /docs\r", ["hello.txt"], ["t1.txt"]),
    # the text editor (M23): create, change, save (^S = 0x13), quit (^Q = 0x11)
    ("edit /notes.txt\rFirst line\rsecond\x13\x11", ["new file", "saved 18 bytes"], ["error"]),
    ("cat /notes.txt\r", ["First line\r\nsecond"], ["error"]),
    ("edit /notes.txt\r" + DOWN + END + " line" + UP + HOME + "The \x13\x11", ["saved 27 bytes"], ["error"]),
    ("cat /notes.txt\r", ["The First line\r\nsecond line"], ["error"]),
    ("edit --selftest\r", ["edit selftest: all checks passed"], ["FAIL"]),
    ("hello e2e world\r", ["Hello from user space! I am process", "arg 1: e2e", "arg 2: world", "Bye!"], ["error"]),
    ("upp\t\rhello Welt äöü\r", ["Type a line: hello Welt äöü", "HELLO WELT ÄÖÜ"], ["error"]),  # completion + stdin
    ("fault null\r", ["fault was killed: page fault at 0x0"], []),
    ("fault kernel\r", ["fault was killed: page fault at 0xffffffff80000000"], ["must never happen"]),
    ("run /bin/fault exit 3\r", ["fault exited with code 3"], []),
    ("echo kernel still alive\r", ["kernel still alive"], ["error"]),
    # leaving /bin/sh: the kernel shell takes over
    ("exit\r", ["/bin/sh exited with code 0 - falling back to the kernel shell"], []),
    ("echo in the kernel shell\r", ["in the kernel shell"], ["error"]),
    ("hello\r", ["Hello from user space!"], ["error"]),  # programs still run from there
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
        env=dict(os.environ, KERNEL_MODE="run", KERNEL_DISK=disk,
                 KERNEL_NETDEV_OPTS=f"hostfwd=tcp:127.0.0.1:{FWD_PORT}-:7,hostfwd=tcp:127.0.0.1:{FWD_HTTP_PORT}-:80"))
    reader = Reader(proc.stdout)
    reader.start()
    failures = 0
    try:
        first, pos = reader.wait_for_prompt(0, 30)
        if reader.text(first, pos).startswith("sh:"):
            print("ok    init is /bin/sh")
        else:
            failures += 1
            print(f"FAIL  init: first prompt is {reader.text(first, pos)!r}, expected the /bin/sh prompt")
        for keys, expected, forbidden in steps:
            for i, part in enumerate(keys.split(PAUSE)):
                if i > 0:
                    time.sleep(0.7)
                proc.stdin.write(part.encode())
                proc.stdin.flush()
            nxt_start, nxt_end = reader.wait_for_prompt(pos, 20)
            output = reader.text(pos, nxt_start)
            missing = [e for e in expected if e not in output]
            present = [f for f in forbidden if f in output]
            label = (keys.replace("\r", "").replace("\x1b", "^[").replace("\t", "<Tab>")
                     .replace(PAUSE, "<pause>").replace(CTRL_C, "^C"))
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

    threading.Thread(target=host_to_guest_echo, daemon=True).start()
    threading.Thread(target=fetch_from_guest_httpd, daemon=True).start()
    failures = run_session(root, elf, extra_args, disk, STEPS, "session 1")
    if GUEST_HTTP["ok"]:
        print("ok    host -> guest HTTP: httpd served /docs/hello.txt")
    else:
        failures += 1
        print("FAIL  host -> guest HTTP: no answer from httpd")
    if HOST_TO_GUEST["ok"]:
        print("ok    host -> guest TCP: echod echoed 50000 bytes")
    else:
        failures += 1
        print("FAIL  host -> guest TCP: no correct echo from echod")
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
