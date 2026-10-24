//! PS/2 keyboard: IRQ1 reads the scancode byte from port 0x60 and stores it in
//! a ring buffer. The rest of the kernel pulls key presses from there.
//!
//! The interrupt handler deliberately does as little as possible (just store the
//! byte); translation happens outside of it in the `Decoder`. The one
//! exception is Ctrl+C, which must be noticed even if nobody reads the
//! keyboard: the handler tracks the Ctrl keys itself (see `handle_interrupt`).

use super::scancode::{Decoder, KeyEvent, Layout};
use crate::arch::x86_64::port::inb;
use crate::arch::x86_64::without_interrupts;
use crate::ringbuf::RingBuffer;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 0x01;

static SCANCODES: Mutex<RingBuffer<128>> = Mutex::new(RingBuffer::new());
static DECODER: Mutex<Decoder> = Mutex::new(Decoder::new(Layout::De));

/// Ctrl state as seen by the interrupt handler (independent of the decoder).
static IRQ_CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static IRQ_LAST_WAS_E0: AtomicBool = AtomicBool::new(false);

const SC_CTRL: u8 = 0x1D;
const SC_CTRL_RELEASE: u8 = 0x9D;
/// Set-1 make code of the key labelled C (same position on QWERTY and QWERTZ).
const SC_C: u8 = 0x2E;

/// Drain the PS/2 controller output buffer (leftovers from firmware/bootloader).
pub fn init() {
    for _ in 0..32 {
        if unsafe { inb(STATUS_PORT) } & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        unsafe { inb(DATA_PORT) };
    }
}

/// Called from the IRQ1 handler (interrupts are disabled there). On Ctrl+C
/// `on_ctrl_c` is asked whether it consumed the key; if so, the C press is
/// not stored.
pub fn handle_interrupt(on_ctrl_c: impl FnOnce() -> bool) {
    let byte = unsafe { inb(DATA_PORT) };
    if track_ctrl_c(byte) && on_ctrl_c() {
        return;
    }
    SCANCODES.lock().push(byte);
}

/// Follows the Ctrl keys in the raw scancode stream; true if `byte` is the
/// C press of a Ctrl+C.
fn track_ctrl_c(byte: u8) -> bool {
    let after_e0 = IRQ_LAST_WAS_E0.swap(byte == 0xE0, Ordering::Relaxed);
    match byte {
        SC_CTRL => IRQ_CTRL_DOWN.store(true, Ordering::Relaxed), // left Ctrl, or right Ctrl after E0
        SC_CTRL_RELEASE => IRQ_CTRL_DOWN.store(false, Ordering::Relaxed),
        SC_C => return !after_e0 && IRQ_CTRL_DOWN.load(Ordering::Relaxed),
        _ => {}
    }
    false
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
        crate::task::wait_for(crate::task::Wait::Input, has_pending);
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
    fn ctrl_c_is_recognised_in_the_raw_stream() {
        let feed = |bytes: &[u8]| bytes.iter().map(|&b| track_ctrl_c(b)).collect::<alloc::vec::Vec<_>>();
        assert_eq!(feed(&[0x2E, 0xAE]), [false, false]); // plain c
        assert_eq!(feed(&[0x1D, 0x2E, 0xAE, 0x9D]), [false, true, false, false]); // left Ctrl+C
        assert_eq!(
            feed(&[0xE0, 0x1D, 0x2E, 0xE0, 0x9D]),
            [false, false, true, false, false]
        ); // right Ctrl
        assert_eq!(feed(&[0x1D, 0xE0, 0x2E, 0x9D]), [false, false, false, false]); // E0 2E = volume down
        assert_eq!(feed(&[0x2E]), [false]); // Ctrl released again
    }

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
