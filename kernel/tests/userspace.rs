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

    assert_eq!(process::count(), 0);
    serial_println!("[ok]");
    exit(ExitCode::Success)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}
