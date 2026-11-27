//! A bitmap font in the PSF2 format (the format of the Linux text console).
//!
//! Layout: a 32-byte header, then `count` glyphs of `bytes_per_glyph` bytes each - one byte per pixel row for
//! fonts up to 8 pixels wide, the highest bit is the leftmost pixel - then a table that says which Unicode
//! characters each glyph shows (UTF-8 sequences, each glyph's list ended by 0xFF).

/// The font built into the kernel: Terminus 8x16 (SIL Open Font License, see assets/README.md).
pub static TERMINUS: &[u8] = include_bytes!("../assets/terminus-8x16.psf");

const PSF2_MAGIC: u32 = 0x864a_b572;
const HAS_UNICODE_TABLE: u32 = 0x01;

pub struct Font {
    data: &'static [u8],
    glyph_offset: usize,
    glyph_count: usize,
    pub width: usize,
    pub height: usize,
    /// Character (below 256) -> glyph number; `u16::MAX` = the font has no glyph for it.
    latin1: [u16; 256],
}

#[derive(Debug, PartialEq, Eq)]
pub enum FontError {
    TooShort,
    BadMagic,
    /// Wider than 8 pixels (more than one byte per row) - not needed for our font.
    Unsupported,
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
}

impl Font {
    pub fn parse(data: &'static [u8]) -> Result<Font, FontError> {
        if data.len() < 32 {
            return Err(FontError::TooShort);
        }
        if read_u32(data, 0) != PSF2_MAGIC {
            return Err(FontError::BadMagic);
        }
        let header_size = read_u32(data, 8) as usize;
        let flags = read_u32(data, 12);
        let glyph_count = read_u32(data, 16) as usize;
        let bytes_per_glyph = read_u32(data, 20) as usize;
        let height = read_u32(data, 24) as usize;
        let width = read_u32(data, 28) as usize;
        if width == 0 || width > 8 || bytes_per_glyph != height {
            return Err(FontError::Unsupported);
        }
        let table_offset = header_size + glyph_count * bytes_per_glyph;
        if data.len() < table_offset {
            return Err(FontError::TooShort);
        }
        let mut latin1 = [u16::MAX; 256];
        if flags & HAS_UNICODE_TABLE != 0 {
            read_unicode_table(&data[table_offset..], glyph_count, &mut latin1);
        } else {
            // no table: glyph n shows character n
            for (c, slot) in latin1.iter_mut().enumerate().take(glyph_count) {
                *slot = c as u16;
            }
        }
        Ok(Font {
            data,
            glyph_offset: header_size,
            glyph_count,
            width,
            height,
            latin1,
        })
    }

    /// The pixel rows of the glyph for `c` (one byte per row); characters the font lacks become '?'.
    pub fn glyph(&self, c: char) -> &[u8] {
        let index = self.index_of(c).or_else(|| self.index_of('?')).unwrap_or(0);
        let start = self.glyph_offset + index * self.height;
        &self.data[start..start + self.height]
    }

    fn index_of(&self, c: char) -> Option<usize> {
        let glyph = *self.latin1.get(c as usize)? as usize;
        (glyph < self.glyph_count).then_some(glyph)
    }
}

/// Fills `latin1` from the Unicode table: for each glyph a list of UTF-8 characters, ended by 0xFF.
/// (We only keep characters below 256 - enough for English and the German umlauts.)
fn read_unicode_table(table: &[u8], glyph_count: usize, latin1: &mut [u16; 256]) {
    let (mut glyph, mut i) = (0, 0);
    while i < table.len() && glyph < glyph_count {
        let len = match table[i] {
            0xFF => {
                glyph += 1;
                i += 1;
                continue;
            }
            0xFE => {
                i += 1; // start of a sequence of combining characters - skipped
                continue;
            }
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            _ => 4,
        };
        let c = table
            .get(i..i + len)
            .and_then(|b| core::str::from_utf8(b).ok())
            .and_then(|s| s.chars().next());
        if let Some(c) = c {
            if (c as usize) < 256 && latin1[c as usize] == u16::MAX {
                latin1[c as usize] = glyph as u16;
            }
        }
        i += len;
    }
}

#[test_case]
fn terminus_is_an_8x16_font() {
    let font = Font::parse(TERMINUS).expect("built-in font");
    assert_eq!((font.width, font.height), (8, 16));
}

#[test_case]
fn letters_have_pixels_and_space_has_none() {
    let font = Font::parse(TERMINUS).unwrap();
    assert!(font.glyph('A').iter().any(|&row| row != 0));
    assert!(font.glyph('ä').iter().any(|&row| row != 0));
    assert!(font.glyph(' ').iter().all(|&row| row == 0));
}

#[test_case]
fn missing_characters_become_question_marks() {
    let font = Font::parse(TERMINUS).unwrap();
    assert_eq!(font.glyph('€'), font.glyph('?')); // U+20AC is beyond the Latin-1 range we keep
}

#[test_case]
fn other_data_is_refused() {
    assert_eq!(Font::parse(&[0; 8]).err(), Some(FontError::TooShort));
    assert_eq!(Font::parse(&[0; 64]).err(), Some(FontError::BadMagic));
}
