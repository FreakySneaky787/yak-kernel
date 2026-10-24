//! PSF2 bitmap font (the Linux console font format).
//!
//! Layout: 32-byte header, then `count` glyphs of `bytes_per_glyph` bytes each
//! (one row = one byte for 8-pixel-wide glyphs, MSB = leftmost pixel), followed by
//! an optional Unicode table: UTF-8 sequences per glyph, terminated by 0xFF.

const PSF2_MAGIC: u32 = 0x864a_b572;
const PSF2_HAS_UNICODE_TABLE: u32 = 0x01;

pub struct Font {
    data: &'static [u8],
    glyph_offset: usize,
    glyph_count: usize,
    bytes_per_glyph: usize,
    pub width: usize,
    pub height: usize,
    /// Code point (< 256) -> glyph index. `u16::MAX` = not present.
    latin1_map: [u16; 256],
}

#[derive(Debug, PartialEq, Eq)]
pub enum FontError {
    TooShort,
    BadMagic,
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

        // We only support glyphs up to 8 pixels wide (1 byte per row)
        if width == 0 || width > 8 || bytes_per_glyph != height {
            return Err(FontError::Unsupported);
        }
        let table_offset = header_size + glyph_count * bytes_per_glyph;
        if data.len() < table_offset {
            return Err(FontError::TooShort);
        }

        let mut latin1_map = [u16::MAX; 256];
        if flags & PSF2_HAS_UNICODE_TABLE != 0 {
            parse_unicode_table(&data[table_offset..], glyph_count, &mut latin1_map);
        } else {
            for (i, slot) in latin1_map.iter_mut().enumerate().take(glyph_count) {
                *slot = i as u16;
            }
        }

        Ok(Font {
            data,
            glyph_offset: header_size,
            glyph_count,
            bytes_per_glyph,
            width,
            height,
            latin1_map,
        })
    }

    /// Bitmap rows of the glyph for `c`; unknown characters become '?'.
    pub fn glyph(&self, c: char) -> &[u8] {
        let index = self.index_of(c).or_else(|| self.index_of('?')).unwrap_or(0);
        let start = self.glyph_offset + index * self.bytes_per_glyph;
        &self.data[start..start + self.bytes_per_glyph]
    }

    fn index_of(&self, c: char) -> Option<usize> {
        let cp = c as usize;
        if cp >= 256 {
            return None;
        }
        match self.latin1_map[cp] {
            u16::MAX => None,
            i if (i as usize) < self.glyph_count => Some(i as usize),
            _ => None,
        }
    }
}

fn parse_unicode_table(table: &[u8], glyph_count: usize, map: &mut [u16; 256]) {
    let mut glyph = 0usize;
    let mut i = 0usize;
    while i < table.len() && glyph < glyph_count {
        let b = table[i];
        match b {
            0xFF => {
                glyph += 1;
                i += 1;
            }
            0xFE => i += 1, // start of combining sequences - skip
            _ => {
                let len = match b {
                    0x00..=0x7F => 1,
                    0xC0..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    _ => 4,
                };
                if let Some(bytes) = table.get(i..i + len) {
                    if let Some(c) = core::str::from_utf8(bytes).ok().and_then(|s| s.chars().next()) {
                        let cp = c as usize;
                        if cp < 256 && map[cp] == u16::MAX {
                            map[cp] = glyph as u16;
                        }
                    }
                }
                i += len;
            }
        }
    }
}

pub static FONT_DATA: &[u8] = include_bytes!("../assets/terminus-8x16.psf");

#[test_case]
fn parses_builtin_font() {
    let font = Font::parse(FONT_DATA).unwrap();
    assert_eq!((font.width, font.height), (8, 16));
}

#[test_case]
fn ascii_and_umlauts_have_glyphs() {
    let font = Font::parse(FONT_DATA).unwrap();
    for c in ['A', 'z', '0', '~', 'ä', 'ö', 'ü', 'ß'] {
        assert!(font.index_of(c).is_some(), "no glyph for {}", c);
    }
    // space is blank, 'A' is not
    assert!(font.glyph(' ').iter().all(|&row| row == 0));
    assert!(font.glyph('A').iter().any(|&row| row != 0));
}

#[test_case]
fn rejects_garbage() {
    static GARBAGE: [u8; 40] = [0x42; 40];
    assert_eq!(Font::parse(&GARBAGE).err(), Some(FontError::BadMagic));
}
