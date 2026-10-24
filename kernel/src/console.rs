//! Text console on the framebuffer: draws characters with the PSF font,
//! keeps a character grid, shows a cursor, wraps lines and scrolls.
//!
//! It understands a small, terminal-compatible subset of control codes, so
//! the same byte stream looks right on the screen and on a serial terminal:
//! - `\n` newline, `\r` carriage return, `\t` tab
//! - `\x08` (backspace) moves the cursor one cell left **without** erasing
//!   (erasing is done with "\x08 \x08", like on real terminals)
//! - ANSI CSI sequences: `ESC[2J` clear screen, `ESC[H` cursor home,
//!   `ESC[K` clear to end of line, `ESC[<n>m` colours (0, 1, 30-37, 90-97)
//!
//! The console only knows a `Surface` (pointer + dimensions), so it can be
//! tested against a plain memory buffer instead of the real screen.

use crate::font::{FONT_DATA, Font};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use spin::Mutex;

/// A 32-bit pixel surface (0x00RRGGBB).
#[derive(Clone, Copy)]
pub struct Surface {
    pub base: *mut u32,
    pub width: usize,
    pub height: usize,
    /// Distance between two rows in pixels (not bytes!)
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

pub const DEFAULT_FG: u32 = 0x00D0D0D0;
pub const DEFAULT_BG: u32 = 0x00101018;

/// The 16 standard terminal colours (normal 0-7, bright 8-15).
const PALETTE: [u32; 16] = [
    0x00101018, 0x00CC3333, 0x0033AA33, 0x00CCAA33, 0x003366CC, 0x00AA44AA, 0x0033AAAA, 0x00D0D0D0,
    0x00666666, 0x00FF5555, 0x0055FF55, 0x00FFFF55, 0x005599FF, 0x00FF66FF, 0x0055FFFF, 0x00FFFFFF,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: u32,
    pub bg: u32,
}

const BLANK: Cell = Cell { ch: ' ', fg: DEFAULT_FG, bg: DEFAULT_BG };

/// Parser state for escape sequences.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EscState {
    Normal,
    Escape,
    /// Inside `ESC [`, collecting up to 4 numeric parameters.
    Csi { params: [u16; 4], count: usize },
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
    bold: bool,
    cells: Vec<Cell>,
    cursor_visible: bool,
    /// Whether the cursor is currently drawn (inverted) on the screen.
    cursor_drawn: bool,
    esc: EscState,
}

impl Console {
    pub fn new(surface: Surface) -> Console {
        let font = Font::parse(FONT_DATA).expect("built-in font is broken");
        let cols = surface.width / font.width;
        let rows = surface.height / font.height;
        let mut console = Console {
            surface,
            font,
            cols,
            rows,
            col: 0,
            row: 0,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            bold: false,
            cells: vec![BLANK; cols * rows],
            cursor_visible: true,
            cursor_drawn: false,
            esc: EscState::Normal,
        };
        console.clear();
        console
    }

    pub fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.col, self.row)
    }

    /// Character and colours stored at a grid position.
    pub fn cell(&self, col: usize, row: usize) -> Cell {
        self.cells[row * self.cols + col]
    }

    /// Clears the screen and moves the cursor home.
    pub fn clear(&mut self) {
        self.cells.fill(Cell { ch: ' ', fg: self.fg, bg: self.bg });
        self.surface.fill_rows(0, self.surface.height, self.bg);
        self.cursor_drawn = false;
        self.col = 0;
        self.row = 0;
        self.update_cursor();
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.hide_cursor();
        self.cursor_visible = visible;
        self.update_cursor();
    }

    /// Writes a whole string, then redraws the cursor once.
    pub fn write_str_raw(&mut self, s: &str) {
        self.hide_cursor();
        for c in s.chars() {
            self.process(c);
        }
        self.update_cursor();
    }

    pub fn write_char(&mut self, c: char) {
        self.hide_cursor();
        self.process(c);
        self.update_cursor();
    }

    fn process(&mut self, c: char) {
        match self.esc {
            EscState::Normal => self.process_normal(c),
            EscState::Escape => {
                self.esc = if c == '[' { EscState::Csi { params: [0; 4], count: 0 } } else { EscState::Normal };
            }
            EscState::Csi { mut params, mut count } => match c {
                '0'..='9' => {
                    let idx = count.min(3);
                    params[idx] = params[idx].saturating_mul(10).saturating_add(c as u16 - '0' as u16);
                    self.esc = EscState::Csi { params, count };
                }
                ';' => {
                    count = (count + 1).min(3);
                    self.esc = EscState::Csi { params, count };
                }
                _ => {
                    self.esc = EscState::Normal;
                    self.execute_csi(c, &params[..=count.min(3)]);
                }
            },
        }
    }

    fn process_normal(&mut self, c: char) {
        match c {
            '\n' => self.newline(),
            '\r' => self.col = 0,
            '\t' => {
                let next = ((self.col / 4 + 1) * 4).min(self.cols);
                while self.col < next {
                    self.put_char(' ');
                }
            }
            '\x08' => self.col = self.col.saturating_sub(1),
            '\x1b' => self.esc = EscState::Escape,
            c if (c as u32) < 0x20 => {} // ignore other control characters
            c => self.put_char(c),
        }
    }

    fn execute_csi(&mut self, command: char, params: &[u16]) {
        match command {
            'J' if params[0] == 2 => self.clear(),
            'H' => {
                // ESC[row;colH (1-based); without parameters: home
                let row = (params[0].max(1) as usize - 1).min(self.rows - 1);
                let col = (params.get(1).copied().unwrap_or(1).max(1) as usize - 1).min(self.cols - 1);
                self.row = row;
                self.col = col;
            }
            'K' => {
                for col in self.col..self.cols {
                    self.set_cell(col, self.row, ' ');
                }
            }
            'm' => {
                for &p in params {
                    self.apply_sgr(p);
                }
            }
            _ => {} // unsupported sequence: ignore
        }
    }

    /// "Select graphic rendition": colours and bold.
    fn apply_sgr(&mut self, p: u16) {
        match p {
            0 => {
                self.fg = DEFAULT_FG;
                self.bg = DEFAULT_BG;
                self.bold = false;
            }
            1 => self.bold = true,
            22 => self.bold = false,
            30..=37 => self.fg = PALETTE[(p - 30) as usize + if self.bold { 8 } else { 0 }],
            39 => self.fg = DEFAULT_FG,
            40..=47 => self.bg = PALETTE[(p - 40) as usize],
            49 => self.bg = DEFAULT_BG,
            90..=97 => self.fg = PALETTE[(p - 90) as usize + 8],
            _ => {}
        }
    }

    fn put_char(&mut self, c: char) {
        if self.col >= self.cols {
            self.newline();
        }
        self.set_cell(self.col, self.row, c);
        self.col += 1;
    }

    fn set_cell(&mut self, col: usize, row: usize, ch: char) {
        let cell = Cell { ch, fg: self.fg, bg: self.bg };
        self.cells[row * self.cols + col] = cell;
        self.draw_cell(col, row, cell.fg, cell.bg);
    }

    fn draw_cell(&self, col: usize, row: usize, fg: u32, bg: u32) {
        let (fw, fh) = (self.font.width, self.font.height);
        let glyph = self.font.glyph(self.cells[row * self.cols + col].ch);
        let x0 = col * fw;
        let y0 = row * fh;
        for (dy, &bits) in glyph.iter().enumerate().take(fh) {
            for dx in 0..fw {
                let on = bits & (0x80 >> dx) != 0;
                self.surface.put(x0 + dx, y0 + dy, if on { fg } else { bg });
            }
        }
    }

    /// The cursor is drawn by inverting the colours of its cell. It may sit
    /// one past the last column (pending wrap); then it is drawn at the end.
    fn cursor_cell(&self) -> (usize, usize) {
        (self.col.min(self.cols - 1), self.row)
    }

    fn hide_cursor(&mut self) {
        if self.cursor_drawn {
            let (col, row) = self.cursor_cell();
            let cell = self.cell(col, row);
            self.draw_cell(col, row, cell.fg, cell.bg);
            self.cursor_drawn = false;
        }
    }

    fn update_cursor(&mut self) {
        if self.cursor_visible && !self.cursor_drawn {
            let (col, row) = self.cursor_cell();
            let cell = self.cell(col, row);
            self.draw_cell(col, row, cell.bg, cell.fg);
            self.cursor_drawn = true;
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

    /// Moves everything up by one text row and clears the bottom row.
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
        self.cells.copy_within(self.cols.., 0);
        let last_row = (self.rows - 1) * self.cols;
        self.cells[last_row..].fill(Cell { ch: ' ', fg: self.fg, bg: self.bg });
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_str_raw(s);
        Ok(())
    }
}

pub static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);

/// Creates the console on the first Limine framebuffer (if present and 32 bpp).
/// Needs the kernel heap for the character grid.
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

/// Runs `f` with the console, if there is one (interrupt-safe).
pub fn with_console<R>(f: impl FnOnce(&mut Console) -> R) -> Option<R> {
    crate::arch::x86_64::without_interrupts(|| CONSOLE.lock().as_mut().map(f))
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

/// Prints to the screen **and** the serial port.
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
    use core::fmt::Write;

    const W: usize = 80; // 10 columns of 8 px
    const H: usize = 64; // 4 rows of 16 px

    fn test_console(buf: &mut [u32; W * H]) -> Console {
        let mut c = Console::new(Surface { base: buf.as_mut_ptr(), width: W, height: H, stride: W });
        c.set_cursor_visible(false); // most tests check plain pixels
        c
    }

    fn row_text(c: &Console, row: usize) -> alloc::string::String {
        (0..c.cols).map(|col| c.cell(col, row).ch).collect()
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
        c.write_str("abc").unwrap();
        assert_eq!(c.cursor(), (3, 0));
        c.write_str("defghijk").unwrap(); // 11 characters > 10 columns -> wrap
        assert_eq!(c.cursor(), (1, 1));
        c.write_char('\n');
        assert_eq!(c.cursor(), (0, 2));
        assert_eq!(row_text(&c, 0), "abcdefghij");
    }

    #[test_case]
    fn draws_pixels_in_fg_color() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        c.write_char('A');
        let s = c.surface;
        let fg_pixels = (0..16).flat_map(|y| (0..8).map(move |x| (x, y))).filter(|&(x, y)| s.get(x, y) == DEFAULT_FG).count();
        assert!(fg_pixels > 10, "'A' has only {} foreground pixels", fg_pixels);
    }

    #[test_case]
    fn scrolls_when_full() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        c.write_str("X\n1\n2\n3\n4").unwrap();
        assert_eq!(c.cursor(), (1, 3));
        assert_eq!(row_text(&c, 0).trim_end(), "1");
        assert_eq!(row_text(&c, 3).trim_end(), "4");
        // row 0 pixels now show '1', not 'X': compare with a fresh drawing of '1'
        let mut buf2 = [0u32; W * H];
        let mut c2 = test_console(&mut buf2);
        c2.write_char('1');
        let (s1, s2) = (c.surface, c2.surface);
        assert!((0..16).all(|y| (0..8).all(|x| s1.get(x, y) == s2.get(x, y))));
    }

    #[test_case]
    fn many_lines_do_not_panic() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        for i in 0..200 {
            writeln!(c, "line {} äöü\ttab", i).unwrap();
        }
        assert_eq!(c.cursor().1, 3);
    }

    #[test_case]
    fn backspace_moves_left_without_erasing() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        c.write_str("ab\x08").unwrap();
        assert_eq!(c.cursor(), (1, 0));
        assert_eq!(c.cell(1, 0).ch, 'b');
        c.write_str(" \x08").unwrap(); // the terminal way to erase
        assert_eq!(c.cell(1, 0).ch, ' ');
        assert_eq!(c.cursor(), (1, 0));
        c.write_str("\x08\x08\x08").unwrap();
        assert_eq!(c.cursor(), (0, 0)); // stops at column 0
    }

    #[test_case]
    fn ansi_clear_home_and_erase_line() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        c.write_str("hello\nworld").unwrap();
        c.write_str("\x1b[H").unwrap();
        assert_eq!(c.cursor(), (0, 0));
        c.write_str("\x1b[2;3H").unwrap();
        assert_eq!(c.cursor(), (2, 1));
        c.write_str("\x1b[K").unwrap();
        assert_eq!(row_text(&c, 1).trim_end(), "wo");
        c.write_str("\x1b[2J").unwrap();
        assert_eq!(c.cursor(), (0, 0));
        assert_eq!(row_text(&c, 0).trim_end(), "");
    }

    #[test_case]
    fn ansi_colors() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        c.write_str("\x1b[31mR\x1b[1;32mG\x1b[0mN").unwrap();
        assert_eq!(c.cell(0, 0).fg, PALETTE[1]);
        assert_eq!(c.cell(1, 0).fg, PALETTE[10]); // bold green = bright green
        assert_eq!(c.cell(2, 0).fg, DEFAULT_FG);
        assert_eq!(c.cell(2, 0).ch, 'N'); // escape sequences are not printed
    }

    #[test_case]
    fn cursor_is_drawn_inverted() {
        let mut buf = [0u32; W * H];
        let mut c = test_console(&mut buf);
        c.set_cursor_visible(true);
        c.write_str("a").unwrap();
        // cursor at (1,0) over a blank cell: background drawn in fg colour
        assert_eq!(c.surface.get(8, 0), DEFAULT_FG);
        c.write_str("b").unwrap();
        // the old cursor cell now shows 'b' normally: its top-left pixel is background
        assert_eq!(c.surface.get(8, 0), DEFAULT_BG);
        assert_eq!(c.surface.get(16, 0), DEFAULT_FG);
    }
}
