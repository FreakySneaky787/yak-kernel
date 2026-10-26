//! Hardware interrupts via the APIC: the I/O APIC routes device lines to
//! vectors, the local APIC delivers them and its own timer drives `time`.
//!
//! The legacy 8259 PICs are remapped (so a stray interrupt can never land on
//! an exception vector) and then fully masked.

use super::idt::{Idt, InterruptStackFrame};
use super::{ioapic, lapic, pic};
use crate::acpi;
use crate::time;

pub const KEYBOARD_IRQ: u8 = 1;
pub const COM1_IRQ: u8 = 4;

pub const TIMER_VECTOR: u8 = 32;
/// ISA IRQ `n` is delivered on vector `ISA_VECTOR_BASE + n`.
pub const ISA_VECTOR_BASE: u8 = 32;

pub const fn isa_vector(irq: u8) -> u8 {
    ISA_VECTOR_BASE + irq
}

pub fn install_irq_handlers(idt: &mut Idt) {
    idt.set(TIMER_VECTOR, timer);
    idt.set(isa_vector(KEYBOARD_IRQ), keyboard);
    idt.set(isa_vector(COM1_IRQ), com1);
    // A masked 8259 can still raise "spurious" IRQ 7/15 on its remapped vectors
    idt.set(pic::PIC1_OFFSET + 7, pic_spurious);
    idt.set(pic::PIC2_OFFSET + 7, pic_spurious);
    idt.set(lapic::SPURIOUS_VECTOR, apic_spurious);
    idt.set(super::smp::WAKE_VECTOR, wake);
    idt.set(super::smp::TLB_VECTOR, tlb_shootdown);
    idt.set(crate::drivers::ahci::VECTOR, ahci);
    idt.set(crate::net::VECTOR, network);
}

/// Routes a legacy ISA IRQ through the I/O APIC to the current CPU,
/// honouring the MADT's interrupt source overrides.
fn route_isa_irq(madt: &acpi::Madt, irq: u8, dest: u8) {
    let (gsi, over) = madt.isa_irq(irq);
    // ISA defaults: edge triggered, active high
    let active_low = over.is_some_and(|o| o.active_low());
    let level = over.is_some_and(|o| o.level_triggered());
    ioapic::route(gsi, isa_vector(irq), dest, active_low, level);
}

/// Sets up local APIC, I/O APIC and the timer, and routes keyboard and COM1.
/// Interrupts are **not** enabled here yet (`kernel::init` does that).
pub fn init() {
    let info = acpi::init().expect("ACPI tables are required for the APIC");
    let madt = &info.madt;

    pic::init(); // remap + mask everything
    lapic::init(madt.local_apic_address);
    let io = madt.io_apics.first().expect("no I/O APIC in the MADT");
    ioapic::init(io.address, io.gsi_base);

    let cpu = lapic::id();
    crate::drivers::keyboard::init();
    route_isa_irq(madt, KEYBOARD_IRQ, cpu);
    route_isa_irq(madt, COM1_IRQ, cpu);

    lapic::start_timer(TIMER_VECTOR, time::TIMER_HZ);
    super::tsc::init(); // uses PIT channel 2 too, so after the LAPIC calibration
}

/// Signals for a process interrupted in user mode. The handler's `frame`
/// argument *is* the frame the CPU pushed, so changing it changes where
/// `iretq` returns to (volatile, so the writes are not optimised away).
fn deliver_signals(frame: &mut InterruptStackFrame) {
    let (mut rip, mut rsp) = (frame.rip, frame.rsp);
    crate::process::deliver_signals(&mut rip, &mut rsp, frame.rflags);
    unsafe {
        core::ptr::write_volatile(&mut frame.rip, rip);
        core::ptr::write_volatile(&mut frame.rsp, rsp);
    }
}

extern "x86-interrupt" fn timer(mut frame: InterruptStackFrame) {
    // every CPU has a timer; the clock is driven by the boot CPU's only
    if super::smp::cpu_index() == 0 {
        time::tick();
    }
    // EOI before scheduling: we may switch to another thread here and only
    // return to this handler much later; the LAPIC must accept new ticks.
    lapic::eoi();
    crate::task::on_tick();
    // About to return to user code: act on pending signals
    if frame.cs & 3 == 3 {
        deliver_signals(&mut frame);
    }
}

extern "x86-interrupt" fn keyboard(_frame: InterruptStackFrame) {
    crate::drivers::keyboard::handle_interrupt(crate::process::interrupt_foreground);
    crate::task::wake_input_waiters();
    lapic::eoi();
    crate::task::preempt_idle();
}

extern "x86-interrupt" fn com1(_frame: InterruptStackFrame) {
    // 0x03 = Ctrl+C on a terminal
    crate::serial::handle_interrupt(|byte| byte == 0x03 && crate::process::interrupt_foreground());
    crate::task::wake_input_waiters();
    lapic::eoi();
    crate::task::preempt_idle();
}

/// AHCI command completion (MSI).
extern "x86-interrupt" fn ahci(_frame: InterruptStackFrame) {
    crate::drivers::ahci::handle_interrupt();
    lapic::eoi();
    crate::task::preempt_idle();
}

/// e1000 (level-triggered INTx through the I/O APIC, see `net::probe_irq`).
extern "x86-interrupt" fn network(_frame: InterruptStackFrame) {
    crate::net::handle_interrupt();
    lapic::eoi();
    crate::task::preempt_idle();
}

/// Wakes an idle CPU (it then looks for work); nothing else to do.
extern "x86-interrupt" fn wake(mut frame: InterruptStackFrame) {
    lapic::eoi();
    crate::task::on_wake_ipi();
    if frame.cs & 3 == 3 {
        deliver_signals(&mut frame);
    }
}

extern "x86-interrupt" fn tlb_shootdown(_frame: InterruptStackFrame) {
    super::smp::on_tlb_ipi();
    lapic::eoi();
}

/// Spurious interrupts must not be acknowledged (no EOI).
extern "x86-interrupt" fn pic_spurious(_frame: InterruptStackFrame) {}

extern "x86-interrupt" fn apic_spurious(_frame: InterruptStackFrame) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn legacy_pic_fully_masked() {
        assert_eq!(pic::masks(), (0xFF, 0xFF));
    }

    #[test_case]
    fn keyboard_and_com1_routed() {
        let madt = &acpi::info().unwrap().madt;
        for irq in [KEYBOARD_IRQ, COM1_IRQ] {
            let gsi = madt.isa_irq(irq).0;
            assert!(!ioapic::is_masked(gsi), "IRQ {} masked", irq);
            assert_eq!(ioapic::vector_of(gsi), isa_vector(irq));
        }
        // the PIT is not used any more: its input stays masked
        assert!(ioapic::is_masked(madt.isa_irq(0).0));
    }

    #[test_case]
    fn timer_ticks_advance() {
        assert!(super::super::interrupts_enabled());
        let start = time::ticks();
        // `hlt` waits for the next interrupt -> only a few iterations needed
        for _ in 0..1000 {
            if time::ticks() >= start + 5 {
                break;
            }
            unsafe { core::arch::asm!("hlt") };
        }
        assert!(time::ticks() >= start + 5, "Ticks: {} -> {}", start, time::ticks());
    }

    #[test_case]
    fn sleep_waits_roughly() {
        let start = time::ticks();
        time::sleep_ms(50);
        let elapsed = time::ticks() - start;
        assert!((5..=10).contains(&elapsed), "sleep_ms(50) took {} ticks", elapsed);
    }
}
