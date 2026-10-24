//! Integration test: checks that a panic inside a test is detected.
//! No test harness: panic = success, returning normally = failure.

#![no_std]
#![no_main]

use kernel::qemu::{ExitCode, exit};
use kernel::serial_println;

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    serial_println!("should_panic::should_fail ... ");
    should_fail();
    serial_println!("[test did not panic]");
    exit(ExitCode::Failed)
}

fn should_fail() {
    assert_eq!(0, 1);
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    serial_println!("[ok]");
    exit(ExitCode::Success)
}
