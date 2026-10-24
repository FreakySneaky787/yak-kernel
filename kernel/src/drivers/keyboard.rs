//! PS/2 keyboard: IRQ1 reads the scancode byte from port 0x60 and stores it in
//! a ring buffer. The rest of the kernel pulls key presses from there.
//!
//! The interrupt handler deliberately does as little as possible (just store the
//! byte); translation happens outside of it in the `Decoder`.

use super::scancode::{Decoder, KeyEvent, Layout};
use crate::ringbuf::RingBuffer;
use crate::arch::x86_64::port::inb;
use crate::arch::x86_64::without_interrupts;
use spin::Mutex;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 0x01;

static SCANCODES: Mutex<RingBuffer<128>> = Mutex::new(RingBuffer::new());
static DECODER: Mutex<Decoder> = Mutex::new(Decoder::new(Layout::De));

/// Drain the PS/2 controller output buffer (leftovers from firmware/bootloader).
pub fn init() {
    for _ in 0..32 {
        if unsafe { inb(STATUS_PORT) } & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        unsafe { inb(DATA_PORT) };
    }
}

/// Called from the IRQ1 handler (interrupts are disabled there).
pub fn handle_interrupt() {
    let byte = unsafe { inb(DATA_PORT) };
    SCANCODES.lock().push(byte);
}

/// Inject a byte from outside (for tests and future input sources).
pub fn inject_scancode(byte: u8) {
    without_interrupts(|| SCANCODES.lock().push(byte));
}

/// Fetch the next key press without waiting.
pub fn try_read_key() -> Option<KeyEvent> {
    loop {
        let byte = without_interrupts(|| SCANCODES.lock().pop())?;
        if let Some(event) = DECODER.lock().feed(byte) {
            return Some(event);
        }
    }
}

/// Wait for the next key press (sleeps with `hlt` between interrupts).
pub fn read_key() -> KeyEvent {
    loop {
        if let Some(event) = try_read_key() {
            return event;
        }
        // Check and sleep atomically: otherwise the IRQ could arrive between the
        // check and `hlt`, and we would hang until the next interrupt.
        crate::arch::x86_64::disable_interrupts();
        if SCANCODES.lock().is_empty() {
            unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
        } else {
            crate::arch::x86_64::enable_interrupts();
        }
    }
}

/// Whether unread scancodes are waiting (call with interrupts disabled for a race-free check).
pub fn has_pending() -> bool {
    !SCANCODES.lock().is_empty()
}

pub fn layout() -> Layout {
    DECODER.lock().layout()
}

pub fn set_layout(layout: Layout) {
    DECODER.lock().set_layout(layout);
}

#[cfg(test)]
mod tests {
    use super::super::scancode::Key;
    use super::*;

    #[test_case]
    fn injected_scancodes_become_keys() {
        while try_read_key().is_some() {}
        for b in [0x23, 0xA3, 0x2A, 0x17, 0x97, 0xAA, 0x1C] {
            inject_scancode(b); // h, Shift+i, Enter
        }
        assert_eq!(try_read_key().unwrap().key, Key::Char('h'));
        assert_eq!(try_read_key().unwrap().key, Key::Char('I'));
        assert_eq!(read_key().key, Key::Enter);
        assert!(try_read_key().is_none());
    }
}
