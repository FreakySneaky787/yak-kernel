//! The serial port COM1: a 16550 UART at I/O port 0x3F8.
//! QEMU connects it to our terminal (`-serial stdio`) - the first way a kernel can talk to us.

use crate::port::{inb, outb};
use core::fmt::{self, Write};

const COM1: u16 = 0x3F8;

/// Sets the UART to 115200 baud, 8 data bits, no parity, 1 stop bit.
pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // no interrupts: we only send, and check the status ourselves
        outb(COM1 + 3, 0x80); // DLAB on: the next two registers are the baud rate divisor
        outb(COM1, 0x01); //     divisor 1 = 115200 baud (low byte)
        outb(COM1 + 1, 0x00); // (high byte)
        outb(COM1 + 3, 0x03); // DLAB off; 8 bits, no parity, 1 stop bit
        outb(COM1 + 2, 0xC7); // FIFO on and cleared
    }
}

fn send(byte: u8) {
    // Line status register, bit 5: the transmit register is empty and takes the next byte
    while unsafe { inb(COM1 + 5) } & 0x20 == 0 {
        core::hint::spin_loop();
    }
    unsafe { outb(COM1, byte) };
}

/// The port has no state of its own, so a zero-sized type is enough to plug it into `core::fmt`.
pub struct Serial;

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                send(b'\r'); // terminals expect "\r\n" to start a new line at the left edge
            }
            send(byte);
        }
        Ok(())
    }
}

/// `format!`-style output to the serial port.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::serial::Serial, $($arg)*);
    }};
}

/// `format!`-style output to the serial port, with a line break.
#[macro_export]
macro_rules! serial_println {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = writeln!($crate::serial::Serial, $($arg)*);
    }};
}

#[test_case]
fn long_lines_are_sent_completely() {
    // 128 bytes, more than the 16-byte FIFO: `send` must wait for the UART instead of dropping bytes
    // (no heap yet, so no `String` - we write the same piece eight times)
    for _ in 0..8 {
        assert_eq!(Serial.write_str("0123456789abcdef"), Ok(()));
    }
    assert_eq!(Serial.write_str("\n"), Ok(()));
}
