//! Minimal test framework for `#![feature(custom_test_frameworks)]`.
//! Every `#[test_case]` function is collected by the compiler and passed to `runner`.
//! Output goes to the serial port, the result is reported via `qemu::exit`.

use crate::qemu::{self, ExitCode};
use crate::{serial_print, serial_println};
use core::panic::PanicInfo;

pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        serial_print!("{} ... ", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn runner(tests: &[&dyn Testable]) -> ! {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    serial_println!("All tests passed.");
    qemu::exit(ExitCode::Success)
}

/// Panic handler for test binaries: report the current test as failed.
pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}", info);
    qemu::exit(ExitCode::Failed)
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1 + 1, 2);
}
