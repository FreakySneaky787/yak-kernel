//! Two cascaded 8259 PICs (programmable interrupt controllers).
//!
//! They collect the 16 hardware IRQs (timer, keyboard, ...) and deliver them to the
//! CPU as interrupt vectors. By default IRQ 0-7 map to vectors
//! 8-15 - exactly where CPU exceptions live (8 = double fault!).
//! That is why we remap them to 32-47.
//!
//! Since M12 the I/O APIC delivers all interrupts; the PICs are only remapped
//! (so a stray interrupt never hits an exception vector) and then masked.

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

/// Current masks (master, slave) - a set bit means the IRQ is masked.
pub fn masks() -> (u8, u8) {
    unsafe { (inb(PIC1_DATA), inb(PIC2_DATA)) }
}

