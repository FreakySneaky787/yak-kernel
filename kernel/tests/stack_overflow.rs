//! Integration test: a broken stack leads to a double fault that is handled on
//! the IST stack from the TSS instead of ending in a triple fault.
//!
//! Limine places our stack in fully mapped memory - a real
//! overflow would not fault there. So we simulate it by pointing RSP
//! at an unmapped address and executing `push`: page fault -> the CPU
//! cannot push the exception frame onto the stack -> double fault.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use kernel::arch::x86_64::gdt::{self, DOUBLE_FAULT_IST};
use kernel::arch::x86_64::idt::{Idt, InterruptStackFrame};
use kernel::arch::x86_64::interrupts::vector;
use kernel::qemu::{ExitCode, exit};
use kernel::serial_println;
use spin::Once;

/// Canonical lower-half address that Limine does not map.
const UNMAPPED: u64 = 0x0000_4000_0000_0000;

static TEST_IDT: Once<Idt> = Once::new();

extern "x86-interrupt" fn test_double_fault(frame: InterruptStackFrame, _code: u64) -> ! {
    // We must be running on the IST stack
    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp) };
    let top = gdt::double_fault_stack_top();
    let on_ist = rsp <= top && rsp > top - 5 * 4096;
    serial_println!("double fault caught, rip={:#x}, rsp on IST: {}", frame.rip, on_ist);
    if on_ist {
        serial_println!("[ok]");
        exit(ExitCode::Success)
    } else {
        exit(ExitCode::Failed)
    }
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    serial_println!("stack_overflow::double_fault_on_ist ... ");

    let idt = TEST_IDT.call_once(|| {
        let mut idt = Idt::new();
        idt.set_diverging_with_error_code(vector::DOUBLE_FAULT, test_double_fault).set_ist(DOUBLE_FAULT_IST);
        idt
    });
    idt.load();

    unsafe { core::arch::asm!("mov rsp, {}", "push rax", in(reg) UNMAPPED, options(noreturn)) };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}
