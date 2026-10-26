//! Integration test: real user programs (built by `make user`, copied to
//! /bin on the test disk) are loaded from FAT32 and run in ring 3.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use kernel::fs;
use kernel::process::{self, ExitStatus, Output};
use kernel::qemu::{ExitCode, exit};
use kernel::serial_println;
use spin::Mutex;

/// Runs `/bin/<name>` with the space-separated `args` and returns its exit
/// status and captured output.
fn run(name: &str, args: &str) -> (ExitStatus, String) {
    let args = args.replace(' ', "\0"); // the argument block is NUL-separated
    let path = alloc::format!("/bin/{}", name);
    let image = fs::with_root(|fs| fs.read_file(&path)).expect("program missing on the test disk (make user?)");
    let out = Arc::new(Mutex::new(Vec::new()));
    let status = process::run(name, &image, &args, Output::Capture(out.clone())).unwrap();
    let text = String::from_utf8(out.lock().clone()).unwrap();
    serial_println!("  {} {:?}: {} | {:?}", name, args, status, text);
    (status, text)
}

fn killed_by(status: &ExitStatus, text: &str) -> bool {
    matches!(status, ExitStatus::Killed(reason) if reason.contains(text))
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    serial_println!("userspace::programs_from_disk ... ");

    let (status, out) = run("hello", "a b");
    assert_eq!(status, ExitStatus::Exited(0));
    assert!(out.starts_with("Hello from user space! I am process "));
    assert!(out.contains("  arg 1: a\n  arg 2: b\n"));
    assert!(out.contains("Slept for 10")); // 100 ms, 10 ms timer resolution

    assert_eq!(run("fault", "exit 7").0, ExitStatus::Exited(7));
    let (status, out) = run("fault", "panic");
    assert_eq!(status, ExitStatus::Exited(101));
    assert!(out.contains("panic: fault was asked to panic"));
    assert!(killed_by(&run("fault", "null").0, "page fault at 0x0"));
    assert!(killed_by(
        &run("fault", "kernel").0,
        "protection violation, read, user mode"
    ));
    assert!(killed_by(&run("fault", "cli").0, "general protection fault"));
    // the stack overflow grows the stack to its limit and runs into the unmapped area below
    let below_stack = alloc::format!(
        "page fault at {:#x}",
        process::USER_STACK_TOP - process::USER_STACK_MAX - 8
    );
    let (status, _) = run("fault", "stack");
    assert!(
        killed_by(&status, &below_stack[..below_stack.len() - 3]),
        "{:?}",
        status
    );

    // M17: programs working with files
    let (status, out) = run("cat", "/docs/hello.txt /etc/motd");
    assert_eq!(status, ExitStatus::Exited(0));
    assert!(
        out.starts_with("Hello from a file on a real (virtual) SATA disk."),
        "{}",
        out
    );
    let (status, out) = run("cat", "/missing");
    assert_eq!(
        (status, out.as_str()),
        (ExitStatus::Exited(1), "cat: /missing: no such file or directory\n")
    );
    let (status, out) = run("ls", "/");
    assert_eq!(status, ExitStatus::Exited(0));
    assert!(
        out.contains("README.txt") && out.contains("docs/") && out.contains("bin/"),
        "{}",
        out
    );
    assert_eq!(run("cp", "/docs/hello.txt /copied.txt").0, ExitStatus::Exited(0));
    let original = fs::with_root(|fs| fs.read_file("/docs/hello.txt")).unwrap();
    assert_eq!(fs::with_root(|fs| fs.read_file("/copied.txt")).unwrap(), original);
    let (status, out) = run("fstest", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);

    // M18: programs with a heap
    let free_before = kernel::mem::pmm::stats().free_frames;
    let (status, out) = run("heaptest", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    kernel::task::yield_now(); // reap the thread
    let leaked = free_before.saturating_sub(kernel::mem::pmm::stats().free_frames);
    assert!(leaked <= 4, "heaptest leaked {} frames", leaked);
    fs::with_root(|fs| fs.write_file("/sortme.txt", "pear\napple\nfig\nbanana\n".as_bytes())).unwrap();
    let (status, out) = run("sort", "/sortme.txt");
    assert_eq!(
        (status, out.as_str()),
        (ExitStatus::Exited(0), "apple\nbanana\nfig\npear\n")
    );
    assert_eq!(run("sort", "-r /sortme.txt").1, "pear\nfig\nbanana\napple\n");

    // M19: processes starting processes
    let (status, out) = run("spawntest", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    kernel::time::sleep_ms(300); // the orphan (hello sleeps 100 ms) ends and is cleaned up
    assert_eq!(process::count(), 0, "orphan left in the process table");

    // M34: signals (handlers, default actions, EINTR, registers kept across a handler)
    let (status, out) = run("sigtest", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);

    // M36: threads, futexes, the thread-safe heap; no thread may be left over
    let alive = || {
        kernel::task::list()
            .iter()
            .filter(|t| t.state != kernel::task::State::Dead)
            .count()
    };
    let threads_before = alive();
    let (status, out) = run("threadtest", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    assert_eq!(process::count(), 0, "processes left over");
    assert_eq!(alive(), threads_before, "threads left over");

    // M41: every syscall with bad pointers, lengths, descriptors and numbers
    let (status, out) = run("syscheck", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    assert!(out.contains("syscheck: all checks passed"));
    // bug #14: copies racing against another thread shrinking the heap
    let (status, out) = run("syscheck", "race 1000");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    assert!(out.contains("kernel still alive"));
    assert_eq!(process::count(), 0, "processes left over");

    // M20: the shell as a user program, driven by injected serial input
    kernel::serial::inject_rx(
        b"echo hi from sh\rcd docs\rpwd\rls\reco\x1b[Dh\x1b[C edited\rmem\rnope\rfault exit 4\rexit 7\r",
    );
    let (status, out) = run("sh", "");
    assert_eq!(status, ExitStatus::Exited(7), "{}", out);
    for expected in [
        "hi from sh\n",
        "sh\x1b[0m:\x1b[94m/docs\x1b[0m> ", // prompt after cd
        "/docs\n",
        "hello.txt",
        "edited\n",  // Left + insert + Right: eco -> echo
        "Physical:", // kernel built-in through kshell
        "unknown command 'nope'",
        "fault exited with code 4",
    ] {
        assert!(out.contains(expected), "missing {:?} in:\n{}", expected, out);
    }

    // M21: pipes and redirection
    let (status, out) = run("pipetest", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    kernel::serial::inject_rx(
        b"ls /bin | sort -r | grep e\recho hello > /r.txt\recho more >> /r.txt\rcat < /r.txt | wc\r\
          mem > /mem.txt\rcat /mem.txt | grep Physical | wc\rgrep nothing /r.txt | wc\recho > \rexit 0\r",
    );
    let (status, out) = run("sh", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    let r = fs::with_root(|fs| fs.read_file("/r.txt")).unwrap();
    assert_eq!(r, b"hello\nmore\n");
    // ls | sort -r | grep e: only names containing an "e", nothing else
    assert!(out.contains("  spawntest\n") && out.contains("  hello\n"), "{}", out);
    assert!(!out.contains("  cat\n") && !out.contains("  sh\n"), "{}", out);
    for expected in [
        "      2       2      11\n", // cat < /r.txt | wc
        "      1      16 ",          // one "Physical:" line from the kernel built-in written to a file
        "      0       0       0\n", // grep without a match, wc still runs (and only the last exit code counts)
        "missing file name after '>'",
    ] {
        assert!(out.contains(expected), "missing {:?} in:\n{}", expected, out);
    }

    // M22: system information and file operations as programs
    let (_, out) = run("ps", "");
    assert!(
        out.contains("TID") && out.contains("main") && out.contains("idle"),
        "{}",
        out
    );
    assert!(
        out.lines().any(|l| l.contains("user") && l.ends_with("ps")),
        "ps does not list itself:\n{}",
        out
    );
    assert!(run("mem", "").1.contains("MiB usable"));
    assert!(run("uptime", "").1.contains("ticks at 100 Hz"));
    assert!(run("df", "").1.contains("KERNELTRY"));
    assert!(run("date", "").1.starts_with("20"));
    assert_eq!(run("mkdir", "/m22").0, ExitStatus::Exited(0));
    assert_eq!(run("touch", "/m22/a.txt").0, ExitStatus::Exited(0));
    assert_eq!(run("mv", "/m22/a.txt /m22/b.txt").0, ExitStatus::Exited(0));
    assert!(run("stat", "/m22/b.txt").1.contains("size:      0 bytes"));
    assert_eq!(run("rm", "/m22").0, ExitStatus::Exited(1), "directory not empty");
    assert_eq!(run("rm", "/m22/b.txt /m22").0, ExitStatus::Exited(0));
    assert!(fs::with_root(|fs| fs.lookup("/m22")).is_err());

    // M23: the text editor
    let (status, out) = run("edit", "--selftest");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    let read = |p: &str| fs::with_root(|fs| fs.read_file(p)).unwrap();
    kernel::serial::inject_rx(b"Hello\rWorld\x13\x11"); // type, Enter, Ctrl+S, Ctrl+Q
    assert_eq!(run("edit", "/ed.txt").0, ExitStatus::Exited(0));
    assert_eq!(read("/ed.txt"), b"Hello\nWorld\n");
    kernel::serial::inject_rx(b"\x1b[B\x1b[F!\x1b[A\x1b[H>\x13\x11"); // Down End ! Up Home > save quit
    let (_, out) = run("edit", "/ed.txt");
    assert!(out.contains("saved 14 bytes"), "{}", out);
    assert_eq!(read("/ed.txt"), b">Hello\nWorld!\n");
    kernel::serial::inject_rx(b"\x7f\x7fzz\x03\x11\x11"); // edit, Ctrl+C is harmless, quit twice without saving
    let (status, out) = run("edit", "/ed.txt");
    assert_eq!(status, ExitStatus::Exited(0));
    assert!(
        out.contains("use ^Q to quit") && out.contains("press ^Q again"),
        "{}",
        out
    );
    assert_eq!(
        read("/ed.txt"),
        b">Hello\nWorld!\n",
        "unsaved changes must not be written"
    );

    // M24: per-thread x87/SSE state (the programs are built with SSE2)
    let (status, out) = run("fputest", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    assert!(out.contains("pi = 3.14158"), "{}", out);

    // M27: network (QEMU's user-mode network: the gateway 10.0.2.2 answers pings)
    let (status, out) = run("ping", "10.0.2.2 2");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    assert!(
        out.contains("reply from 10.0.2.2: seq=1") && out.contains("2 packets sent, 2 received"),
        "{}",
        out
    );
    let (_, out) = run("netinfo", "");
    assert!(out.contains("IPv4     10.0.2.15") && out.contains("link up"), "{}", out);
    let (status, out) = run("udpecho", "10.0.2.99 9 nobody");
    assert_eq!(
        status,
        ExitStatus::Exited(1),
        "an address nobody has must fail: {}",
        out
    );

    // M28: microsecond clock
    let (status, out) = run("time", "hello");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    let real: f64 = out
        .split("real ")
        .nth(1)
        .and_then(|r| r.split(' ').next())
        .and_then(|r| r.parse().ok())
        .expect("no time printed");
    // hello sleeps 100 ms (10 ticks: 90-110 ms depending on where in a tick it starts)
    assert!(out.contains("Bye!") && (0.085..0.2).contains(&real), "{}", out);
    assert!(run("ping", "10.0.2.2 1").1.contains(" ms\n"));

    // M29: TCP between two user processes over loopback
    let echod_out = Arc::new(Mutex::new(Vec::new()));
    let image = fs::with_root(|fs| fs.read_file("/bin/echod")).unwrap();
    let server = process::spawn("echod", &image, "7300\0-n\01", Output::Capture(echod_out.clone())).unwrap();
    kernel::time::sleep_ms(100); // let it listen
    let (status, out) = run("nc", "10.0.2.15 7300 hello over tcp");
    assert_eq!((status, out.as_str()), (ExitStatus::Exited(0), "hello over tcp\n"));
    assert_eq!(process::wait(server), Some(ExitStatus::Exited(0)));
    let log = String::from_utf8(echod_out.lock().clone()).unwrap();
    assert!(
        log.contains("echoed 15 bytes") && log.contains("served 1 connection"),
        "{}",
        log
    );
    let (status, out) = run("nc", "10.0.2.15 7301 nobody listens");
    assert_eq!(status, ExitStatus::Exited(1));
    assert!(out.contains("connection refused"), "{}", out);

    // M30: DNS client and HTTP over loopback
    let (status, out) = run("host", "--selftest");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    assert!(run("netinfo", "").1.contains("config   DHCP"));
    let image = fs::with_root(|fs| fs.read_file("/bin/httpd")).unwrap();
    let httpd_out = Arc::new(Mutex::new(Vec::new()));
    let server = process::spawn("httpd", &image, "8080\0-n\02", Output::Capture(httpd_out.clone())).unwrap();
    kernel::time::sleep_ms(100);
    let (status, out) = run("wget", "http://10.0.2.15:8080/docs/hello.txt");
    let file = String::from_utf8(fs::with_root(|fs| fs.read_file("/docs/hello.txt")).unwrap()).unwrap();
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    assert!(out.contains(&file) && out.contains("HTTP/1.0 200 OK"), "{}", out);
    let (status, out) = run("wget", "http://10.0.2.15:8080/nothing");
    assert_eq!(status, ExitStatus::Exited(1));
    assert!(out.contains("404 Not Found"), "{}", out);
    assert_eq!(process::wait(server), Some(ExitStatus::Exited(0)));
    let log = String::from_utf8(httpd_out.lock().clone()).unwrap();
    assert!(log.contains("GET /docs/hello.txt -> 200 OK"), "{}", log);

    // M31: variables, stderr redirection, background jobs, scripts
    fs::with_root(|fs| {
        fs.write_file(
            "/test.sh",
            b"#!/bin/sh\n# a comment\necho script arg=$1 count=$# # trailing comment\nset -e\ntrue\nfalse\necho not reached\n",
        )
    })
    .unwrap();
    let (status, out) = run("sh", "/test.sh foo");
    assert_eq!(status, ExitStatus::Exited(1), "set -e stops at false: {}", out);
    assert!(
        out.contains("script arg=foo count=1\n") && !out.contains("not reached"),
        "{}",
        out
    );
    kernel::serial::inject_rx(
        b"export GREETING=hello\rOTHER=world\recho $GREETING ${OTHER} '$GREETING' \\$OTHER\renv GREETING\r\
          false\recho status=$?\rls /nothing 2> /err.txt\rcat /err.txt\rhello in the background &\rwait\r\
          /test.sh bar\rexit 3\r",
    );
    let (status, out) = run("sh", "");
    assert_eq!(status, ExitStatus::Exited(3), "{}", out);
    for expected in [
        "hello world $GREETING $OTHER\n", // expansion, single quotes and \$ keep the dollar
        "> env GREETING\nhello\n",        // inherited by a child process
        "status=1\n",
        "ls: /nothing: no such file or directory", // stderr went to the file
        "[1] ",                                    // job number and pid
        "Hello from user space!",
        "[1] Done     hello in the background",
        "script arg=bar count=1", // "#!" file started by name
    ] {
        assert!(out.contains(expected), "missing {:?} in:\n{}", expected, out);
    }

    // M34: the shell's kill built-in (signal names, %job)
    kernel::serial::inject_rx(b"fault sleep &\rkill -USR1 %1\rwait\rkill -NOPE 1\rexit 0\r");
    let (status, out) = run("sh", "");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    assert!(out.contains("killed: user signal 1"), "{}", out);
    assert!(out.contains("kill: unknown signal 'NOPE'"), "{}", out);

    // M35: shell programming (control flow, functions, $((...)), read, set -e)
    fs::with_root(|fs| fs.write_file("/m35.sh", SCRIPT_M35.as_bytes())).unwrap();
    let (status, out) = run("sh", "/m35.sh");
    assert_eq!(
        status,
        ExitStatus::Exited(1),
        "set -e ends the script at 'false': {}",
        out
    );
    assert_eq!(out, EXPECTED_M35);
    // multi-line input at the prompt, syntax errors, an exit inside a function
    kernel::serial::inject_rx(b"for i in 1 2\rdo echo n$i\rdone\rfi\recho $((2 * 21))\rbye() { exit 5; }\rbye\r");
    let (status, out) = run("sh", "");
    assert_eq!(status, ExitStatus::Exited(5), "{}", out);
    for expected in ["\n> do echo n$i\n> done\nn1\nn2\n", "syntax error near 'fi'", "\n42\n"] {
        assert!(out.contains(expected), "missing {:?} in:\n{}", expected, out);
    }

    // M32: disk speed (informational: prints the time; checks the result)
    let data: Vec<u8> = (0..300_000u32).map(|i| b"abcdefghij\n"[(i % 11) as usize]).collect();
    fs::with_root(|fs| fs.write_file("/bench.txt", &data)).unwrap();
    let (status, out) = run("time", "wc /bench.txt");
    assert_eq!(status, ExitStatus::Exited(0), "{}", out);
    assert!(out.contains(" 300000 /bench.txt"), "{}", out);
    serial_println!("  BENCH reading 300 KB: {}", out.lines().last().unwrap_or(""));
    let t0 = kernel::time::uptime_us();
    let mut buf = alloc::vec![0u8; 4096];
    let mut off = 0u64;
    loop {
        let n = fs::with_root(|fs| fs.read_at("/bench.txt", off, &mut buf)).unwrap();
        if n == 0 {
            break;
        }
        off += n as u64;
    }
    serial_println!(
        "  BENCH kernel read_at 4 KiB chunks: {} us for {} bytes",
        kernel::time::uptime_us() - t0,
        off
    );

    assert_eq!(process::count(), 0);
    serial_println!("[ok]");
    exit(ExitCode::Success)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}

const SCRIPT_M35: &str = r#"#!/bin/sh
# M35: control flow, functions, arithmetic
x=3
if [ $x -gt 2 ]; then echo big; elif [ $x -eq 2 ]; then echo two; else echo small; fi
if test -d /docs && ! test -f /docs; then echo "docs is a directory"; fi
i=0
while [ $i -lt 5 ]; do
  i=$((i + 1))
  if [ $i -eq 2 ]; then continue; fi
  if [ $i -eq 4 ]; then break; fi
  echo "round $i"
done
until [ $i -eq 0 ]; do i=$((i - 1)); done; echo "down to $i"
for f in apple "big banana" cherry; do echo "fruit: $f"; done
greet() {
  echo "hello $1 ($# args)"
  return 3
}
greet world two
echo "greet returned $?"
sum() { echo $(( $1 + $2 * 2 )); }
sum 1 4
false || echo "or works"
true && echo "and works"
false && echo "not printed"
echo a; echo b
for n in 1 2 3; do echo "n$n"; done | sort -r
echo "x y z" | { read a b; echo "a=$a b=$b"; }
cat /docs/hello.txt | while read first rest; do echo "first word: $first"; done
for a in 1 2; do for b in 1 2 3; do if [ $b -eq 2 ]; then continue 2; fi; echo "$a.$b"; done; done
for a in 1 2; do for b in 1 2; do break 2; done; echo never; done; echo "after break 2"
echo $((7 / 2)) $((7 % 3)) $((-(2 + 3) * 2)) $((1 < 2)) $((2 == 3 || 1))
{ echo grouped; echo output; } > /grp.txt
cat /grp.txt
show() { shift; echo "rest: $1"; }
show a b
x=outer; change() { x=inner; }; change; echo "x is $x"
deep() { if [ $1 -gt 0 ]; then deep $(($1 - 1)); else echo "bottom reached"; fi; }
deep 20
while read word; do echo "read: $word"; done < /grp.txt
[ abc = abc ] && [ abc != abd ] && [ -n abc ] && [ -z "" ] && echo "tests ok"
for f in /docs/*.txt /nothing/*; do echo "glob: $f"; done
echo "*" \*
set -e
if false; then echo no; fi
false || true
echo "set -e spares conditions"
false
echo "not reached"
"#;

const EXPECTED_M35: &str = "big\n\
docs is a directory\n\
round 1\n\
round 3\n\
down to 0\n\
fruit: apple\n\
fruit: big banana\n\
fruit: cherry\n\
hello world (2 args)\n\
greet returned 3\n\
9\n\
or works\n\
and works\n\
a\n\
b\n\
n3\n\
n2\n\
n1\n\
a=x b=y z\n\
first word: Hello\n\
1.1\n\
2.1\n\
after break 2\n\
3 1 -10 1 1\n\
grouped\n\
output\n\
rest: b\n\
x is inner\n\
bottom reached\n\
read: grouped\n\
read: output\n\
tests ok\n\
glob: /docs/hello.txt\n\
glob: /nothing/*\n\
* *\n\
set -e spares conditions\n\
";
