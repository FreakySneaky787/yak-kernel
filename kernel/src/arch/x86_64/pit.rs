//! 8253/8254 PIT (Programmable Interval Timer), channel 0 -> IRQ 0.
//! The chip counts down at 1.193182 MHz and raises an interrupt when it hits 0.

use super::port::{inb, outb};

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

const CHANNEL2_DATA: u16 = 0x42;
/// Port 0x61: bit 0 = channel 2 gate, bit 1 = speaker enable, bit 5 = channel 2 output
const CONTROL_PORT_B: u16 = 0x61;

/// Starts a one-shot countdown of `ms` milliseconds (max. ~54 ms) on channel 2.
/// Channel 2 is wired to the PC speaker, not to an IRQ, so this works with
/// interrupts disabled; its output can be polled in port 0x61.
pub fn channel2_start_oneshot(ms: u32) {
    let count = (BASE_FREQUENCY * ms / 1000).clamp(1, 0xFFFF) as u16;
    unsafe {
        let port_b = inb(CONTROL_PORT_B) & !0b10; // speaker off
        outb(CONTROL_PORT_B, port_b & !0b1); // gate low while programming
        // channel 2, low+high byte, mode 0 (interrupt on terminal count), binary
        outb(COMMAND, 0b1011_0000);
        outb(CHANNEL2_DATA, count as u8);
        outb(CHANNEL2_DATA, (count >> 8) as u8);
        outb(CONTROL_PORT_B, port_b | 0b1); // gate high: counting starts
    }
}

/// Busy-waits until the channel 2 countdown reaches zero.
pub fn channel2_wait() {
    while unsafe { inb(CONTROL_PORT_B) } & 0x20 == 0 {
        core::hint::spin_loop();
    }
}
