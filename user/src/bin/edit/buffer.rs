//! The text being edited: lines of characters plus a cursor. Pure logic,
//! checked by `edit --selftest`.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub struct Buffer {
    pub lines: Vec<Vec<char>>,
    /// Cursor: line index and character index within the line.
    pub row: usize,
    pub col: usize,
    /// Column the cursor tries to keep when moving up/down.
    want_col: usize,
    pub dirty: bool,
}

impl Buffer {
    /// Splits `text` into lines; a final newline does not start an extra line.
    pub fn from_text(text: &str) -> Buffer {
        let mut lines: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
        if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        Buffer {
            lines,
            row: 0,
            col: 0,
            want_col: 0,
            dirty: false,
        }
    }

    /// The text with a newline after every line (empty buffer -> empty text).
    pub fn to_text(&self) -> String {
        if self.lines.len() == 1 && self.lines[0].is_empty() {
            return String::new();
        }
        let mut text = String::new();
        for line in &self.lines {
            text.extend(line.iter());
            text.push('\n');
        }
        text
    }

    fn line(&self) -> &Vec<char> {
        &self.lines[self.row]
    }

    pub fn insert(&mut self, c: char) {
        let (row, col) = (self.row, self.col);
        self.lines[row].insert(col, c);
        self.col += 1;
        self.want_col = self.col;
        self.dirty = true;
    }

    /// Enter: splits the line at the cursor.
    pub fn newline(&mut self) {
        let rest = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
        self.want_col = 0;
        self.dirty = true;
    }

    /// Deletes the character before the cursor (joins lines at column 0).
    pub fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let (row, col) = (self.row, self.col);
            self.lines[row].remove(col);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].extend(line);
        } else {
            return;
        }
        self.want_col = self.col;
        self.dirty = true;
    }

    /// Deletes the character under the cursor (joins with the next line at the end).
    pub fn delete(&mut self) {
        if self.col < self.line().len() {
            let (row, col) = (self.row, self.col);
            self.lines[row].remove(col);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].extend(next);
        } else {
            return;
        }
        self.dirty = true;
    }

    pub fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line().len();
        }
        self.want_col = self.col;
    }

    pub fn right(&mut self) {
        if self.col < self.line().len() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
        self.want_col = self.col;
    }

    /// Moves `delta` lines (negative = up), keeping the wanted column if possible.
    pub fn vertical(&mut self, delta: isize) {
        let target = (self.row as isize + delta).clamp(0, self.lines.len() as isize - 1);
        self.row = target as usize;
        self.col = self.want_col.min(self.line().len());
    }

    pub fn home(&mut self) {
        self.col = 0;
        self.want_col = 0;
    }

    pub fn end(&mut self) {
        self.col = self.line().len();
        self.want_col = self.col;
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
            want_col: 0,
            dirty: false,
        }
    }
}

/// The checks of `edit --selftest`; returns the descriptions of failed ones.
pub fn selftest() -> Vec<&'static str> {
    let mut failed = Vec::new();
    let mut check = |ok: bool, what: &'static str| {
        if !ok {
            failed.push(what);
        }
    };
    let b = Buffer::from_text("one\ntwo\n");
    check(
        b.lines.len() == 2 && b.to_text() == "one\ntwo\n",
        "load/save round trip",
    );
    check(Buffer::from_text("").to_text().is_empty(), "empty file stays empty");
    check(
        Buffer::from_text("no newline").to_text() == "no newline\n",
        "a final newline is added",
    );

    let mut b = Buffer::default();
    for c in "helo".chars() {
        b.insert(c);
    }
    b.left();
    b.insert('l');
    check(
        b.to_text() == "hello\n" && b.col == 4 && b.dirty,
        "insert in the middle",
    );
    b.end();
    b.newline();
    for c in "world".chars() {
        b.insert(c);
    }
    check(
        b.to_text() == "hello\nworld\n" && (b.row, b.col) == (1, 5),
        "newline at the end",
    );
    b.home();
    b.backspace();
    check(
        b.to_text() == "helloworld\n" && (b.row, b.col) == (0, 5),
        "backspace joins lines",
    );
    b.newline();
    check(
        b.to_text() == "hello\nworld\n" && (b.row, b.col) == (1, 0),
        "newline splits a line",
    );
    b.vertical(-1);
    b.end();
    b.delete();
    check(
        b.to_text() == "helloworld\n",
        "delete at the end joins with the next line",
    );
    b.home();
    b.backspace(); // nothing before the start
    check(
        b.to_text() == "helloworld\n" && (b.row, b.col) == (0, 0),
        "backspace at the start does nothing",
    );

    let mut b = Buffer::from_text("a long line\nab\nanother long line\n");
    b.end(); // col 11
    b.vertical(1);
    check(b.col == 2, "moving down clamps to a shorter line");
    b.vertical(1);
    check(b.col == 11, "and returns to the wanted column");
    b.vertical(-10);
    check(b.row == 0, "moving up stops at the first line");
    b.vertical(10);
    check(b.row == 2, "moving down stops at the last line");
    b.right();
    b.right();
    check((b.row, b.col) == (2, 13), "right moves within the line");
    b.end();
    b.right();
    check((b.row, b.col) == (2, 17), "right at the end of the last line stays");
    b.home();
    b.left();
    check(
        (b.row, b.col) == (1, 2),
        "left at column 0 goes to the end of the previous line",
    );
    let mut u = Buffer::default();
    u.insert('ä');
    u.insert('ß');
    check(
        u.to_text() == "äß\n" && u.col == 2,
        "non-ASCII characters are one column each",
    );
    failed
}
