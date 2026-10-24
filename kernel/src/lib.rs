//! KernelTry-Kernel als Library: `main.rs`, die Unit-Tests und die
//! Integrationstests in `tests/` nutzen alle denselben Code.

#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]

pub mod arch;
pub mod boot;
pub mod qemu;
pub mod serial;
pub mod testing;

/// Gemeinsame Initialisierung für Kernel und alle Test-Binaries.
pub fn init() {
    serial::init();
    if !boot::BASE_REVISION.is_supported() {
        serial_println!(
            "Limine unterstuetzt Base Revision nicht (aktiv: {:?})",
            boot::BASE_REVISION.actual_revision()
        );
        arch::x86_64::halt_loop();
    }
}

/// Einsprungpunkt für `cargo test --lib`.
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
