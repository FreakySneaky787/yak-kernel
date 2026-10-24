//! PS/2-Tastatur: IRQ1 liest das Scancode-Byte aus Port 0x60 und legt es in
//! einen Ringpuffer. Der Rest des Kernels holt sich daraus Tastendrücke.
//!
//! Der Interrupt-Handler macht bewusst so wenig wie möglich (nur Byte
//! speichern); das Übersetzen passiert außerhalb im `Decoder`.

use super::scancode::{Decoder, KeyEvent, Layout};
use crate::arch::x86_64::port::inb;
use crate::arch::x86_64::without_interrupts;
use spin::Mutex;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 0x01;

/// Fester Ringpuffer ohne Heap. Ist er voll, werden neue Bytes verworfen.
pub struct RingBuffer<const N: usize> {
    buf: [u8; N],
    head: usize,
    len: usize,
}

impl<const N: usize> RingBuffer<N> {
    pub const fn new() -> Self {
        RingBuffer { buf: [0; N], head: 0, len: 0 }
    }

    /// `false`, wenn der Puffer voll war.
    pub fn push(&mut self, byte: u8) -> bool {
        if self.len == N {
            return false;
        }
        self.buf[(self.head + self.len) % N] = byte;
        self.len += 1;
        true
    }

    pub fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.buf[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(byte)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Default for RingBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

static SCANCODES: Mutex<RingBuffer<128>> = Mutex::new(RingBuffer::new());
static DECODER: Mutex<Decoder> = Mutex::new(Decoder::new(Layout::De));

/// Ausgabepuffer des PS/2-Controllers leeren (Reste von Firmware/Bootloader).
pub fn init() {
    for _ in 0..32 {
        if unsafe { inb(STATUS_PORT) } & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        unsafe { inb(DATA_PORT) };
    }
}

/// Wird aus dem IRQ1-Handler aufgerufen (Interrupts sind dort aus).
pub fn handle_interrupt() {
    let byte = unsafe { inb(DATA_PORT) };
    SCANCODES.lock().push(byte);
}

/// Ein Byte von außen einspeisen (für Tests und spätere Eingabequellen).
pub fn inject_scancode(byte: u8) {
    without_interrupts(|| SCANCODES.lock().push(byte));
}

/// Nächsten Tastendruck holen, ohne zu warten.
pub fn try_read_key() -> Option<KeyEvent> {
    loop {
        let byte = without_interrupts(|| SCANCODES.lock().pop())?;
        if let Some(event) = DECODER.lock().feed(byte) {
            return Some(event);
        }
    }
}

/// Auf den nächsten Tastendruck warten (schläft mit `hlt` zwischen Interrupts).
pub fn read_key() -> KeyEvent {
    loop {
        if let Some(event) = try_read_key() {
            return event;
        }
        // Atomar prüfen und schlafen: sonst könnte der IRQ zwischen Prüfen
        // und `hlt` kommen, und wir würden bis zum nächsten Interrupt hängen.
        crate::arch::x86_64::disable_interrupts();
        if SCANCODES.lock().is_empty() {
            unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
        } else {
            crate::arch::x86_64::enable_interrupts();
        }
    }
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
    fn ring_buffer_fifo_and_overflow() {
        let mut rb: RingBuffer<3> = RingBuffer::new();
        assert!(rb.push(1) && rb.push(2) && rb.push(3));
        assert!(!rb.push(4)); // voll
        assert_eq!(rb.pop(), Some(1));
        assert!(rb.push(5)); // Wrap-around
        assert_eq!((rb.pop(), rb.pop(), rb.pop(), rb.pop()), (Some(2), Some(3), Some(5), None));
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

    #[test_case]
    fn keyboard_irq_unmasked() {
        let (master, _) = crate::arch::x86_64::pic::masks();
        assert_eq!(master & 0b10, 0);
    }
}
