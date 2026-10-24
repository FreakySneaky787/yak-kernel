//! Textkonsole auf dem Framebuffer: Zeichen mit dem PSF-Font zeichnen,
//! Cursor, Zeilenumbruch, Scrolling.
//!
//! Die Konsole kennt nur eine `Surface` (Zeiger + Maße), dadurch lässt sie sich
//! in Tests mit einem normalen Speicherpuffer statt dem echten Bildschirm prüfen.

use crate::font::{FONT_DATA, Font};
use core::fmt;
use spin::Mutex;

/// Eine 32-bit-Pixelfläche (0x00RRGGBB).
#[derive(Clone, Copy)]
pub struct Surface {
    pub base: *mut u32,
    pub width: usize,
    pub height: usize,
    /// Abstand zwischen zwei Zeilen in Pixeln (nicht Bytes!)
    pub stride: usize,
}

unsafe impl Send for Surface {}

impl Surface {
    fn put(&self, x: usize, y: usize, color: u32) {
        debug_assert!(x < self.width && y < self.height);
        unsafe { self.base.add(y * self.stride + x).write_volatile(color) };
    }

    #[cfg(test)]
    fn get(&self, x: usize, y: usize) -> u32 {
        unsafe { self.base.add(y * self.stride + x).read_volatile() }
    }

    fn fill_rows(&self, y_start: usize, y_end: usize, color: u32) {
        for y in y_start..y_end {
            for x in 0..self.width {
                self.put(x, y, color);
            }
        }
    }
}

pub struct Console {
    surface: Surface,
    font: Font,
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
    pub fg: u32,
    pub bg: u32,
}

pub const DEFAULT_FG: u32 = 0x00D0D0D0;
pub const DEFAULT_BG: u32 = 0x00101018;

impl Console {
    pub fn new(surface: Surface) -> Console {
        let font = Font::parse(FONT_DATA).expect("eingebauter Font kaputt");
        let cols = surface.width / font.width;
        let rows = surface.height / font.height;
        let console = Console { surface, font, cols, rows, col: 0, row: 0, fg: DEFAULT_FG, bg: DEFAULT_BG };
        console.surface.fill_rows(0, surface.height, console.bg);
        console
    }

    pub fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.col, self.row)
    }

    pub fn clear(&mut self) {
        self.surface.fill_rows(0, self.surface.height, self.bg);
        self.col = 0;
        self.row = 0;
    }

    pub fn write_char(&mut self, c: char) {
        match c {
            '\n' => self.newline(),
            '\r' => self.col = 0,
            '\t' => {
                let next = (self.col / 4 + 1) * 4;
                while self.col < next.min(self.cols) {
                    self.write_char(' ');
                }
            }
            '\x08' => {
                // Backspace: einen Schritt zurück und Zelle löschen
                if self.col > 0 {
                    self.col -= 1;
                    self.draw_glyph(' ', self.col, self.row);
                }
            }
            c => {
                if self.col >= self.cols {
                    self.newline();
                }
                self.draw_glyph(c, self.col, self.row);
                self.col += 1;
            }
        }
    }

    fn draw_glyph(&self, c: char, col: usize, row: usize) {
        let (fw, fh) = (self.font.width, self.font.height);
        let glyph = self.font.glyph(c);
        let x0 = col * fw;
        let y0 = row * fh;
        for (dy, &bits) in glyph.iter().enumerate().take(fh) {
            for dx in 0..fw {
                let on = bits & (0x80 >> dx) != 0;
                self.surface.put(x0 + dx, y0 + dy, if on { self.fg } else { self.bg });
            }
        }
    }

    fn newline(&mut self) {
        self.col = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.scroll();
        }
    }

    /// Alles eine Textzeile nach oben schieben, unterste Zeile leeren.
    fn scroll(&mut self) {
        let fh = self.font.height;
        let s = self.surface;
        let text_height = self.rows * fh;
        for y in 0..text_height - fh {
            unsafe {
                let dst = s.base.add(y * s.stride);
                let src = s.base.add((y + fh) * s.stride);
                core::ptr::copy_nonoverlapping(src, dst, s.width);
            }
        }
        s.fill_rows(text_height - fh, text_height, self.bg);
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.write_char(c);
        }
        Ok(())
    }
}

pub static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);

/// Konsole auf dem ersten Limine-Framebuffer anlegen (falls vorhanden und 32 bpp).
pub fn init() {
    let Some(resp) = crate::boot::FRAMEBUFFER.response() else { return };
    let Some(fb) = resp.framebuffers().first() else { return };
    if fb.bpp != 32 {
        return;
    }
    let surface = Surface {
        base: fb.address() as *mut u32,
        width: fb.width as usize,
        height: fb.height as usize,
        stride: fb.pitch as usize / 4,
    };
    *CONSOLE.lock() = Some(Console::new(surface));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    crate::arch::x86_64::without_interrupts(|| {
        crate::serial::_print_locked(args);
        if let Some(console) = CONSOLE.lock().as_mut() {
            console.write_fmt(args).ok();
        }
    });
}

/// Ausgabe auf Bildschirm **und** Serial.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::console::_print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::print!("{}\n", format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 80; // 10 Spalten à 8 px
    const H: usize = 64; // 4 Zeilen à 16 px

    fn test_console(buf: &mut [u32; W * H]) -> Console {
        Console::new(Surface { base: buf.as_mut_ptr(), width: W, height: H, stride: W })
    }

    #[test_case]
    fn size_from_font() {
        let mut buf = [0u32; W * H];
        let c = test_console(&mut buf);
        assert_eq!(c.size(), (10, 4));
    }

    #[test_case]
    fn cursor_advances_and_wraps() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        for ch in "abc".chars() {
            c.write_char(ch);
        }
        assert_eq!(c.cursor(), (3, 0));
        for ch in "defghijk".chars() {
            c.write_char(ch); // 11 Zeichen > 10 Spalten → Umbruch
        }
        assert_eq!(c.cursor(), (1, 1));
        c.write_char('\n');
        assert_eq!(c.cursor(), (0, 2));
    }

    #[test_case]
    fn draws_pixels_in_fg_color() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        c.write_char('A');
        let s = c.surface;
        let fg_pixels = (0..16).flat_map(|y| (0..8).map(move |x| (x, y))).filter(|&(x, y)| s.get(x, y) == DEFAULT_FG).count();
        assert!(fg_pixels > 10, "'A' hat nur {} Vordergrund-Pixel", fg_pixels);
    }

    #[test_case]
    fn scrolls_when_full() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        c.write_char('X'); // landet in Zeile 0
        for _ in 0..4 {
            c.write_char('\n'); // 4 Umbrüche bei 4 Zeilen → einmal gescrollt
        }
        assert_eq!(c.cursor(), (0, 3));
        // Zeile 0 ist jetzt die alte Zeile 1 (leer) → keine Vordergrund-Pixel mehr
        let s = c.surface;
        assert!((0..16).all(|y| (0..8).all(|x| s.get(x, y) == DEFAULT_BG)));
    }

    #[test_case]
    fn many_lines_do_not_panic() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        use core::fmt::Write;
        for i in 0..200 {
            writeln!(c, "Zeile {} äöü\tTab", i).unwrap();
        }
        assert_eq!(c.cursor().1, 3);
    }

    #[test_case]
    fn backspace_moves_left() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        c.write_char('a');
        c.write_char('b');
        c.write_char('\x08');
        assert_eq!(c.cursor(), (1, 0));
    }
}
