//! /bin/edit: a full-screen text editor. Usage: edit <file> | edit --selftest
//!
//! Arrow keys, Home/End, PageUp/PageDown move; typing inserts; Backspace,
//! Delete remove; Enter splits a line; Tab inserts 4 spaces.
//! Ctrl+S saves, Ctrl+Q quits (twice if there are unsaved changes).
//! Ctrl+C does not kill the editor (`ignore_interrupt`).
//!
//! Screen: all rows but the last show text (lines longer than the screen
//! scroll horizontally), the last row is an inverted status bar. Normally only
//! the cursor line and the status bar are redrawn; scrolling and changes to
//! the line structure redraw everything. Drawing uses VT100 sequences, so it
//! works on the framebuffer console and on a serial terminal.

#![no_std]
#![no_main]

extern crate alloc;

mod buffer;
#[path = "../sh/keys.rs"]
mod keys;

use alloc::format;
use alloc::string::String;
use buffer::Buffer;
use keys::{Key, KeyEvent};
use ulib::fs::{Error, File};
use ulib::{eprintln, io, println, sys};

ulib::entry!(main);

struct Editor {
    buf: Buffer,
    path: String,
    /// First visible line and column.
    top: usize,
    left: usize,
    /// Text area size (the status bar is below it).
    rows: usize,
    cols: usize,
    status: String,
    /// Ctrl+Q was pressed once with unsaved changes.
    quit_armed: bool,
}

fn out(text: &str) {
    sys::write(sys::STDOUT, text.as_bytes());
}

impl Editor {
    fn scroll(&mut self) -> bool {
        let (top, left) = (self.top, self.left);
        if self.buf.row < self.top {
            self.top = self.buf.row;
        } else if self.buf.row >= self.top + self.rows {
            self.top = self.buf.row + 1 - self.rows;
        }
        if self.buf.col < self.left {
            self.left = self.buf.col;
        } else if self.buf.col >= self.left + self.cols {
            self.left = self.buf.col + 1 - self.cols;
        }
        (top, left) != (self.top, self.left)
    }

    fn line_text(&self, index: usize, screen: &mut String) {
        match self.buf.lines.get(index) {
            Some(line) => screen.extend(line.iter().skip(self.left).take(self.cols)),
            None => screen.push_str("\x1b[94m~\x1b[0m"),
        }
        screen.push_str("\x1b[K");
    }

    fn draw(&mut self, full: bool) {
        let mut screen = String::new();
        if full {
            for r in 0..self.rows {
                screen.push_str(&format!("\x1b[{};1H", r + 1));
                self.line_text(self.top + r, &mut screen);
            }
        } else {
            screen.push_str(&format!("\x1b[{};1H", self.buf.row - self.top + 1));
            self.line_text(self.buf.row, &mut screen);
        }
        // status bar (inverted), then the cursor
        let info = format!(
            " {}{} | line {}/{}, column {} | ^S save  ^Q quit ",
            self.path,
            if self.buf.dirty { " [modified]" } else { "" },
            self.buf.row + 1,
            self.buf.lines.len(),
            self.buf.col + 1
        );
        let mut bar: String = info.chars().chain(self.status.chars()).take(self.cols).collect();
        while bar.chars().count() < self.cols {
            bar.push(' ');
        }
        screen.push_str(&format!("\x1b[{};1H\x1b[7m{}\x1b[0m", self.rows + 1, bar));
        screen.push_str(&format!(
            "\x1b[{};{}H",
            self.buf.row - self.top + 1,
            self.buf.col - self.left + 1
        ));
        out(&screen);
    }

    fn save(&mut self) {
        let text = self.buf.to_text();
        let result = File::create(&self.path).and_then(|mut f| f.write(text.as_bytes()));
        self.status = match result {
            Ok(n) => {
                self.buf.dirty = false;
                format!("saved {} bytes", n)
            }
            Err(e) => format!("cannot save: {}", e),
        };
    }

    /// Handles one key; returns false when the editor should quit.
    fn key(&mut self, ev: KeyEvent) -> bool {
        let lines_before = self.buf.lines.len();
        self.status.clear();
        let quit_armed = core::mem::take(&mut self.quit_armed);
        match ev.key {
            Key::Char('s') if ev.ctrl => self.save(),
            Key::Char('q') if ev.ctrl => {
                if !self.buf.dirty || quit_armed {
                    return false;
                }
                self.quit_armed = true;
                self.status = String::from("unsaved changes - press ^Q again to quit without saving");
            }
            Key::Char('c') if ev.ctrl => self.status = String::from("use ^Q to quit"),
            Key::Char(_) if ev.ctrl => {}
            Key::Char(c) => self.buf.insert(c),
            Key::Tab => (0..4).for_each(|_| self.buf.insert(' ')),
            Key::Enter => self.buf.newline(),
            Key::Backspace => self.buf.backspace(),
            Key::Delete => self.buf.delete(),
            Key::Left => self.buf.left(),
            Key::Right => self.buf.right(),
            Key::Up => self.buf.vertical(-1),
            Key::Down => self.buf.vertical(1),
            Key::PageUp => self.buf.vertical(-(self.rows as isize)),
            Key::PageDown => self.buf.vertical(self.rows as isize),
            Key::Home => self.buf.home(),
            Key::End => self.buf.end(),
            _ => {}
        }
        // Moving to another line leaves both lines unchanged; only scrolling
        // and inserted/removed lines need the whole screen.
        let scrolled = self.scroll();
        self.draw(scrolled || self.buf.lines.len() != lines_before);
        true
    }
}

fn load(path: &str) -> Result<(Buffer, &'static str), Error> {
    match ulib::fs::stat(path) {
        Ok(st) if st.kind == sys::KIND_DIR => Err(Error(21)),
        Ok(_) => {
            let data = io::read_input(Some(path))?;
            Ok((Buffer::from_text(&String::from_utf8_lossy(&data)), ""))
        }
        Err(Error(2)) => Ok((Buffer::default(), "new file")),
        Err(e) => Err(e),
    }
}

fn main(mut args: ulib::Args) -> i32 {
    let Some(path) = args.next() else {
        eprintln!("usage: edit <file> | edit --selftest");
        return 2;
    };
    if path == "--selftest" {
        let failed = buffer::selftest();
        for f in &failed {
            println!("FAIL: {}", f);
        }
        if failed.is_empty() {
            println!("edit selftest: all checks passed");
        }
        return failed.len() as i32;
    }
    let (buf, status) = match load(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("edit: {}: {}", path, e);
            return 1;
        }
    };
    let size = sys::termsize();
    let (screen_rows, screen_cols) = ((size >> 32) as usize, (size & 0xFFFF_FFFF) as usize);
    let mut ed = Editor {
        buf,
        path: String::from(path),
        top: 0,
        left: 0,
        rows: screen_rows.max(3) - 1,
        cols: screen_cols.max(20),
        status: String::from(status),
        quit_armed: false,
    };
    sys::ignore_interrupt(true);
    out("\x1b[2J");
    ed.draw(true);
    let mut decoder = keys::Decoder::new();
    'keys: loop {
        let mut bytes = [0u8; 16];
        let n = sys::read(sys::STDIN, &mut bytes);
        if n <= 0 {
            break; // end of input (not a terminal)
        }
        for &b in &bytes[..n as usize] {
            if let Some(ev) = decoder.feed(b) {
                if !ed.key(ev) {
                    break 'keys;
                }
            }
        }
    }
    out("\x1b[0m\x1b[2J\x1b[H");
    0
}
