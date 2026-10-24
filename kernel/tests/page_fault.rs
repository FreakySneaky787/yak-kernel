//! Integrationstest: Ein Schreibzugriff auf ungemappten Speicher löst einen
//! Page Fault aus; CR2 enthält die Adresse, der Fehlercode sagt „nicht
//! vorhanden + Schreiben“.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use kernel::arch::x86_64::idt::{Idt, InterruptStackFrame, read_cr2};
use kernel::arch::x86_64::interrupts::vector;
use kernel::qemu::{ExitCode, exit};
use kernel::serial_println;
use spin::Once;

const UNMAPPED: u64 = 0x0000_4000_0000_1234;

static TEST_IDT: Once<Idt> = Once::new();

extern "x86-interrupt" fn test_page_fault(_frame: InterruptStackFrame, code: u64) {
    let cr2 = read_cr2();
    serial_println!("page fault: cr2={:#x} code={:#b}", cr2, code);
    // Bit 0 = 0 (nicht vorhanden), Bit 1 = 1 (Schreiben), Bit 2 = 0 (Kernel)
    if cr2 == UNMAPPED && code & 0b111 == 0b010 {
        serial_println!("[ok]");
        exit(ExitCode::Success)
    } else {
        exit(ExitCode::Failed)
    }
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    serial_println!("page_fault::write_to_unmapped ... ");

    let idt = TEST_IDT.call_once(|| {
        let mut idt = Idt::new();
        idt.set_with_error_code(vector::PAGE_FAULT, test_page_fault);
        idt
    });
    idt.load();

    unsafe { (UNMAPPED as *mut u8).write_volatile(42) };
    serial_println!("[kein page fault]");
    exit(ExitCode::Failed)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}
