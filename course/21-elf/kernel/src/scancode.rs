//! Turns the bytes of a PS/2 keyboard (scancode set 1) into keys - pure logic without hardware, so the tests can
//! feed it any bytes they like.
//!
//! Pressing a key sends its code ("make"), releasing it the same code with bit 7 set ("break"). A code names the
//! *position* of a key, not its letter: 0x15 is the key right of T - a Y on a US keyboard, a Z on a German one.
//! What a position means is the *layout*; ours is the US one, whose names QEMU's `sendkey` uses too.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
}

const LEFT_SHIFT: u8 = 0x2A;
const RIGHT_SHIFT: u8 = 0x36;
const CAPS_LOCK: u8 = 0x3A;
const ENTER: u8 = 0x1C;
const BACKSPACE: u8 = 0x0E;
const RELEASED: u8 = 0x80;

/// The US layout, one keyboard row at a time: the code of the row's first key, its characters without Shift and
/// with Shift. The codes of a row follow each other (Q is 0x10, W 0x11, ...).
const ROWS: [(u8, &str, &str); 5] = [
    (0x02, "1234567890-=", "!@#$%^&*()_+"),
    (0x10, "qwertyuiop[]", "QWERTYUIOP{}"),
    (0x1E, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
    (0x2B, "\\zxcvbnm,./", "|ZXCVBNM<>?"),
    (0x39, " ", " "),
];

pub struct Decoder {
    left_shift: bool,
    right_shift: bool,
    caps_lock: bool,
    /// The last byte was a prefix (0xE0 or 0xE1): the next one belongs to a key we do not handle.
    after_prefix: bool,
}

impl Decoder {
    pub const fn new() -> Decoder {
        Decoder {
            left_shift: false,
            right_shift: false,
            caps_lock: false,
            after_prefix: false,
        }
    }

    /// Takes one byte from the keyboard. Gives a key when one that types something is pressed - nothing for
    /// Shift, Caps Lock or a release.
    pub fn feed(&mut self, byte: u8) -> Option<Key> {
        if core::mem::replace(&mut self.after_prefix, false) {
            return None;
        }
        if byte == 0xE0 || byte == 0xE1 {
            // keys newer than the first PC keyboards (arrows, right Ctrl, ...) send 0xE0 and one byte; Pause sends
            // E1 1D 45 E1 9D C5 - two prefixed pairs, then 45 and C5, which are no key we know
            self.after_prefix = true;
            return None;
        }
        let pressed = byte & RELEASED == 0;
        match byte & !RELEASED {
            LEFT_SHIFT => self.left_shift = pressed,
            RIGHT_SHIFT => self.right_shift = pressed,
            CAPS_LOCK if pressed => self.caps_lock = !self.caps_lock,
            _ if !pressed => {}
            ENTER => return Some(Key::Enter),
            BACKSPACE => return Some(Key::Backspace),
            code => return self.character(code).map(Key::Char),
        }
        None
    }

    /// The character of the key at `code`, if it has one in our layout.
    fn character(&self, code: u8) -> Option<char> {
        let &(first, normal, shifted) = ROWS
            .iter()
            .find(|(first, normal, _)| (*first..*first + normal.len() as u8).contains(&code))?;
        let i = usize::from(code - first);
        let shift = self.left_shift || self.right_shift;
        // Caps Lock makes letters capitals and leaves digits and symbols alone; Shift changes every key
        let upper = if normal.as_bytes()[i].is_ascii_alphabetic() {
            shift != self.caps_lock
        } else {
            shift
        };
        let row = if upper { shifted } else { normal };
        Some(char::from(row.as_bytes()[i]))
    }
}

#[cfg(test)]
mod tests {
    use super::Key::{Backspace, Char, Enter};
    use super::*;

    /// Feeds the bytes one by one and checks what each one gives.
    fn check(decoder: &mut Decoder, steps: &[(u8, Option<Key>)]) {
        for &(byte, key) in steps {
            assert_eq!(decoder.feed(byte), key, "byte {:#x}", byte);
        }
    }

    #[test_case]
    fn a_key_types_when_pressed_not_when_released() {
        let mut d = Decoder::new();
        check(
            &mut d,
            &[
                (0x23, Some(Char('h'))),
                (0xA3, None),
                (0x02, Some(Char('1'))),
                (0x82, None),
            ],
        );
        check(
            &mut d,
            &[
                (0x15, Some(Char('y'))),
                (0x39, Some(Char(' '))),
                (0x2B, Some(Char('\\'))),
            ],
        );
        check(&mut d, &[(0x1C, Some(Enter)), (0x9C, None), (0x0E, Some(Backspace))]);
    }

    #[test_case]
    fn shift_counts_while_it_is_held() {
        let mut d = Decoder::new();
        check(
            &mut d,
            &[
                (0x2A, None),
                (0x23, Some(Char('H'))),
                (0x02, Some(Char('!'))),
                (0xAA, None),
            ],
        );
        check(&mut d, &[(0x23, Some(Char('h')))]);
        check(
            &mut d,
            &[
                (0x36, None),
                (0x35, Some(Char('?'))),
                (0xB6, None),
                (0x35, Some(Char('/'))),
            ],
        );
    }

    #[test_case]
    fn caps_lock_toggles_letters_only() {
        let mut d = Decoder::new();
        check(
            &mut d,
            &[
                (0x3A, None),
                (0xBA, None),
                (0x1E, Some(Char('A'))),
                (0x02, Some(Char('1'))),
            ],
        );
        check(
            &mut d,
            &[
                (0x2A, None),
                (0x1E, Some(Char('a'))),
                (0x02, Some(Char('!'))),
                (0xAA, None),
            ],
        );
        check(&mut d, &[(0x3A, None), (0xBA, None), (0x1E, Some(Char('a')))]);
    }

    #[test_case]
    fn prefixed_keys_are_skipped() {
        let mut d = Decoder::new();
        // E0 48 = arrow up, not "8"; E0 2A = a "fake shift" some keys send, which must not make capitals
        check(
            &mut d,
            &[
                (0xE0, None),
                (0x48, None),
                (0xE0, None),
                (0x2A, None),
                (0x1E, Some(Char('a'))),
            ],
        );
        // Pause, then a key that must come through
        for byte in [0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5] {
            assert_eq!(d.feed(byte), None);
        }
        check(&mut d, &[(0x1C, Some(Enter))]);
    }
}
