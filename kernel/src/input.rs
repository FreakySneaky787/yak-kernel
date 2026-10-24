//! Unified input: key events from the PS/2 keyboard **and** from the serial
//! port. Serial input lets you use the shell from a terminal (`make run`
//! prints to and reads from stdio) and lets tests drive the shell.
//!
//! `TerminalDecoder` turns the byte stream a terminal sends into `KeyEvent`s:
//! printable UTF-8, Enter (CR / LF / CRLF), Backspace (DEL or BS), Ctrl+letter
//! (0x01-0x1A) and ANSI escape sequences for arrows, Home/End and Delete.

use crate::arch::x86_64::{disable_interrupts, enable_interrupts};
use crate::drivers::keyboard;
use crate::drivers::scancode::{Key, KeyEvent};
use crate::serial;
use spin::Mutex;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    Escape,
    /// `ESC [` plus an optional numeric parameter (for `ESC [ 3 ~`)
    Csi(u8),
    /// Remaining continuation bytes and the code point collected so far
    Utf8 { remaining: u8, acc: u32 },
}

pub struct TerminalDecoder {
    state: State,
    last_was_cr: bool,
}

fn plain(key: Key) -> KeyEvent {
    KeyEvent { key, ctrl: false, alt: false }
}

impl TerminalDecoder {
    pub const fn new() -> TerminalDecoder {
        TerminalDecoder { state: State::Normal, last_was_cr: false }
    }

    pub fn feed(&mut self, byte: u8) -> Option<KeyEvent> {
        let was_cr = core::mem::replace(&mut self.last_was_cr, false);
        match self.state {
            State::Normal => match byte {
                b'\r' => {
                    self.last_was_cr = true;
                    Some(plain(Key::Enter))
                }
                b'\n' if was_cr => None, // second half of CRLF
                b'\n' => Some(plain(Key::Enter)),
                0x7F | 0x08 => Some(plain(Key::Backspace)),
                b'\t' => Some(plain(Key::Tab)),
                0x1B => {
                    self.state = State::Escape;
                    None
                }
                0x01..=0x1A => Some(KeyEvent { key: Key::Char((b'a' + byte - 1) as char), ctrl: true, alt: false }),
                0x20..=0x7E => Some(plain(Key::Char(byte as char))),
                0xC0..=0xDF => self.start_utf8(1, byte & 0x1F),
                0xE0..=0xEF => self.start_utf8(2, byte & 0x0F),
                0xF0..=0xF7 => self.start_utf8(3, byte & 0x07),
                _ => None,
            },
            State::Escape => {
                if byte == b'[' {
                    self.state = State::Csi(0);
                    None
                } else {
                    self.state = State::Normal;
                    Some(plain(Key::Escape))
                }
            }
            State::Csi(param) => {
                let key = match byte {
                    b'0'..=b'9' => {
                        self.state = State::Csi(param.saturating_mul(10).saturating_add(byte - b'0'));
                        return None;
                    }
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    b'C' => Some(Key::Right),
                    b'D' => Some(Key::Left),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    b'~' => match param {
                        1 | 7 => Some(Key::Home),
                        2 => Some(Key::Insert),
                        3 => Some(Key::Delete),
                        4 | 8 => Some(Key::End),
                        5 => Some(Key::PageUp),
                        6 => Some(Key::PageDown),
                        _ => None,
                    },
                    _ => None,
                };
                self.state = State::Normal;
                key.map(plain)
            }
            State::Utf8 { remaining, acc } => {
                if byte & 0xC0 != 0x80 {
                    self.state = State::Normal; // invalid sequence: drop it
                    return None;
                }
                let acc = (acc << 6) | (byte & 0x3F) as u32;
                if remaining == 1 {
                    self.state = State::Normal;
                    char::from_u32(acc).map(|c| plain(Key::Char(c)))
                } else {
                    self.state = State::Utf8 { remaining: remaining - 1, acc };
                    None
                }
            }
        }
    }

    fn start_utf8(&mut self, remaining: u8, bits: u8) -> Option<KeyEvent> {
        self.state = State::Utf8 { remaining, acc: bits as u32 };
        None
    }
}

static SERIAL_DECODER: Mutex<TerminalDecoder> = Mutex::new(TerminalDecoder::new());

/// Next key event from keyboard or serial port, without waiting.
pub fn try_read_key() -> Option<KeyEvent> {
    if let Some(event) = keyboard::try_read_key() {
        return Some(event);
    }
    while let Some(byte) = serial::try_read_byte() {
        if let Some(event) = SERIAL_DECODER.lock().feed(byte) {
            return Some(event);
        }
    }
    None
}

/// Waits for the next key event from keyboard or serial port.
pub fn read_key() -> KeyEvent {
    loop {
        if let Some(event) = try_read_key() {
            return event;
        }
        // Check and sleep atomically (see keyboard::read_key)
        disable_interrupts();
        if keyboard::has_pending() || serial::has_pending() {
            enable_interrupts();
        } else {
            unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> alloc::vec::Vec<KeyEvent> {
        let mut d = TerminalDecoder::new();
        bytes.iter().filter_map(|&b| d.feed(b)).collect()
    }

    fn keys(bytes: &[u8]) -> alloc::vec::Vec<Key> {
        decode(bytes).into_iter().map(|e| e.key).collect()
    }

    #[test_case]
    fn printable_and_enter_variants() {
        assert_eq!(keys(b"hi\r"), [Key::Char('h'), Key::Char('i'), Key::Enter]);
        assert_eq!(keys(b"a\r\nb\n"), [Key::Char('a'), Key::Enter, Key::Char('b'), Key::Enter]);
        assert_eq!(keys(b"\r\r"), [Key::Enter, Key::Enter]);
    }

    #[test_case]
    fn backspace_tab_and_ctrl() {
        assert_eq!(keys(b"\x7f\x08\t"), [Key::Backspace, Key::Backspace, Key::Tab]);
        let e = decode(b"\x03");
        assert_eq!(e[0], KeyEvent { key: Key::Char('c'), ctrl: true, alt: false });
    }

    #[test_case]
    fn ansi_sequences() {
        assert_eq!(keys(b"\x1b[A\x1b[B\x1b[C\x1b[D"), [Key::Up, Key::Down, Key::Right, Key::Left]);
        assert_eq!(keys(b"\x1b[H\x1b[F\x1b[3~\x1b[1~\x1b[4~"), [Key::Home, Key::End, Key::Delete, Key::Home, Key::End]);
        assert_eq!(keys(b"\x1bx"), [Key::Escape]);
    }

    #[test_case]
    fn utf8_umlauts() {
        assert_eq!(keys("äß€".as_bytes()), [Key::Char('ä'), Key::Char('ß'), Key::Char('€')]);
        assert_eq!(keys(b"\xc3x"), []); // broken sequence is dropped
    }

    #[test_case]
    fn serial_rx_reaches_read_key() {
        while try_read_key().is_some() {}
        serial::inject_rx(b"ok\r");
        assert_eq!(read_key().key, Key::Char('o'));
        assert_eq!(read_key().key, Key::Char('k'));
        assert_eq!(read_key().key, Key::Enter);
        assert!(try_read_key().is_none());
    }
}
