//! Integration test: a real stack overflow (endless recursion) on a kernel
//! stack with a guard page. The overflow hits the unmapped guard page ->
//! page fault -> the CPU cannot push the exception frame onto the broken
//! stack -> double fault, handled on the IST stack instead of a triple fault.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::sync::atomic::{AtomicU64, Ordering};
use kernel::arch::x86_64::gdt::DOUBLE_FAULT_IST;
use kernel::arch::x86_64::idt::{Idt, InterruptStackFrame, read_cr2};
use kernel::arch::x86_64::interrupts::{install_exception_handlers, vector};
use kernel::mem::stack::alloc_stack;
use kernel::qemu::{ExitCode, exit};
use kernel::serial_println;
use spin::Once;

static TEST_IDT: Once<Idt> = Once::new();
static GUARD_PAGE: AtomicU64 = AtomicU64::new(0);

extern "x86-interrupt" fn test_double_fault(_frame: InterruptStackFrame, _code: u64) -> ! {
    let cr2 = read_cr2();
    let guard = GUARD_PAGE.load(Ordering::Relaxed);
    let in_guard = (guard..guard + 4096).contains(&cr2);
    serial_println!(
        "double fault caught, cr2={:#x}, guard page={:#x}, hit guard: {}",
        cr2,
        guard,
        in_guard
    );
    if in_guard {
        serial_println!("[ok]");
        exit(ExitCode::Success)
    } else {
        exit(ExitCode::Failed)
    }
}

#[allow(unconditional_recursion)]
fn recurse(depth: u64) -> u64 {
    let buffer = [depth; 16]; // make each frame a bit bigger
    core::hint::black_box(&buffer);
    recurse(depth + 1) + 1
}

extern "C" fn on_guarded_stack() -> ! {
    let depth = recurse(0);
    serial_println!("[recursion returned?! depth {}]", depth);
    exit(ExitCode::Failed)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    serial_println!("stack_overflow::guard_page_catches_recursion ... ");

    let idt = TEST_IDT.call_once(|| {
        let mut idt = Idt::new();
        install_exception_handlers(&mut idt);
        idt.set_diverging_with_error_code(vector::DOUBLE_FAULT, test_double_fault)
            .set_ist(DOUBLE_FAULT_IST);
        idt
    });
    // the test IDT has no IRQ vectors: a timer tick would raise #GP (bug #10)
    kernel::arch::x86_64::disable_interrupts();
    idt.load();

    let stack = alloc_stack(4).expect("cannot allocate test stack");
    GUARD_PAGE.store(stack.guard.as_u64(), Ordering::Relaxed);
    unsafe { kernel::arch::x86_64::switch_stack(stack.top.as_u64(), on_guarded_stack) }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}
