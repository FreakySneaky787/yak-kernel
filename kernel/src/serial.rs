//! Treiber für den UART 16550 an COM1 (Port 0x3F8).
//! QEMU leitet COM1 mit `-serial stdio` ins Terminal um – unser wichtigster
//! Debug- und Testkanal, weil er auch ohne Bildschirm funktioniert.

use crate::arch::x86_64::port::{inb, outb};
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
            outb(b + 1, 0x00); // Interrupts aus
            outb(b + 3, 0x80); // DLAB an: Baudraten-Teiler setzen
            outb(b, 0x01); // Teiler 1 → 115200 Baud
            outb(b + 1, 0x00);
            outb(b + 3, 0x03); // 8 Bit, keine Parität, 1 Stoppbit (DLAB aus)
            outb(b + 2, 0xC7); // FIFO an, geleert, 14-Byte-Schwelle
            outb(b + 4, 0x0B); // DTR + RTS + OUT2
        }
    }

    fn send(&mut self, byte: u8) {
        // Warten bis das Senderegister leer ist (Line Status Register Bit 5)
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

pub fn init() {
    SERIAL.lock().init();
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    // TODO(M6): Interrupts sperren, sobald es Interrupt-Handler gibt, die drucken
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
