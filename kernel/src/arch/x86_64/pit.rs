//! 8253/8254 PIT (Programmable Interval Timer), Kanal 0 → IRQ 0.
//! Der Chip zählt mit 1.193182 MHz herunter; bei 0 gibt es einen Interrupt.

use super::port::outb;

pub const BASE_FREQUENCY: u32 = 1_193_182;

const CHANNEL0_DATA: u16 = 0x40;
const COMMAND: u16 = 0x43;

/// Kanal 0 auf `hz` Interrupts pro Sekunde stellen.
pub fn init(hz: u32) {
    let divisor = (BASE_FREQUENCY / hz).clamp(1, 0xFFFF) as u16;
    unsafe {
        // Kanal 0, Zugriff Low+High-Byte, Modus 3 (Rechteck), binär
        outb(COMMAND, 0b0011_0110);
        outb(CHANNEL0_DATA, divisor as u8);
        outb(CHANNEL0_DATA, (divisor >> 8) as u8);
    }
}
