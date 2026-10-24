//! 8253/8254 PIT (Programmable Interval Timer), channel 0 -> IRQ 0.
//! The chip counts down at 1.193182 MHz and raises an interrupt when it hits 0.

use super::port::outb;

pub const BASE_FREQUENCY: u32 = 1_193_182;

const CHANNEL0_DATA: u16 = 0x40;
const COMMAND: u16 = 0x43;

/// Program channel 0 to fire `hz` interrupts per second.
pub fn init(hz: u32) {
    let divisor = (BASE_FREQUENCY / hz).clamp(1, 0xFFFF) as u16;
    unsafe {
        // channel 0, access low+high byte, mode 3 (square wave), binary
        outb(COMMAND, 0b0011_0110);
        outb(CHANNEL0_DATA, divisor as u8);
        outb(CHANNEL0_DATA, (divisor >> 8) as u8);
    }
}
