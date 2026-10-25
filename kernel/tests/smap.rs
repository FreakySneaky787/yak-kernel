//! Integration test: with SMAP the kernel faults when it touches a user
//! page outside `cpu::user_access`, and with SMEP when it jumps into one.
//! Runs in two phases; the page fault handler checks each fault and moves
//! on to the next phase (by changing the return address).

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::arch::x86_64::cpu;
use kernel::arch::x86_64::idt::{Idt, InterruptStackFrame, read_cr2};
use kernel::arch::x86_64::interrupts::vector;
use kernel::mem::VirtAddr;
use kernel::mem::address_space::AddressSpace;
use kernel::mem::paging::{self, Flags};
use kernel::qemu::{ExitCode, exit};
use kernel::serial_println;
use spin::Once;

const DATA: u64 = 0x40_0000;
const CODE: u64 = 0x40_1000;

static TEST_IDT: Once<Idt> = Once::new();
static PHASE: AtomicU32 = AtomicU32::new(0);

// error code bits: 0 present, 1 write, 2 user mode, 4 instruction fetch
extern "x86-interrupt" fn test_page_fault(mut frame: InterruptStackFrame, code: u64) {
    let cr2 = read_cr2();
    serial_println!("page fault: cr2={:#x} code={:#b}", cr2, code);
    match PHASE.load(Ordering::SeqCst) {
        // SMAP: a present user page, read in kernel mode
        1 if cr2 == DATA && code & 0b10111 == 0b00001 => {
            serial_println!("[ok]");
            unsafe { core::ptr::write_volatile(&mut frame.rip, smep_phase as *const () as u64) };
        }
        // SMEP: an instruction fetch from a present user page in kernel mode
        2 if cr2 == CODE && code & 0b10101 == 0b10001 => {
            serial_println!("[ok]");
            exit(ExitCode::Success)
        }
        _ => exit(ExitCode::Failed),
    }
}

extern "C" fn smep_phase() -> ! {
    serial_println!("smap::kernel_jump_into_user_page_faults ... ");
    PHASE.store(2, Ordering::SeqCst);
    let f: extern "C" fn() = unsafe { core::mem::transmute(CODE as *const ()) };
    f();
    serial_println!("[no page fault]");
    exit(ExitCode::Failed)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    let (smep, smap) = cpu::protection();
    serial_println!("smap::protection_enabled ... SMEP {} SMAP {}", smep, smap);
    if !smep || !smap {
        // other CPU models (KERNEL_CPU=qemu64) lack them: nothing to test
        serial_println!("[skipped: the CPU has no SMEP/SMAP]");
        exit(ExitCode::Success);
    }
    kernel::arch::x86_64::disable_interrupts(); // stay on this CPU and this CR3
    let mut space = AddressSpace::new().unwrap();
    space
        .map_fresh(VirtAddr(DATA), 1, Flags::WRITABLE | Flags::NO_EXECUTE)
        .unwrap();
    space.map_fresh(VirtAddr(CODE), 1, Flags::WRITABLE).unwrap();
    unsafe { paging::write_cr3(space.root()) };

    serial_println!("smap::user_access_works ... ");
    let back = cpu::user_access(|| unsafe {
        (DATA as *mut u64).write_volatile(42);
        (CODE as *mut u8).write_volatile(0xC3); // ret
        (DATA as *const u64).read_volatile()
    });
    serial_println!("{}", if back == 42 { "[ok]" } else { "[failed]" });

    let idt = TEST_IDT.call_once(|| {
        let mut idt = Idt::new();
        idt.set_with_error_code(vector::PAGE_FAULT, test_page_fault);
        idt
    });
    idt.load();
    serial_println!("smap::kernel_read_of_user_page_faults ... ");
    PHASE.store(1, Ordering::SeqCst);
    let v = unsafe { (DATA as *const u64).read_volatile() };
    serial_println!("[no page fault, read {}]", v);
    exit(ExitCode::Failed)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}
