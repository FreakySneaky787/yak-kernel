//! Handlers for CPU exceptions and hardware interrupts (IRQs), and the kernel's IDT.
//!
//! An exception in our kernel is a bug - except `int3`, the breakpoint, which code raises on purpose. So the
//! breakpoint handler reports and returns; all others report what happened and panic. That is still a big step
//! from chapter 4: instead of a silent reset (triple fault) we get the exception, the address and the registers.
//!
//! The handlers print with `serial_println!` only: an exception can hit code that holds the console lock, and a
//! handler that waits for that lock would wait forever. Since chapter 6 that is no longer theory: the timer
//! interrupts the kernel 100 times a second, wherever it is.

use crate::gdt::{self, DOUBLE_FAULT_IST};
use crate::idt::{Idt, InterruptStackFrame, read_cr2};
use crate::{pic, pit, serial_println, time};
use core::arch::asm;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Once;

// The vectors of the exceptions we handle (Intel SDM Vol. 3A, table 6-1)
pub const DIVIDE_ERROR: u8 = 0;
pub const BREAKPOINT: u8 = 3;
pub const INVALID_OPCODE: u8 = 6;
pub const DOUBLE_FAULT: u8 = 8;
pub const GENERAL_PROTECTION: u8 = 13;
pub const PAGE_FAULT: u8 = 14;

// The IRQs we handle: the timer, and the two lines on which the PICs report "spurious" interrupts
pub const TIMER_IRQ: u8 = 0;
const SPURIOUS_MASTER_IRQ: u8 = 7;
const SPURIOUS_SLAVE_IRQ: u8 = 15;

/// How often `int3` was handled - the tests use it to see that the handler really ran.
pub static BREAKPOINT_COUNT: AtomicU64 = AtomicU64::new(0);

static IDT: Once<Idt> = Once::new();

/// Builds the IDT (once) and loads it.
pub fn init() {
    let idt = IDT.call_once(|| {
        let mut idt = Idt::new();
        idt.set(DIVIDE_ERROR, divide_error);
        idt.set(BREAKPOINT, breakpoint);
        idt.set(INVALID_OPCODE, invalid_opcode);
        // the one handler that must not use the current stack: it may be what broke
        idt.set_diverging_with_error_code(DOUBLE_FAULT, double_fault)
            .set_ist(DOUBLE_FAULT_IST);
        idt.set_with_error_code(GENERAL_PROTECTION, general_protection);
        idt.set_with_error_code(PAGE_FAULT, page_fault);
        idt.set(pic::vector(TIMER_IRQ), timer);
        idt.set(pic::vector(SPURIOUS_MASTER_IRQ), spurious_master);
        idt.set(pic::vector(SPURIOUS_SLAVE_IRQ), spurious_slave);
        idt
    });
    idt.load();
}

/// Starts the timer: PIC and PIT set up, IRQ 0 let through, and interrupts on (`sti`). Needs `init` first - the
/// first tick arrives 10 ms later and must find its gate.
pub fn start_timer() {
    pic::disable_local_apic();
    pic::init();
    pit::init(time::TICKS_PER_SECOND);
    pic::unmask(TIMER_IRQ);
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Whether interrupts are on: bit 9 (IF) of RFLAGS.
#[cfg(test)]
pub fn enabled() -> bool {
    let rflags: u64;
    unsafe { asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags)) };
    rflags & (1 << 9) != 0
}

/// The loaded IDT (`init` ran) - for the tests.
#[cfg(test)]
pub fn idt() -> &'static Idt {
    IDT.get().expect("interrupts::init ran")
}

extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    BREAKPOINT_COUNT.fetch_add(1, Ordering::Relaxed);
    serial_println!("EXCEPTION: BREAKPOINT at {:#x}", frame.rip);
}

extern "x86-interrupt" fn timer(_frame: InterruptStackFrame) {
    time::tick();
    // without it, the PIC would never send the next tick
    pic::end_of_interrupt(TIMER_IRQ);
}

// A *spurious* interrupt: a device raised its line and dropped it again before the PIC could deliver it. The PIC
// then reports its lowest-priority line (7 or 15) - but no handler is running, so it must not get an end of
// interrupt (the in-service bit tells). For line 15 the master did forward something and does need one.
extern "x86-interrupt" fn spurious_master(_frame: InterruptStackFrame) {
    if pic::in_service().0 & (1 << 7) != 0 {
        pic::end_of_interrupt(SPURIOUS_MASTER_IRQ); // a real IRQ 7 after all
    }
}

extern "x86-interrupt" fn spurious_slave(_frame: InterruptStackFrame) {
    if pic::in_service().1 & (1 << 7) != 0 {
        pic::end_of_interrupt(SPURIOUS_SLAVE_IRQ);
    } else {
        pic::end_of_interrupt(2); // only the master: line 2 is where the slave hangs
    }
}

extern "x86-interrupt" fn divide_error(frame: InterruptStackFrame) {
    panic!("EXCEPTION: DIVIDE ERROR\n{:?}", frame);
}

extern "x86-interrupt" fn invalid_opcode(frame: InterruptStackFrame) {
    panic!("EXCEPTION: INVALID OPCODE\n{:?}", frame);
}

extern "x86-interrupt" fn general_protection(frame: InterruptStackFrame, error_code: u64) {
    // the error code names a segment selector or - bit 1 set - an IDT entry: (code >> 3) is its number
    panic!(
        "EXCEPTION: GENERAL PROTECTION FAULT (code {:#x})\n{:?}",
        error_code, frame
    );
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, error_code: u64) {
    panic!(
        "EXCEPTION: PAGE FAULT at address {:#018x}\n  cause: {}\n{:?}",
        read_cr2(),
        PageFaultError(error_code),
        frame
    );
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, error_code: u64) -> ! {
    let rsp: u64;
    unsafe { asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags)) };
    let on_ist = if gdt::on_double_fault_stack(rsp) { "yes" } else { "no" };
    serial_println!("running on the double fault stack: {} (rsp={:#x})", on_ist, rsp);
    // a double fault cannot be returned from: the CPU does not say how far the first exception got
    panic!(
        "EXCEPTION: DOUBLE FAULT (code {:#x}), last page fault at {:#x}\n{:?}",
        error_code,
        read_cr2(),
        frame
    );
}

/// The page fault error code in words (Intel SDM Vol. 3A, section 4.7).
pub struct PageFaultError(pub u64);

impl fmt::Display for PageFaultError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let c = self.0;
        write!(
            f,
            "{}, {}, {}",
            if c & 1 != 0 {
                "protection violation"
            } else {
                "page not present"
            },
            if c & 2 != 0 { "write" } else { "read" },
            if c & 4 != 0 { "user mode" } else { "kernel mode" },
        )?;
        if c & 16 != 0 {
            write!(f, ", instruction fetch")?;
        }
        Ok(())
    }
}

/// An address no page table maps (checked: the page fault it causes says "page not present").
const UNMAPPED: u64 = 0xdeadbeef;

/// Writes to an unmapped address: a page fault the handler reports.
pub fn provoke_page_fault() -> ! {
    unsafe { (UNMAPPED as *mut u8).write_volatile(42) };
    unreachable!("the write to {:#x} did not fault", UNMAPPED);
}

/// Points the stack at unmapped memory and pushes: a page fault, whose frame the CPU cannot push either -
/// a double fault, which only survives on its own stack.
pub fn provoke_double_fault() -> ! {
    unsafe { asm!("mov rsp, {}", "push rax", in(reg) UNMAPPED & !0xF, options(noreturn)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn breakpoints_return() {
        let before = BREAKPOINT_COUNT.load(Ordering::Relaxed);
        unsafe { asm!("int3", "int3") };
        assert_eq!(BREAKPOINT_COUNT.load(Ordering::Relaxed), before + 2);
    }

    #[test_case]
    fn the_double_fault_gate_switches_to_ist1() {
        let gate = idt().entry(DOUBLE_FAULT);
        assert!(gate.is_present());
        assert_eq!(gate.ist(), DOUBLE_FAULT_IST);
        assert_eq!(gate.address(), double_fault as usize as u64);
    }

    #[test_case]
    fn only_the_vectors_we_handle_are_present() {
        for vector in 0..=255u8 {
            let handled = [
                DIVIDE_ERROR,
                BREAKPOINT,
                INVALID_OPCODE,
                DOUBLE_FAULT,
                GENERAL_PROTECTION,
                PAGE_FAULT,
                pic::vector(TIMER_IRQ),
                pic::vector(SPURIOUS_MASTER_IRQ),
                pic::vector(SPURIOUS_SLAVE_IRQ),
            ];
            assert_eq!(
                idt().entry(vector).is_present(),
                handled.contains(&vector),
                "vector {}",
                vector
            );
        }
    }

    #[test_case]
    fn the_page_fault_error_code_in_words() {
        struct Text([u8; 64], usize);
        impl fmt::Write for Text {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.0[self.1..self.1 + s.len()].copy_from_slice(s.as_bytes());
                self.1 += s.len();
                Ok(())
            }
        }
        let mut text = Text([0; 64], 0);
        fmt::Write::write_fmt(&mut text, format_args!("{}", PageFaultError(0b10))).unwrap();
        assert_eq!(
            core::str::from_utf8(&text.0[..text.1]).unwrap(),
            "page not present, write, kernel mode"
        );
    }
}
