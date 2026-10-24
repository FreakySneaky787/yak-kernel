//! Integrationstest: Bootet der Kernel und liefert Limine die Antworten,
//! die wir anfordern?

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::testing::runner)]
#![reexport_test_harness_main = "test_main"]

use kernel::boot;

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    test_main();
    kernel::qemu::exit(kernel::qemu::ExitCode::Success)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}

#[test_case]
fn base_revision_supported() {
    assert!(boot::BASE_REVISION.is_supported());
}

#[test_case]
fn framebuffer_present() {
    let resp = boot::FRAMEBUFFER.response().expect("keine Framebuffer-Antwort");
    let fb = resp.framebuffers().first().expect("kein Framebuffer");
    assert!(fb.width > 0 && fb.height > 0);
}

#[test_case]
fn hhdm_present() {
    let hhdm = boot::HHDM.response().expect("keine HHDM-Antwort");
    assert!(hhdm.offset >= 0xffff_8000_0000_0000);
}

#[test_case]
fn memmap_has_usable_memory() {
    let mm = boot::MEMMAP.response().expect("keine Memory-Map-Antwort");
    let usable = mm
        .entries()
        .iter()
        .filter(|e| e.type_ == limine::memmap::MEMMAP_USABLE)
        .count();
    assert!(usable > 0);
}

#[test_case]
fn serial_println_does_not_panic() {
    kernel::serial_println!("Testausgabe ueber COM1");
}
