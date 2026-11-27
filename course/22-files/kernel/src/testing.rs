//! A test framework for a kernel. `cargo test` cannot run our tests as normal programs - there is no
//! operating system to run them on. Instead the compiler collects every `#[test_case]` function into a
//! list and hands it to `runner`, inside a test build of the kernel that QEMU boots.

use crate::qemu::{self, ExitCode};
use crate::{serial_print, serial_println};
use core::panic::PanicInfo;

/// Anything that can run as a test: every `fn()` - and it prints its own name.
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

/// Called by the generated `test_main` with all `#[test_case]` functions of the crate.
pub fn runner(tests: &[&dyn Testable]) -> ! {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    serial_println!("All tests passed.");
    qemu::exit(ExitCode::Success)
}

/// In a test build a panic means: the current test failed.
pub fn panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}", info);
    qemu::exit(ExitCode::Failed)
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1 + 1, 2);
}
