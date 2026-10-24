//! Key types for the line editor (same definitions as the kernel's
//! `drivers::scancode`, which the shared `line_editor.rs` expects).

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
}

impl KeyEvent {
    pub fn plain(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            ctrl: false,
            alt: false,
        }
    }
}

/// Turns the bytes `read(0)` delivers (UTF-8, control codes, VT100 escape
/// sequences for arrows etc.) back into key events.
pub struct Decoder {
    state: State,
}

enum State {
    Normal,
    Escape,
    /// Inside `ESC [`, collecting the numeric parameter.
    Csi(u32),
    /// Inside a UTF-8 sequence.
    Utf8 {
        remaining: u8,
        acc: u32,
    },
}

impl Decoder {
    pub const fn new() -> Decoder {
        Decoder { state: State::Normal }
    }

    pub fn feed(&mut self, byte: u8) -> Option<KeyEvent> {
        match self.state {
            State::Normal => self.normal(byte),
            State::Escape => {
                self.state = State::Normal;
                if byte == b'[' {
                    self.state = State::Csi(0);
                    None
                } else {
                    Some(KeyEvent::plain(Key::Escape))
                }
            }
            State::Csi(param) => {
                if byte.is_ascii_digit() {
                    self.state = State::Csi(param * 10 + (byte - b'0') as u32);
                    return None;
                }
                self.state = State::Normal;
                let key = match (byte, param) {
                    (b'A', _) => Key::Up,
                    (b'B', _) => Key::Down,
                    (b'C', _) => Key::Right,
                    (b'D', _) => Key::Left,
                    (b'H', _) | (b'~', 1) | (b'~', 7) => Key::Home,
                    (b'F', _) | (b'~', 4) | (b'~', 8) => Key::End,
                    (b'~', 2) => Key::Insert,
                    (b'~', 3) => Key::Delete,
                    (b'~', 5) => Key::PageUp,
                    (b'~', 6) => Key::PageDown,
                    _ => return None,
                };
                Some(KeyEvent::plain(key))
            }
            State::Utf8 { remaining, acc } => {
                if byte & 0xC0 != 0x80 {
                    self.state = State::Normal;
                    return self.normal(byte);
                }
                let acc = (acc << 6) | (byte & 0x3F) as u32;
                if remaining == 1 {
                    self.state = State::Normal;
                    char::from_u32(acc).map(|c| KeyEvent::plain(Key::Char(c)))
                } else {
                    self.state = State::Utf8 {
                        remaining: remaining - 1,
                        acc,
                    };
                    None
                }
            }
        }
    }

    fn normal(&mut self, byte: u8) -> Option<KeyEvent> {
        let key = match byte {
            0x1B => {
                self.state = State::Escape;
                return None;
            }
            b'\n' | b'\r' => Key::Enter,
            0x08 | 0x7F => Key::Backspace,
            b'\t' => Key::Tab,
            0x01..=0x1A => {
                return Some(KeyEvent {
                    key: Key::Char((b'a' + byte - 1) as char),
                    ctrl: true,
                    alt: false,
                });
            }
            0x20..=0x7E => Key::Char(byte as char),
            0xC0..=0xDF => return self.start_utf8(1, byte & 0x1F),
            0xE0..=0xEF => return self.start_utf8(2, byte & 0x0F),
            0xF0..=0xF7 => return self.start_utf8(3, byte & 0x07),
            _ => return None,
        };
        Some(KeyEvent::plain(key))
    }

    fn start_utf8(&mut self, remaining: u8, bits: u8) -> Option<KeyEvent> {
        self.state = State::Utf8 {
            remaining,
            acc: bits as u32,
        };
        None
    }
}
