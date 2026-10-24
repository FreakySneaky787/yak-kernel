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

/// Runs `/bin/<name>` and returns its exit status and captured output.
fn run(name: &str, args: &str) -> (ExitStatus, String) {
    let path = alloc::format!("/bin/{}", name);
    let image = fs::with_root(|fs| fs.read_file(&path)).expect("program missing on the test disk (make user?)");
    let out = Arc::new(Mutex::new(Vec::new()));
    let status = process::run(name, &image, args, Output::Capture(out.clone())).unwrap();
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

    assert_eq!(process::count(), 0);
    serial_println!("[ok]");
    exit(ExitCode::Success)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}
