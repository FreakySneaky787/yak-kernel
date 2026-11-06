//! Handlers for CPU exceptions and hardware interrupts (IRQs), and the kernel's IDT.
//!
//! An exception in our kernel is a bug - except `int3`, the breakpoint, which code raises on purpose. So the
//! breakpoint handler reports and returns; all others report what happened and panic. That is still a big step
//! from chapter 4: instead of a silent reset (triple fault) we get the exception, the address and the registers.
//!
//! Since chapter 12 the hardware interrupts come through the APIC: `start` reads the MADT, moves the PIC out of the
//! way, and routes the timer and the keyboard through the I/O APIC to the local APIC.
//!
//! The handlers print with `serial_println!` only: an exception can hit code that holds the console lock, and a
//! handler that waits for that lock would wait forever. Since chapter 6 that is no longer theory: the timer
//! interrupts the kernel 100 times a second, wherever it is.

use crate::acpi::{self, Acpi};
use crate::gdt::{self, DOUBLE_FAULT_IST};
use crate::idt::{Idt, InterruptStackFrame, read_cr2};
use crate::{ioapic, keyboard, lapic, pic, pit, serial_println, smp, task, time, user};
use core::arch::asm;
use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use spin::Once;

// The vectors of the exceptions we handle (Intel SDM Vol. 3A, table 6-1)
pub const DIVIDE_ERROR: u8 = 0;
pub const BREAKPOINT: u8 = 3;
pub const INVALID_OPCODE: u8 = 6;
pub const DOUBLE_FAULT: u8 = 8;
pub const GENERAL_PROTECTION: u8 = 13;
pub const PAGE_FAULT: u8 = 14;

// The ISA interrupt lines we use, and the vectors they raise: the same ones as with the PIC in chapters 6 and 7
pub const TIMER_IRQ: u8 = 0;
pub const KEYBOARD_IRQ: u8 = 1;
pub const TIMER_VECTOR: u8 = 32;
pub const KEYBOARD_VECTOR: u8 = 33;
/// The inter-processor interrupt that wakes another CPU from `hlt` (smp.rs). Its handler only ends it.
pub const WAKE_VECTOR: u8 = 0xF0;

/// Over how many PIT ticks the local APIC timer is measured: 10, a tenth of a second.
const CALIBRATION_TICKS: u64 = 10;

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
        idt.set(TIMER_VECTOR, timer);
        idt.set(KEYBOARD_VECTOR, keyboard);
        idt.set(lapic::SPURIOUS_VECTOR, spurious);
        idt.set(WAKE_VECTOR, wake);
        idt.set(crate::ahci::VECTOR, disk);
        idt.set_address(user::SYSCALL_VECTOR, user::syscall_entry as *const () as u64)
            .set_user_callable();
        idt
    });
    idt.load();
}

/// Loads the IDT that `init` built - on the other CPUs, which all share it.
pub fn load_idt() {
    IDT.get().expect("interrupts::init ran").load();
}

/// What `start` found and did - for the boot output.
pub struct Started {
    pub acpi: Acpi,
    /// The APIC ID of our CPU, which the I/O APIC delivers to.
    pub cpu: u8,
    /// How far the local APIC timer counts in one tick (10 ms).
    pub counts_per_tick: u32,
}

/// Starts hardware interrupts through the APIC: the timer at 100 Hz from the local APIC, the keyboard through the
/// I/O APIC, interrupts on (`sti`). Needs `init` and memory (`mem::init`): the registers must be mapped, and the
/// MADT's lists live on the heap.
pub fn start() -> Started {
    let acpi = acpi::read().expect("ACPI tables with a MADT");
    let madt = &acpi.madt;
    // the PIC stays, but silent: moved off the exception vectors and every line masked
    pic::init();
    lapic::init(madt.local_apic);
    ioapic::init(madt.io_apic, madt.io_apic_gsi_base);
    let cpu = lapic::id();

    // The PIT is our clock for a moment: its IRQ 0 arrives at the I/O APIC input the MADT says (2, not 0)
    pit::init(time::TICKS_PER_SECOND);
    ioapic::route(madt.gsi(TIMER_IRQ), TIMER_VECTOR, cpu);
    unsafe { asm!("sti", options(nomem, nostack)) };
    // ... long enough to measure the local APIC timer against it; then the APIC timer takes over the same vector
    let counts_per_tick = lapic::measure_timer(CALIBRATION_TICKS);
    ioapic::mask(madt.gsi(TIMER_IRQ));
    COUNTS_PER_TICK.store(counts_per_tick, Ordering::Relaxed);
    start_timer_here();

    keyboard::init();
    ioapic::route(madt.gsi(KEYBOARD_IRQ), KEYBOARD_VECTOR, cpu);
    Started {
        acpi,
        cpu,
        counts_per_tick,
    }
}

/// How far the local APIC timer counts in one tick - measured once on the BSP; all CPUs of a machine share the clock
/// that drives their local APIC timers.
static COUNTS_PER_TICK: AtomicU32 = AtomicU32::new(0);
/// Timer interrupts so far, per CPU - for the tests.
pub static TIMER_INTERRUPTS: [AtomicU64; smp::MAX_CPUS] = [const { AtomicU64::new(0) }; smp::MAX_CPUS];

/// Starts the local APIC timer of the CPU this runs on, at 100 Hz (needs `start` on the BSP first).
pub fn start_timer_here() {
    lapic::start_timer(TIMER_VECTOR, COUNTS_PER_TICK.load(Ordering::Relaxed));
}

/// Whether interrupts are on: bit 9 (IF) of RFLAGS.
pub fn enabled() -> bool {
    let rflags: u64;
    unsafe { asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags)) };
    rflags & (1 << 9) != 0
}

/// Runs `f` with interrupts off, and turns them back on afterwards only if they were on before - so it can be
/// nested, and used in interrupt handlers, where they are off already.
pub fn without_interrupts<T>(f: impl FnOnce() -> T) -> T {
    let were_on = enabled();
    unsafe { asm!("cli", options(nomem, nostack)) };
    let result = f();
    if were_on {
        unsafe { asm!("sti", options(nomem, nostack)) };
    }
    result
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
    // every CPU has a timer now, but the clock is counted once: by the BSP's
    if smp::cpu() == 0 {
        time::tick();
    }
    TIMER_INTERRUPTS[smp::cpu()].fetch_add(1, Ordering::Relaxed);
    // without it, the local APIC would never deliver the next tick - and it must come first: `on_tick` may switch to
    // another thread, and this handler finishes only when this thread runs again
    lapic::end_of_interrupt();
    task::on_tick();
}

extern "x86-interrupt" fn keyboard(_frame: InterruptStackFrame) {
    keyboard::handle_interrupt();
    lapic::end_of_interrupt();
}

/// The SATA controller finished a command (chapter 19). MSI goes straight to the local APIC: no I/O APIC involved.
extern "x86-interrupt" fn disk(_frame: InterruptStackFrame) {
    crate::ahci::handle_interrupt();
    lapic::end_of_interrupt();
}

extern "x86-interrupt" fn wake(_frame: InterruptStackFrame) {
    lapic::end_of_interrupt(); // the wake-up itself is the point: `hlt` is over, the idle thread looks at the queue
    smp::run_job(); // and a job `smp::run_on` sent, if any
}

// A *spurious* interrupt: one that vanished before the local APIC could deliver it. No handler ran for it, so it
// gets no end of interrupt - that would end a real one.
extern "x86-interrupt" fn spurious(_frame: InterruptStackFrame) {}

extern "x86-interrupt" fn divide_error(frame: InterruptStackFrame) {
    end_user_program_on_fault(&frame, "divide error");
    panic!("EXCEPTION: DIVIDE ERROR\n{:?}", frame);
}

extern "x86-interrupt" fn invalid_opcode(frame: InterruptStackFrame) {
    end_user_program_on_fault(&frame, "invalid opcode");
    panic!("EXCEPTION: INVALID OPCODE\n{:?}", frame);
}

/// A fault in ring 3 is the program's fault, not the kernel's: the program ends, the kernel goes on (chapter 20).
/// The low 2 bits of the CS the CPU saved say which ring was interrupted.
fn end_user_program_on_fault(frame: &InterruptStackFrame, what: &'static str) {
    if frame.cs & 3 == 3 {
        crate::println!("Ring 3: {} at {:#x} - the program is ended", what, frame.rip);
        user::end(user::Exit::Killed(what));
    }
}

extern "x86-interrupt" fn general_protection(frame: InterruptStackFrame, error_code: u64) {
    end_user_program_on_fault(&frame, "general protection fault");
    // the error code names a segment selector or - bit 1 set - an IDT entry: (code >> 3) is its number
    panic!(
        "EXCEPTION: GENERAL PROTECTION FAULT (code {:#x})\n{:?}",
        error_code, frame
    );
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, error_code: u64) {
    end_user_program_on_fault(&frame, "page fault");
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

/// Switches to the stack whose top is `top` and calls a function that calls itself forever: a real stack overflow.
/// On a stack with a guard page it ends where chapter 5's fake one did - in the double fault handler.
pub fn provoke_stack_overflow(top: u64) -> ! {
    unsafe { asm!("mov rsp, {}", "call {}", in(reg) top, sym overflow, options(noreturn)) }
}

extern "C" fn overflow() -> ! {
    #[allow(unconditional_recursion)]
    fn deeper(depth: u64) -> u64 {
        let room = core::hint::black_box([depth; 64]); // 512 bytes of stack per call
        deeper(depth + 1) + room[0]
    }
    deeper(0);
    unreachable!()
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
        assert_eq!(gate.address(), double_fault as *const () as u64);
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
                TIMER_VECTOR,
                KEYBOARD_VECTOR,
                lapic::SPURIOUS_VECTOR,
                WAKE_VECTOR,
                crate::ahci::VECTOR,
                user::SYSCALL_VECTOR,
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
