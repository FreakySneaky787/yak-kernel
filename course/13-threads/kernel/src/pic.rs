//! The two 8259 PICs (programmable interrupt controllers) - the classic way hardware interrupts reach the CPU.
//!
//! Each chip has 8 input lines, IRQ 0-7; the second ("slave") is wired to IRQ 2 of the first ("master"), so there
//! are 16 IRQs. When a device raises its line, the PIC hands the CPU a vector number: the chip's *offset* plus the
//! line. After a reset the master's offset is 8 - IRQ 0, the timer, would arrive as vector 8, the double fault.
//! So the first thing we do is move both chips to 32-47, just past the CPU's exceptions.
//!
//! Since chapter 12 the APIC delivers all hardware interrupts. The PIC is still there, so `init` moves it off the
//! exception vectors and masks every line - and that is all we do with it.

#[cfg(test)]
use crate::port::inb;
use crate::port::outb;

pub const MASTER_OFFSET: u8 = 32;
pub const SLAVE_OFFSET: u8 = MASTER_OFFSET + 8;

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xA0;
const SLAVE_DATA: u16 = 0xA1;

/// The vector an IRQ would arrive on.
#[cfg(test)]
pub const fn vector(irq: u8) -> u8 {
    MASTER_OFFSET + irq
}

/// The chips are old and slow: a write to the unused port 0x80 gives them about a microsecond between commands.
fn io_wait() {
    unsafe { outb(0x80, 0) };
}

/// Moves both PICs to vectors 32-47 and masks every IRQ.
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

/// The masks of (master, slave): a set bit means blocked - for the tests.
#[cfg(test)]
pub fn masks() -> (u8, u8) {
    unsafe { (inb(MASTER_DATA), inb(SLAVE_DATA)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn every_line_is_masked() {
        assert_eq!(masks(), (0xFF, 0xFF));
    }

    #[test_case]
    fn irqs_start_after_the_exceptions() {
        assert_eq!(vector(0), 32);
        assert_eq!(vector(15), 47);
    }
}
