//! The KernelTry kernel as a library: `main.rs`, the unit tests and the
//! integration tests in `tests/` all share this code.

#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![feature(abi_x86_interrupt)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

pub mod arch;
pub mod boot;
pub mod console;
pub mod drivers;
pub mod font;
pub mod input;
pub mod mem;
pub mod qemu;
pub mod ringbuf;
pub mod serial;
pub mod testing;
pub mod time;

/// Common initialization for the kernel and all test binaries.
pub fn init() {
    serial::init();
    // Check the protocol revision before touching any Limine response
    if !boot::BASE_REVISION.is_supported() {
        serial_println!(
            "Limine does not support our base revision (active: {:?})",
            boot::BASE_REVISION.actual_revision()
        );
        arch::x86_64::halt_loop();
    }
    arch::x86_64::gdt::init();
    arch::x86_64::interrupts::init();
    mem::init();
    arch::x86_64::irq::init();
    console::init();
    arch::x86_64::enable_interrupts();
}

/// Entry point for `cargo test --lib`.
#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    init();
    test_main();
    qemu::exit(qemu::ExitCode::Success)
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    testing::test_panic_handler(info)
}
