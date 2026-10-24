//! Hardware interrupts (IRQs) via the PIC: handlers and initialization.

use super::idt::{Idt, InterruptStackFrame};
use super::pic::{self, PIC1_OFFSET};
use crate::time;

pub const TIMER_IRQ: u8 = 0;
pub const KEYBOARD_IRQ: u8 = 1;
pub const COM1_IRQ: u8 = 4;

pub const fn vector(irq: u8) -> u8 {
    PIC1_OFFSET + irq
}

pub fn install_irq_handlers(idt: &mut Idt) {
    idt.set(vector(TIMER_IRQ), timer);
    idt.set(vector(KEYBOARD_IRQ), keyboard);
    idt.set(vector(COM1_IRQ), com1);
    idt.set(vector(7), spurious_master);
    idt.set(vector(15), spurious_slave);
}

/// Set up PIC + PIT and unmask the IRQs we need.
/// Interrupts are **not** enabled here yet (`kernel::init` does that).
pub fn init() {
    super::lapic::disable();
    pic::init();
    super::pit::init(time::TIMER_HZ);
    pic::unmask(TIMER_IRQ);
    crate::drivers::keyboard::init();
    pic::unmask(KEYBOARD_IRQ);
    pic::unmask(COM1_IRQ);
}

extern "x86-interrupt" fn timer(_frame: InterruptStackFrame) {
    time::tick();
    pic::end_of_interrupt(TIMER_IRQ);
}

extern "x86-interrupt" fn keyboard(_frame: InterruptStackFrame) {
    crate::drivers::keyboard::handle_interrupt();
    pic::end_of_interrupt(KEYBOARD_IRQ);
}

extern "x86-interrupt" fn com1(_frame: InterruptStackFrame) {
    crate::serial::handle_interrupt();
    pic::end_of_interrupt(COM1_IRQ);
}

// Spurious IRQs: the PIC reports IRQ 7/15 although nothing is pending. Then no
// EOI may go to that PIC (for the slave the master still needs one, due to the cascade).
extern "x86-interrupt" fn spurious_master(_frame: InterruptStackFrame) {
    if pic::in_service() & (1 << 7) != 0 {
        pic::end_of_interrupt(7);
    }
}

extern "x86-interrupt" fn spurious_slave(_frame: InterruptStackFrame) {
    if pic::in_service() & (1 << 15) != 0 {
        pic::end_of_interrupt(15);
    } else {
        pic::end_of_interrupt(0); // master EOI only
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn local_apic_disabled_for_pic_mode() {
        assert!(!super::super::lapic::is_enabled());
    }

    #[test_case]
    fn timer_irq_unmasked() {
        let (master, _slave) = pic::masks();
        assert_eq!(master & 1, 0, "IRQ0 is masked");
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
