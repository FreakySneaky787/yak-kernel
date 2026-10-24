//! Übersetzt PS/2-Scancodes (Set 1) in Tastendrücke.
//!
//! Set 1: Ein Byte pro Taste beim Drücken („make“), beim Loslassen dasselbe
//! Byte mit gesetztem Bit 7 („break“). Manche Tasten (Pfeile, AltGr, …)
//! kommen mit Präfix 0xE0. Pause schickt eine 6-Byte-Folge mit 0xE1.
//!
//! Reine Logik ohne Hardware-Zugriff – darum komplett per Unit-Test prüfbar.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Deutsches QWERTZ
    De,
    /// US-QWERTY
    Us,
}

#[derive(Default)]
struct Modifiers {
    left_shift: bool,
    right_shift: bool,
    ctrl: bool,
    alt: bool,
    alt_gr: bool,
    caps_lock: bool,
}

pub struct Decoder {
    layout: Layout,
    mods: Modifiers,
    extended: bool,
    /// Noch zu überspringende Bytes der Pause-Sequenz
    skip: u8,
}

impl Decoder {
    pub const fn new(layout: Layout) -> Decoder {
        Decoder {
            layout,
            mods: Modifiers {
                left_shift: false,
                right_shift: false,
                ctrl: false,
                alt: false,
                alt_gr: false,
                caps_lock: false,
            },
            extended: false,
            skip: 0,
        }
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }

    pub fn caps_lock(&self) -> bool {
        self.mods.caps_lock
    }

    /// Ein Byte vom Keyboard verarbeiten. Liefert ein Event nur beim Drücken
    /// einer Taste, die etwas erzeugt (nicht bei Modifiern oder Loslassen).
    pub fn feed(&mut self, byte: u8) -> Option<KeyEvent> {
        if self.skip > 0 {
            self.skip -= 1;
            return None;
        }
        match byte {
            0xE0 => {
                self.extended = true;
                return None;
            }
            0xE1 => {
                self.skip = 5; // Pause: E1 1D 45 E1 9D C5
                return None;
            }
            _ => {}
        }
        let extended = core::mem::replace(&mut self.extended, false);
        let released = byte & 0x80 != 0;
        let code = byte & 0x7F;

        if extended {
            self.feed_extended(code, released)
        } else {
            self.feed_normal(code, released)
        }
    }

    fn feed_normal(&mut self, code: u8, released: bool) -> Option<KeyEvent> {
        let pressed = !released;
        let key = match code {
            0x2A => return self.set(|m| &mut m.left_shift, pressed),
            0x36 => return self.set(|m| &mut m.right_shift, pressed),
            0x1D => return self.set(|m| &mut m.ctrl, pressed),
            0x38 => return self.set(|m| &mut m.alt, pressed),
            0x3A => {
                if pressed {
                    self.mods.caps_lock = !self.mods.caps_lock;
                }
                return None;
            }
            _ if released => return None,
            0x01 => Key::Escape,
            0x0E => Key::Backspace,
            0x0F => Key::Tab,
            0x1C => Key::Enter,
            0x3B..=0x44 => Key::F(code - 0x3B + 1),
            0x57 => Key::F(11),
            0x58 => Key::F(12),
            _ => Key::Char(self.translate(code)?),
        };
        Some(self.event(key))
    }

    fn feed_extended(&mut self, code: u8, released: bool) -> Option<KeyEvent> {
        let pressed = !released;
        let key = match code {
            0x1D => return self.set(|m| &mut m.ctrl, pressed),
            0x38 => return self.set(|m| &mut m.alt_gr, pressed),
            0x2A | 0x36 => return None, // „Fake-Shift“ von Druck/Pos1 & Co.
            _ if released => return None,
            0x1C => Key::Enter,
            0x35 => Key::Char('/'),
            0x47 => Key::Home,
            0x48 => Key::Up,
            0x49 => Key::PageUp,
            0x4B => Key::Left,
            0x4D => Key::Right,
            0x4F => Key::End,
            0x50 => Key::Down,
            0x51 => Key::PageDown,
            0x52 => Key::Insert,
            0x53 => Key::Delete,
            _ => return None,
        };
        Some(self.event(key))
    }

    fn set(&mut self, field: impl FnOnce(&mut Modifiers) -> &mut bool, value: bool) -> Option<KeyEvent> {
        *field(&mut self.mods) = value;
        None
    }

    fn event(&self, key: Key) -> KeyEvent {
        KeyEvent { key, ctrl: self.mods.ctrl, alt: self.mods.alt }
    }

    fn translate(&self, code: u8) -> Option<char> {
        let (normal, shifted, alt_gr) = layout_table(self.layout, code)?;
        if self.mods.alt_gr {
            return alt_gr;
        }
        let shift = self.mods.left_shift || self.mods.right_shift;
        // Caps Lock wirkt nur auf Buchstaben (inkl. Umlaute), nicht auf Ziffern/Zeichen
        let upper = if normal.is_alphabetic() { shift ^ self.mods.caps_lock } else { shift };
        Some(if upper { shifted } else { normal })
    }
}

/// (normal, mit Shift, mit AltGr) für eine Taste im gegebenen Layout.
fn layout_table(layout: Layout, code: u8) -> Option<(char, char, Option<char>)> {
    // Zuerst die layoutspezifischen Tasten …
    let specific = match layout {
        Layout::De => match code {
            0x02 => ('1', '!', None),
            0x03 => ('2', '"', Some('²')),
            0x04 => ('3', '§', Some('³')),
            0x05 => ('4', '$', None),
            0x06 => ('5', '%', None),
            0x07 => ('6', '&', None),
            0x08 => ('7', '/', Some('{')),
            0x09 => ('8', '(', Some('[')),
            0x0A => ('9', ')', Some(']')),
            0x0B => ('0', '=', Some('}')),
            0x0C => ('ß', '?', Some('\\')),
            0x0D => ('´', '`', None),
            0x10 => ('q', 'Q', Some('@')),
            0x15 => ('z', 'Z', None),
            0x1A => ('ü', 'Ü', None),
            0x1B => ('+', '*', Some('~')),
            0x27 => ('ö', 'Ö', None),
            0x28 => ('ä', 'Ä', None),
            0x29 => ('^', '°', None),
            0x2B => ('#', '\'', None),
            0x2C => ('y', 'Y', None),
            0x32 => ('m', 'M', Some('µ')),
            0x33 => (',', ';', None),
            0x34 => ('.', ':', None),
            0x35 => ('-', '_', None),
            0x53 => (',', ',', None), // Ziffernblock-Komma
            0x56 => ('<', '>', Some('|')),
            _ => ('\0', '\0', None),
        },
        Layout::Us => match code {
            0x02 => ('1', '!', None),
            0x03 => ('2', '@', None),
            0x04 => ('3', '#', None),
            0x05 => ('4', '$', None),
            0x06 => ('5', '%', None),
            0x07 => ('6', '^', None),
            0x08 => ('7', '&', None),
            0x09 => ('8', '*', None),
            0x0A => ('9', '(', None),
            0x0B => ('0', ')', None),
            0x0C => ('-', '_', None),
            0x0D => ('=', '+', None),
            0x15 => ('y', 'Y', None),
            0x1A => ('[', '{', None),
            0x1B => (']', '}', None),
            0x27 => (';', ':', None),
            0x28 => ('\'', '"', None),
            0x29 => ('`', '~', None),
            0x2B => ('\\', '|', None),
            0x2C => ('z', 'Z', None),
            0x33 => (',', '<', None),
            0x34 => ('.', '>', None),
            0x35 => ('/', '?', None),
            0x53 => ('.', '.', None),
            0x56 => ('\\', '|', None),
            _ => ('\0', '\0', None),
        },
    };
    if specific.0 != '\0' {
        return Some(specific);
    }
    // … dann die, die in beiden Layouts gleich sind
    let letter = |c: char| Some((c, c.to_ascii_uppercase(), None));
    match code {
        0x11 => letter('w'),
        0x12 => letter('e'),
        0x13 => letter('r'),
        0x14 => letter('t'),
        0x16 => letter('u'),
        0x17 => letter('i'),
        0x18 => letter('o'),
        0x19 => letter('p'),
        0x10 => letter('q'),
        0x1E => letter('a'),
        0x1F => letter('s'),
        0x20 => letter('d'),
        0x21 => letter('f'),
        0x22 => letter('g'),
        0x23 => letter('h'),
        0x24 => letter('j'),
        0x25 => letter('k'),
        0x26 => letter('l'),
        0x2D => letter('x'),
        0x2E => letter('c'),
        0x2F => letter('v'),
        0x30 => letter('b'),
        0x31 => letter('n'),
        0x32 => letter('m'),
        0x39 => Some((' ', ' ', None)),
        // Ziffernblock (NumLock wird als „an“ angenommen)
        0x37 => Some(('*', '*', None)),
        0x47 => Some(('7', '7', None)),
        0x48 => Some(('8', '8', None)),
        0x49 => Some(('9', '9', None)),
        0x4A => Some(('-', '-', None)),
        0x4B => Some(('4', '4', None)),
        0x4C => Some(('5', '5', None)),
        0x4D => Some(('6', '6', None)),
        0x4E => Some(('+', '+', None)),
        0x4F => Some(('1', '1', None)),
        0x50 => Some(('2', '2', None)),
        0x51 => Some(('3', '3', None)),
        0x52 => Some(('0', '0', None)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(d: &mut Decoder, bytes: &[u8]) -> Option<KeyEvent> {
        let mut last = None;
        for &b in bytes {
            if let Some(e) = d.feed(b) {
                last = Some(e);
            }
        }
        last
    }

    fn ch(d: &mut Decoder, bytes: &[u8]) -> Option<char> {
        match feed_all(d, bytes)?.key {
            Key::Char(c) => Some(c),
            _ => None,
        }
    }

    #[test_case]
    fn press_and_release() {
        let mut d = Decoder::new(Layout::De);
        assert_eq!(ch(&mut d, &[0x1E]), Some('a'));
        assert_eq!(d.feed(0x9E), None);
    }

    #[test_case]
    fn shift_makes_uppercase() {
        let mut d = Decoder::new(Layout::De);
        assert_eq!(ch(&mut d, &[0x2A, 0x1E]), Some('A'));
        assert_eq!(ch(&mut d, &[0xAA, 0x1E]), Some('a'));
        assert_eq!(ch(&mut d, &[0x36, 0x02]), Some('!'));
    }

    #[test_case]
    fn qwertz_vs_qwerty() {
        let mut de = Decoder::new(Layout::De);
        let mut us = Decoder::new(Layout::Us);
        assert_eq!(ch(&mut de, &[0x15]), Some('z'));
        assert_eq!(ch(&mut de, &[0x2C]), Some('y'));
        assert_eq!(ch(&mut us, &[0x15]), Some('y'));
        assert_eq!(ch(&mut us, &[0x2C]), Some('z'));
        assert_eq!(ch(&mut de, &[0x2A, 0x03]), Some('"'));
        assert_eq!(ch(&mut us, &[0x2A, 0x03]), Some('@'));
    }

    #[test_case]
    fn umlauts() {
        let mut d = Decoder::new(Layout::De);
        assert_eq!(ch(&mut d, &[0x27]), Some('ö'));
        assert_eq!(ch(&mut d, &[0x28]), Some('ä'));
        assert_eq!(ch(&mut d, &[0x1A]), Some('ü'));
        assert_eq!(ch(&mut d, &[0x0C]), Some('ß'));
        assert_eq!(ch(&mut d, &[0x2A, 0x27]), Some('Ö'));
    }

    #[test_case]
    fn caps_lock_only_letters() {
        let mut d = Decoder::new(Layout::De);
        feed_all(&mut d, &[0x3A, 0xBA]);
        assert!(d.caps_lock());
        assert_eq!(ch(&mut d, &[0x1E]), Some('A'));
        assert_eq!(ch(&mut d, &[0x28]), Some('Ä'));
        assert_eq!(ch(&mut d, &[0x02]), Some('1'));
        assert_eq!(ch(&mut d, &[0x2A, 0x1E]), Some('a')); // Shift hebt Caps auf
    }

    #[test_case]
    fn alt_gr_symbols() {
        let mut d = Decoder::new(Layout::De);
        assert_eq!(ch(&mut d, &[0xE0, 0x38, 0x10]), Some('@'));
        assert_eq!(ch(&mut d, &[0x08]), Some('{'));
        assert_eq!(ch(&mut d, &[0x56]), Some('|'));
        assert_eq!(ch(&mut d, &[0xE0, 0xB8, 0x10]), Some('q'));
    }

    #[test_case]
    fn extended_keys() {
        let mut d = Decoder::new(Layout::De);
        assert_eq!(feed_all(&mut d, &[0xE0, 0x48]).unwrap().key, Key::Up);
        assert_eq!(feed_all(&mut d, &[0xE0, 0x53]).unwrap().key, Key::Delete);
        assert_eq!(d.feed(0xE0), None);
        assert_eq!(d.feed(0xC8), None); // Up losgelassen
        // Nach einer E0-Sequenz ist 0x48 wieder Ziffernblock-8
        assert_eq!(ch(&mut d, &[0x48]), Some('8'));
    }

    #[test_case]
    fn ctrl_flag() {
        let mut d = Decoder::new(Layout::De);
        let e = feed_all(&mut d, &[0x1D, 0x2E]).unwrap();
        assert_eq!(e, KeyEvent { key: Key::Char('c'), ctrl: true, alt: false });
        let e = feed_all(&mut d, &[0x9D, 0x2E]).unwrap();
        assert!(!e.ctrl);
    }

    #[test_case]
    fn special_keys() {
        let mut d = Decoder::new(Layout::De);
        assert_eq!(feed_all(&mut d, &[0x1C]).unwrap().key, Key::Enter);
        assert_eq!(feed_all(&mut d, &[0x0E]).unwrap().key, Key::Backspace);
        assert_eq!(feed_all(&mut d, &[0x3B]).unwrap().key, Key::F(1));
        assert_eq!(feed_all(&mut d, &[0x58]).unwrap().key, Key::F(12));
    }

    #[test_case]
    fn pause_sequence_ignored() {
        let mut d = Decoder::new(Layout::De);
        assert_eq!(feed_all(&mut d, &[0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5]), None);
        assert_eq!(ch(&mut d, &[0x1E]), Some('a'));
    }
}
