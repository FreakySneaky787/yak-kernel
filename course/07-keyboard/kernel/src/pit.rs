//! The PIT (programmable interval timer, Intel 8253/8254): a counter that runs at 1.193182 MHz - a frequency from
//! the original IBM PC - and counts down from a number we choose. Channel 0 raises IRQ 0 every time it reaches zero,
//! then starts again: a steady beat, the kernel's clock.

use crate::port::outb;

/// How fast the PIT counts, in Hz.
pub const BASE_FREQUENCY: u32 = 1_193_182;

const CHANNEL0: u16 = 0x40;
const COMMAND: u16 = 0x43;

/// The number to count down from for `hz` interrupts per second. The counter has 16 bits, so the slowest the PIT
/// can go is about 18.2 Hz.
pub fn divisor(hz: u32) -> u16 {
    (BASE_FREQUENCY / hz).clamp(1, 0xFFFF) as u16
}

/// Lets channel 0 fire `hz` times per second.
pub fn init(hz: u32) {
    let divisor = divisor(hz);
    unsafe {
        // channel 0, low byte then high byte, mode 3 (square wave: reload and repeat), binary counting
        outb(COMMAND, 0b0011_0110);
        outb(CHANNEL0, divisor as u8);
        outb(CHANNEL0, (divisor >> 8) as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn the_divisor_for_100_hz() {
        assert_eq!(divisor(100), 11931); // 1193182 / 100, rounded down: 100.0015 Hz
        assert_eq!(divisor(1), 0xFFFF); // too slow for 16 bits: the slowest it can do
    }
}
