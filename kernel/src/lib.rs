//! The KernelTry kernel as a library: `main.rs`, the unit tests and the
//! integration tests in `tests/` all share this code.

#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![feature(abi_x86_interrupt)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

pub mod acpi;
pub mod arch;
pub mod boot;
pub mod console;
pub mod drivers;
pub mod font;
pub mod fs;
pub mod input;
pub mod mem;
pub mod qemu;
pub mod ringbuf;
pub mod serial;
pub mod shell;
pub mod task;
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
    task::init();
    drivers::init_storage();
    if let Err(e) = fs::mount_root() {
        serial_println!("fs: root not mounted: {}", e);
    }
    arch::x86_64::enable_interrupts();
}

/// Reports a panic on the serial port and - if the console is not locked -
/// on the screen, then halts. Used by the boot binary's panic handler.
pub fn panic_report(info: &core::panic::PanicInfo) -> ! {
    arch::x86_64::disable_interrupts();
    serial_println!("KERNEL PANIC: {}", info);
    // try_lock: the panic may have happened while the console was locked
    if let Some(mut guard) = console::CONSOLE.try_lock() {
        if let Some(console) = guard.as_mut() {
            use core::fmt::Write;
            write!(
                console,
                "\n\x1b[1;37;41m KERNEL PANIC \x1b[0m \x1b[91m{}\x1b[0m\nSystem halted.\n",
                info
            )
            .ok();
        }
    }
    arch::x86_64::halt_loop()
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
