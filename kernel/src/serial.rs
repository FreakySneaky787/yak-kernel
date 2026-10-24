//! Driver for the 16550 UART on COM1 (port 0x3F8).
//! QEMU redirects COM1 to the terminal with `-serial stdio` - our most important
//! debug and test channel, because it works without a screen.

use crate::arch::x86_64::port::{inb, outb};
use crate::arch::x86_64::without_interrupts;
use crate::ringbuf::RingBuffer;
use core::fmt::{self, Write};
use spin::Mutex;

const COM1: u16 = 0x3F8;

pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    const fn new(base: u16) -> Self {
        Self { base }
    }

    fn init(&mut self) {
        let b = self.base;
        unsafe {
            outb(b + 1, 0x00); // disable UART interrupts
            outb(b + 3, 0x80); // DLAB on: set baud rate divisor
            outb(b, 0x01); // divisor 1 -> 115200 baud
            outb(b + 1, 0x00);
            outb(b + 3, 0x03); // 8 bits, no parity, 1 stop bit (DLAB off)
            outb(b + 2, 0xC7); // FIFO on, cleared, 14-byte threshold
            outb(b + 4, 0x0B); // DTR + RTS + OUT2 (OUT2 routes the UART interrupt to the PIC)
            outb(b + 1, 0x01); // interrupt on "received data available"
        }
    }

    fn send(&mut self, byte: u8) {
        // Wait until the transmit register is empty (line status register bit 5)
        while unsafe { inb(self.base + 5) } & 0x20 == 0 {
            core::hint::spin_loop();
        }
        unsafe { outb(self.base, byte) };
    }
}

impl Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.send(b'\r');
            }
            self.send(byte);
        }
        Ok(())
    }
}

pub static SERIAL: Mutex<SerialPort> = Mutex::new(SerialPort::new(COM1));

/// Bytes received on COM1, filled by the IRQ4 handler.
static RX: Mutex<RingBuffer<256>> = Mutex::new(RingBuffer::new());

/// Called from the IRQ4 handler: drain the UART receive FIFO into `RX`.
pub fn handle_interrupt() {
    let mut rx = RX.lock();
    // Line status register bit 0 = data ready
    while unsafe { inb(COM1 + 5) } & 0x01 != 0 {
        rx.push(unsafe { inb(COM1) });
    }
}

/// Next received byte, if any.
pub fn try_read_byte() -> Option<u8> {
    without_interrupts(|| RX.lock().pop())
}

/// Whether received bytes are waiting (call with interrupts disabled for a race-free check).
pub fn has_pending() -> bool {
    !RX.lock().is_empty()
}

/// Feed bytes as if they were received (for tests).
pub fn inject_rx(bytes: &[u8]) {
    without_interrupts(|| {
        let mut rx = RX.lock();
        for &b in bytes {
            rx.push(b);
        }
    });
}

pub fn init() {
    SERIAL.lock().init();
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    crate::arch::x86_64::without_interrupts(|| _print_locked(args));
}

/// Like `_print`, but the caller has already disabled interrupts.
pub(crate) fn _print_locked(args: fmt::Arguments) {
    SERIAL.lock().write_fmt(args).ok();
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => { $crate::serial::_print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! serial_println {
    () => { $crate::serial_print!("\n") };
    ($($arg:tt)*) => { $crate::serial_print!("{}\n", format_args!($($arg)*)) };
}
