//! The PS/2 keyboard. On every press and release the keyboard controller puts one byte at port 0x60 and raises
//! IRQ 1. The handler only moves the byte into a ring buffer - an interrupt handler should be quick - and
//! `read_key` takes bytes out and decodes them, outside the handler.

use crate::port::inb;
use crate::ringbuf::RingBuffer;
use crate::scancode::{Decoder, Key};
use crate::task::{self, Wait};
use spin::Mutex;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const OUTPUT_FULL: u8 = 1 << 0; // status bit 0: a byte waits at the data port

/// The bytes the handler received and nobody has read yet. The handler and the kernel both lock it, so the kernel
/// takes it only with interrupts off: a handler that interrupts the lock's holder would wait for it forever.
static SCANCODES: Mutex<RingBuffer<128>> = Mutex::new(RingBuffer::new());
/// Only `read_key` uses the decoder - the handler never touches it.
static DECODER: Mutex<Decoder> = Mutex::new(Decoder::new());

/// Empties the controller: a byte that waits from before (a key pressed during boot) would be read by nobody, and
/// while one waits the controller sends no new one.
pub fn init() {
    for _ in 0..16 {
        if unsafe { inb(STATUS_PORT) } & OUTPUT_FULL == 0 {
            break;
        }
        unsafe { inb(DATA_PORT) };
    }
}

/// Called by the IRQ 1 handler: the byte into the buffer. If the buffer is full the byte is lost - better than
/// waiting inside an interrupt handler for somebody to read.
pub fn handle_interrupt() {
    let byte = unsafe { inb(DATA_PORT) };
    SCANCODES.lock().push(byte);
    task::wake(Wait::Input); // the thread that waits in `read_key`, if there is one
}

/// Waits for the next key that types something. Since chapter 13 the thread blocks while there is none, and the
/// keyboard handler wakes it.
pub fn read_key() -> Key {
    loop {
        let byte = task::wait_for(Wait::Input, || SCANCODES.lock().pop());
        if let Some(key) = DECODER.lock().feed(byte) {
            return key;
        }
    }
}

/// Puts a byte into the buffer as if the keyboard had sent it - for the tests.
#[cfg(test)]
fn inject(byte: u8) {
    crate::interrupts::without_interrupts(|| SCANCODES.lock().push(byte));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn bytes_in_the_buffer_become_keys() {
        // Shift+h, i, Enter - each press followed by its release
        for byte in [0x2A, 0x23, 0xA3, 0xAA, 0x17, 0x97, 0x1C, 0x9C] {
            inject(byte);
        }
        assert_eq!(read_key(), Key::Char('H'));
        assert_eq!(read_key(), Key::Char('i'));
        assert_eq!(read_key(), Key::Enter);
    }
}
