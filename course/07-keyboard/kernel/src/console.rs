//! A text console on a framebuffer: a grid of character cells, each drawn pixel by pixel with the font.
//! `print!`/`println!` write to it and to the serial port.

use crate::font::{self, Font};
use core::fmt;
use spin::Mutex;

const FOREGROUND: u32 = 0x00E0_E0E0; // 0x00RRGGBB: light grey
const BACKGROUND: u32 = 0x0010_1018; // almost black, a little blue

pub struct Console<'a> {
    /// The pixels, row after row. One `u32` per pixel (32 bits per pixel, 0x00RRGGBB).
    pixels: &'a mut [u32],
    /// Pixels from the start of one row to the start of the next - can be more than the visible width.
    stride: usize,
    font: Font,
    /// Size of the character grid, and the cursor in it.
    columns: usize,
    rows: usize,
    column: usize,
    row: usize,
}

impl<'a> Console<'a> {
    /// A console on `pixels` (a `width` x `height` picture whose rows are `stride` pixels apart), cleared.
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize, stride: usize, font: Font) -> Console<'a> {
        let (columns, rows) = (width / font.width, height / font.height);
        let mut console = Console {
            pixels,
            stride,
            font,
            columns,
            rows,
            column: 0,
            row: 0,
        };
        console.clear();
        console
    }

    pub fn size(&self) -> (usize, usize) {
        (self.columns, self.rows)
    }

    pub fn clear(&mut self) {
        self.pixels.fill(BACKGROUND);
        (self.column, self.row) = (0, 0);
    }

    pub fn put_char(&mut self, c: char) {
        match c {
            '\n' => self.new_line(),
            '\r' => self.column = 0,
            // backspace: one cell back; erasing the character is "\x08 \x08" (back, a space over it, back)
            '\x08' => self.column = self.column.saturating_sub(1),
            c => {
                if self.column == self.columns {
                    self.new_line(); // the line is full: continue on the next one
                }
                self.draw(c, self.column, self.row);
                self.column += 1;
            }
        }
    }

    /// Draws the glyph of `c` into the cell at `column`, `row`: every font bit becomes one pixel.
    fn draw(&mut self, c: char, column: usize, row: usize) {
        let (x0, y0) = (column * self.font.width, row * self.font.height);
        for (dy, bits) in self.font.glyph(c).iter().enumerate() {
            let line = (y0 + dy) * self.stride + x0;
            for dx in 0..self.font.width {
                let on = bits & (0x80 >> dx) != 0; // the highest bit is the leftmost pixel
                self.pixels[line + dx] = if on { FOREGROUND } else { BACKGROUND };
            }
        }
    }

    fn new_line(&mut self) {
        self.column = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.scroll();
        }
    }

    /// Moves every text row up by one (copying pixels) and clears the bottom one.
    fn scroll(&mut self) {
        let row_pixels = self.font.height * self.stride; // one row of text, in pixels
        let used = self.rows * row_pixels;
        self.pixels.copy_within(row_pixels..used, 0);
        self.pixels[used - row_pixels..used].fill(BACKGROUND);
    }
}

impl fmt::Write for Console<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.chars().for_each(|c| self.put_char(c));
        Ok(())
    }
}

/// The console on the screen - `None` until `init` found a framebuffer.
pub static CONSOLE: Mutex<Option<Console<'static>>> = Mutex::new(None);

/// Puts the console on the framebuffer Limine set up for us (if there is one with 32 bits per pixel).
pub fn init() {
    let Some(response) = crate::FRAMEBUFFER.response() else {
        return;
    };
    let Some(fb) = response.framebuffers().first() else {
        return;
    };
    if fb.bpp != 32 {
        return;
    }
    let (width, height, stride) = (fb.width as usize, fb.height as usize, fb.pitch as usize / 4);
    // Safety: Limine mapped this memory for us, and nothing else in the kernel uses it.
    let pixels = unsafe { core::slice::from_raw_parts_mut(fb.address() as *mut u32, stride * height) };
    let font = Font::parse(font::TERMINUS).expect("the built-in font is valid");
    *CONSOLE.lock() = Some(Console::new(pixels, width, height, stride, font));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    let _ = crate::serial::Serial.write_fmt(args);
    if let Some(console) = CONSOLE.lock().as_mut() {
        let _ = console.write_fmt(args);
    }
}

/// Like `std`'s `print!`: to the screen and the serial port.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::console::_print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::console::_print(format_args!("{}\n", format_args!($($arg)*))) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    // A tiny screen in memory: 10 x 3 characters of 8 x 16 pixels, rows 88 pixels apart (stride > width)
    const W: usize = 80;
    const H: usize = 48;
    const STRIDE: usize = 88;

    fn console(pixels: &mut [u32]) -> Console<'_> {
        Console::new(pixels, W, H, STRIDE, Font::parse(font::TERMINUS).unwrap())
    }

    /// Whether the cell at `column`, `row` shows exactly the glyph of `c`.
    fn shows(pixels: &[u32], column: usize, row: usize, c: char) -> bool {
        let font = Font::parse(font::TERMINUS).unwrap();
        font.glyph(c).iter().enumerate().all(|(dy, bits)| {
            (0..8).all(|dx| {
                let pixel = pixels[(row * 16 + dy) * STRIDE + column * 8 + dx];
                pixel
                    == if bits & (0x80 >> dx) != 0 {
                        FOREGROUND
                    } else {
                        BACKGROUND
                    }
            })
        })
    }

    #[test_case]
    fn characters_land_in_their_cells() {
        let mut pixels = [0u32; STRIDE * H];
        let mut c = console(&mut pixels);
        assert_eq!(c.size(), (10, 3));
        write!(c, "Hi\nyou").unwrap();
        assert!(shows(&pixels, 0, 0, 'H') && shows(&pixels, 1, 0, 'i'));
        assert!(shows(&pixels, 0, 1, 'y') && shows(&pixels, 2, 1, 'u'));
        assert!(shows(&pixels, 2, 0, ' ')); // untouched cells stay background
    }

    #[test_case]
    fn long_lines_wrap() {
        let mut pixels = [0u32; STRIDE * H];
        let mut c = console(&mut pixels);
        write!(c, "0123456789AB").unwrap(); // 12 characters on a 10-column screen
        assert!(shows(&pixels, 9, 0, '9'));
        assert!(shows(&pixels, 0, 1, 'A') && shows(&pixels, 1, 1, 'B'));
    }

    #[test_case]
    fn a_full_screen_scrolls_up() {
        let mut pixels = [0u32; STRIDE * H];
        let mut c = console(&mut pixels);
        write!(c, "a\nb\nc\nd").unwrap(); // 4 lines on a 3-row screen: "a" scrolls away
        assert!(shows(&pixels, 0, 0, 'b') && shows(&pixels, 0, 1, 'c') && shows(&pixels, 0, 2, 'd'));
    }

    #[test_case]
    fn backspace_space_backspace_erases() {
        let mut pixels = [0u32; STRIDE * H];
        let mut c = console(&mut pixels);
        write!(c, "ab\x08 \x08c").unwrap();
        assert!(shows(&pixels, 0, 0, 'a') && shows(&pixels, 1, 0, 'c') && shows(&pixels, 2, 0, ' '));
    }

    #[test_case]
    fn the_padding_right_of_the_picture_is_not_drawn_into() {
        let mut pixels = [0u32; STRIDE * H];
        let mut c = console(&mut pixels);
        write!(c, "0123456789").unwrap();
        // pixels 80..88 of each row are beyond the visible width: only `clear` touches them
        assert!((0..H).all(|y| {
            pixels[y * STRIDE + W..(y + 1) * STRIDE]
                .iter()
                .all(|&p| p == BACKGROUND)
        }));
    }
}
