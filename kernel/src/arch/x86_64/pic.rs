//! Two cascaded 8259 PICs (programmable interrupt controllers).
//!
//! They collect the 16 hardware IRQs (timer, keyboard, ...) and deliver them to the
//! CPU as interrupt vectors. By default IRQ 0-7 map to vectors
//! 8-15 - exactly where CPU exceptions live (8 = double fault!).
//! That is why we remap them to 32-47.

use super::port::{inb, outb};

pub const PIC1_OFFSET: u8 = 32;
pub const PIC2_OFFSET: u8 = PIC1_OFFSET + 8;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;
const ICW4_8086: u8 = 0x01;
const CMD_EOI: u8 = 0x20;
const CMD_READ_ISR: u8 = 0x0B;

/// Short delay: a write to the unused port 0x80 takes about 1 us.
fn io_wait() {
    unsafe { outb(0x80, 0) };
}

/// Initialize (remap) both PICs and mask all IRQs.
pub fn init() {
    unsafe {
        // ICW1: start initialization, ICW4 follows
        outb(PIC1_CMD, ICW1_INIT | ICW1_ICW4);
        io_wait();
        outb(PIC2_CMD, ICW1_INIT | ICW1_ICW4);
        io_wait();
        // ICW2: vector offsets
        outb(PIC1_DATA, PIC1_OFFSET);
        io_wait();
        outb(PIC2_DATA, PIC2_OFFSET);
        io_wait();
        // ICW3: slave is wired to IRQ2 of the master
        outb(PIC1_DATA, 1 << 2);
        io_wait();
        outb(PIC2_DATA, 2);
        io_wait();
        // ICW4: 8086 mode
        outb(PIC1_DATA, ICW4_8086);
        io_wait();
        outb(PIC2_DATA, ICW4_8086);
        io_wait();
        // Mask everything; individual IRQs are enabled with `unmask`
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}

fn data_port(irq: u8) -> (u16, u8) {
    if irq < 8 { (PIC1_DATA, irq) } else { (PIC2_DATA, irq - 8) }
}

pub fn unmask(irq: u8) {
    let (port, bit) = data_port(irq);
    unsafe { outb(port, inb(port) & !(1 << bit)) };
    if irq >= 8 {
        unmask(2); // the cascade line to the slave must be open
    }
}

pub fn mask(irq: u8) {
    let (port, bit) = data_port(irq);
    unsafe { outb(port, inb(port) | (1 << bit)) };
}

/// Current masks (master, slave) - a set bit means the IRQ is masked.
pub fn masks() -> (u8, u8) {
    unsafe { (inb(PIC1_DATA), inb(PIC2_DATA)) }
}

/// "End of interrupt": tell the PIC the IRQ has been handled.
/// Without an EOI it sends no further interrupts of this or lower priority.
pub fn end_of_interrupt(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(PIC2_CMD, CMD_EOI);
        }
        outb(PIC1_CMD, CMD_EOI);
    }
}

/// In-service register: which IRQs are currently being handled. Used to detect
/// spurious IRQ 7/15 (glitches without a real interrupt).
pub fn in_service() -> u16 {
    unsafe {
        outb(PIC1_CMD, CMD_READ_ISR);
        outb(PIC2_CMD, CMD_READ_ISR);
        (inb(PIC2_CMD) as u16) << 8 | inb(PIC1_CMD) as u16
    }
}
