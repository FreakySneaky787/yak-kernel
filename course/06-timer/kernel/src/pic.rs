//! The two 8259 PICs (programmable interrupt controllers) - the classic way hardware interrupts reach the CPU.
//!
//! Each chip has 8 input lines, IRQ 0-7; the second ("slave") is wired to IRQ 2 of the first ("master"), so there
//! are 16 IRQs. When a device raises its line, the PIC hands the CPU a vector number: the chip's *offset* plus the
//! line. After a reset the master's offset is 8 - IRQ 0, the timer, would arrive as vector 8, the double fault.
//! So the first thing we do is move both chips to 32-47, just past the CPU's exceptions.

use crate::port::{inb, outb};

pub const MASTER_OFFSET: u8 = 32;
pub const SLAVE_OFFSET: u8 = MASTER_OFFSET + 8;

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xA0;
const SLAVE_DATA: u16 = 0xA1;

const END_OF_INTERRUPT: u8 = 0x20;

/// The vector an IRQ arrives on.
pub const fn vector(irq: u8) -> u8 {
    MASTER_OFFSET + irq
}

/// The chips are old and slow: a write to the unused port 0x80 gives them about a microsecond between commands.
fn io_wait() {
    unsafe { outb(0x80, 0) };
}

/// Moves both PICs to vectors 32-47 and masks every IRQ; `unmask` lets single ones through.
pub fn init() {
    // the four "initialization command words" (ICW), always in this order
    let commands = [
        (MASTER_COMMAND, 0x11),       // ICW1 master: start initialization, ICW4 follows
        (SLAVE_COMMAND, 0x11),        // ICW1 slave
        (MASTER_DATA, MASTER_OFFSET), // ICW2: the vector of IRQ 0
        (SLAVE_DATA, SLAVE_OFFSET),   //       the vector of IRQ 8
        (MASTER_DATA, 1 << 2),        // ICW3 master: a slave hangs on line 2
        (SLAVE_DATA, 2),              // ICW3 slave: you hang on line 2
        (MASTER_DATA, 0x01),          // ICW4: 8086 mode
        (SLAVE_DATA, 0x01),
    ];
    for (port, value) in commands {
        unsafe { outb(port, value) };
        io_wait();
    }
    // mask register: a set bit blocks that IRQ
    unsafe {
        outb(MASTER_DATA, 0xFF);
        outb(SLAVE_DATA, 0xFF);
    }
}

/// Lets `irq` through. An IRQ of the slave also needs the master's line 2 open.
pub fn unmask(irq: u8) {
    let (port, line) = if irq < 8 {
        (MASTER_DATA, irq)
    } else {
        (SLAVE_DATA, irq - 8)
    };
    unsafe { outb(port, inb(port) & !(1 << line)) };
    if irq >= 8 {
        unmask(2);
    }
}

/// The masks of (master, slave): a set bit means blocked - for the tests.
#[cfg(test)]
pub fn masks() -> (u8, u8) {
    unsafe { (inb(MASTER_DATA), inb(SLAVE_DATA)) }
}

/// Tells the PIC that the handler of `irq` is done. Until then it delivers no further IRQ of the same or a lower
/// priority. An IRQ of the slave went through both chips, so both get the message.
pub fn end_of_interrupt(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(SLAVE_COMMAND, END_OF_INTERRUPT);
        }
        outb(MASTER_COMMAND, END_OF_INTERRUPT);
    }
}

/// The in-service register of (master, slave): which IRQ handlers the PIC thinks are running right now.
pub fn in_service() -> (u8, u8) {
    const READ_ISR: u8 = 0x0B;
    unsafe {
        outb(MASTER_COMMAND, READ_ISR);
        outb(SLAVE_COMMAND, READ_ISR);
        (inb(MASTER_COMMAND), inb(SLAVE_COMMAND))
    }
}

/// Turns the CPU's local APIC off (bit 11 of the model-specific register IA32_APIC_BASE).
///
/// Every modern x86 core has a *local APIC* between the PIC and the CPU; the PIC's signal enters it on its LINT0
/// pin. Limine leaves LINT0 masked - with the APIC on, no PIC interrupt would ever arrive. With the APIC off, the
/// PIC's signal goes straight to the CPU's interrupt pin, as on a PC from 1990. (The reference kernel later switches
/// to the APIC and masks the PIC instead.)
pub fn disable_local_apic() {
    let base = read_msr(IA32_APIC_BASE);
    unsafe { write_msr(IA32_APIC_BASE, base & !APIC_GLOBAL_ENABLE) };
}

/// Whether the local APIC is on - for the tests.
#[cfg(test)]
pub fn local_apic_enabled() -> bool {
    read_msr(IA32_APIC_BASE) & APIC_GLOBAL_ENABLE != 0
}

const IA32_APIC_BASE: u32 = 0x1B;
const APIC_GLOBAL_ENABLE: u64 = 1 << 11;

/// Model-specific registers: CPU settings with a 32-bit number, read and written 64 bits at a time (EDX:EAX).
fn read_msr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    unsafe { core::arch::asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack)) };
    (high as u64) << 32 | low as u64
}

unsafe fn write_msr(msr: u32, value: u64) {
    let (low, high) = (value as u32, (value >> 32) as u32);
    unsafe { core::arch::asm!("wrmsr", in("ecx") msr, in("eax") low, in("edx") high, options(nostack)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn the_local_apic_is_off() {
        assert!(!local_apic_enabled());
    }

    #[test_case]
    fn only_the_timer_is_let_through() {
        assert_eq!(masks(), (0b1111_1110, 0b1111_1111));
    }

    #[test_case]
    fn irqs_start_after_the_exceptions() {
        assert_eq!(vector(0), 32);
        assert_eq!(vector(15), 47);
    }
}
