//! Integration test: a write to unmapped memory raises a
//! page fault; CR2 holds the address and the error code says "not
//! present + write".

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
    // bit 0 = 0 (not present), bit 1 = 1 (write), bit 2 = 0 (kernel)
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
    // the test IDT has no IRQ vectors: a timer tick would raise #GP (bug #10)
    kernel::arch::x86_64::disable_interrupts();
    idt.load();

    unsafe { (UNMAPPED as *mut u8).write_volatile(42) };
    serial_println!("[no page fault]");
    exit(ExitCode::Failed)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}
