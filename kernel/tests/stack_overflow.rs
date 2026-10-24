//! Integrationstest: Ein kaputter Stack führt zu einem Double Fault, der auf
//! dem IST-Stack aus dem TSS behandelt wird statt in einem Triple Fault zu enden.
//!
//! Limine legt unseren Stack in komplett gemappten Speicher – ein echter
//! Overflow würde dort nicht faulten. Wir simulieren ihn deshalb, indem wir RSP
//! auf eine ungemappte Adresse setzen und `push` ausführen: Page Fault → die CPU
//! kann den Exception-Frame nicht auf den Stack legen → Double Fault.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use kernel::arch::x86_64::gdt::{self, DOUBLE_FAULT_IST};
use kernel::arch::x86_64::idt::{Idt, InterruptStackFrame};
use kernel::arch::x86_64::interrupts::vector;
use kernel::qemu::{ExitCode, exit};
use kernel::serial_println;
use spin::Once;

/// Kanonische Adresse in der unteren Hälfte, die Limine nicht mappt.
const UNMAPPED: u64 = 0x0000_4000_0000_0000;

static TEST_IDT: Once<Idt> = Once::new();

extern "x86-interrupt" fn test_double_fault(frame: InterruptStackFrame, _code: u64) -> ! {
    // Wir müssen auf dem IST-Stack laufen
    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp) };
    let top = gdt::double_fault_stack_top();
    let on_ist = rsp <= top && rsp > top - 5 * 4096;
    serial_println!("double fault abgefangen, rip={:#x}, rsp auf IST: {}", frame.rip, on_ist);
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
