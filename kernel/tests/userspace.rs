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
    // the stack overflow runs into the unmapped area below the user stack
    let below_stack = alloc::format!(
        "page fault at {:#x}",
        process::USER_STACK_TOP - process::USER_STACK_PAGES as u64 * 4096 - 8
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
          mem > /mem.txt\rcat /mem.txt | grep Physical | wc\rgrep nothing /r.txt | wc\recho > \rexit\r",
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

    assert_eq!(process::count(), 0);
    serial_println!("[ok]");
    exit(ExitCode::Success)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}
