//! Interactive line editor: cursor movement, history and tab completion.
//!
//! The editor only produces a terminal-compatible byte stream (`\x08` moves
//! left, characters overwrite, `ESC[nA`/`B`/`C` move up, down and right), so it
//! renders the same on our framebuffer console and on a serial terminal. It
//! is pure logic and fully unit-tested with a `String` as output.
//!
//! A line may be longer than one screen row (M57): the editor knows the
//! terminal width and the prompt's width and computes the row and column of
//! every buffer position. Two terminal habits are avoided on purpose: a
//! backspace does not go back to the previous row, and after a character in
//! the last column the cursor waits *there* until the next character ("pending
//! wrap"). So moves across rows use `ESC[nA`/`ESC[nB` + `\r` + `ESC[nC`, and
//! output that ends exactly at the end of a row is followed by `" \r"`, which
//! puts the cursor at the start of the next row on every terminal.

// `super::keys`: the kernel shell and /bin/sh both compile this file
use super::keys::{Key, KeyEvent};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// Maximum line length in characters (the kernel's argument limit is 4096 bytes).
pub const MAX_LINE: usize = 4096;
/// Terminal width until `set_width` says otherwise.
pub const DEFAULT_WIDTH: usize = 80;
const MAX_HISTORY: usize = 50;

#[derive(Debug, PartialEq, Eq)]
pub enum EditResult {
    /// Line not finished yet.
    Pending,
    /// Enter was pressed.
    Line(String),
    /// Ctrl+C: line discarded.
    Cancelled,
}

pub struct LineEditor {
    prompt: String,
    buf: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Index into `history` while browsing with Up/Down.
    history_pos: Option<usize>,
    /// The unfinished line, restored when browsing past the newest entry.
    saved: Vec<char>,
    /// Terminal columns, and the columns the prompt takes (escape sequences do not count).
    width: usize,
    prompt_width: usize,
}

impl LineEditor {
    pub fn new() -> LineEditor {
        LineEditor {
            prompt: String::new(),
            buf: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            history_pos: None,
            saved: Vec::new(),
            width: DEFAULT_WIDTH,
            prompt_width: 0,
        }
    }

    /// The terminal's width in columns.
    pub fn set_width(&mut self, columns: usize) {
        self.width = columns.max(2);
    }

    /// Starts a new line: prints the prompt and resets the buffer.
    pub fn start(&mut self, prompt: &str, out: &mut dyn Write) {
        self.prompt = String::from(prompt);
        self.prompt_width = visible_width(prompt);
        self.buf.clear();
        self.cursor = 0;
        self.history_pos = None;
        out.write_str(prompt).ok();
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn buffer(&self) -> String {
        self.buf.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Processes one key. `complete` returns all candidates for a word prefix.
    pub fn handle(
        &mut self,
        event: KeyEvent,
        out: &mut dyn Write,
        complete: &dyn Fn(&str) -> Vec<String>,
    ) -> EditResult {
        if event.ctrl {
            if let Key::Char(c) = event.key {
                return self.handle_ctrl(c, out);
            }
        }
        match event.key {
            Key::Char(c) => self.insert(c, out),
            Key::Enter => {
                self.leave_line(out);
                out.write_str("\n").ok();
                let line: String = self.buf.iter().collect();
                if !line.trim().is_empty() && self.history.last() != Some(&line) {
                    if self.history.len() == MAX_HISTORY {
                        self.history.remove(0);
                    }
                    self.history.push(line.clone());
                }
                return EditResult::Line(line);
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.move_to(out, self.cursor - 1);
                    self.buf.remove(self.cursor);
                    self.refresh(out, self.cursor, 1);
                }
            }
            Key::Delete => {
                if self.cursor < self.buf.len() {
                    self.buf.remove(self.cursor);
                    self.refresh(out, self.cursor, 1);
                }
            }
            Key::Left => {
                if self.cursor > 0 {
                    self.move_to(out, self.cursor - 1);
                }
            }
            Key::Right => {
                if self.cursor < self.buf.len() {
                    self.move_to(out, self.cursor + 1);
                }
            }
            Key::Home => self.move_home(out),
            Key::End => self.move_end(out),
            Key::Up => self.history_prev(out),
            Key::Down => self.history_next(out),
            Key::Tab => self.complete(out, complete),
            _ => {}
        }
        EditResult::Pending
    }

    fn handle_ctrl(&mut self, c: char, out: &mut dyn Write) -> EditResult {
        match c {
            'c' => {
                self.leave_line(out);
                out.write_str("^C\n").ok();
                self.buf.clear();
                self.cursor = 0;
                return EditResult::Cancelled;
            }
            'a' => self.move_home(out),
            'e' => self.move_end(out),
            'u' => {
                // delete everything before the cursor
                let removed = self.cursor;
                self.move_to(out, 0);
                self.buf.drain(..removed);
                self.refresh(out, 0, removed);
            }
            'l' => {
                out.write_str("\x1b[2J\x1b[H").ok();
                self.redraw_line(out);
            }
            _ => {}
        }
        EditResult::Pending
    }

    fn insert(&mut self, c: char, out: &mut dyn Write) {
        if self.buf.len() >= MAX_LINE || (c as u32) < 0x20 {
            return;
        }
        let at = self.cursor;
        self.buf.insert(at, c);
        self.cursor += 1;
        self.refresh(out, at, 0);
    }

    /// Row and column (relative to the start of the prompt) of buffer position `i`.
    fn place(&self, i: usize) -> (usize, usize) {
        let p = self.prompt_width + i;
        (p / self.width, p % self.width)
    }

    /// Moves the terminal cursor from buffer position `from` to `to`.
    fn move_between(&self, out: &mut dyn Write, from: usize, to: usize) {
        let ((r0, c0), (r1, c1)) = (self.place(from), self.place(to));
        if r0 == r1 {
            if c1 < c0 {
                for _ in c1..c0 {
                    out.write_char('\x08').ok();
                }
            } else if c1 > c0 {
                write!(out, "\x1b[{}C", c1 - c0).ok();
            }
            return;
        }
        if r1 < r0 {
            write!(out, "\x1b[{}A\r", r0 - r1).ok();
        } else {
            write!(out, "\x1b[{}B\r", r1 - r0).ok();
        }
        if c1 > 0 {
            write!(out, "\x1b[{}C", c1).ok();
        }
    }

    fn move_to(&mut self, out: &mut dyn Write, to: usize) {
        self.move_between(out, self.cursor, to);
        self.cursor = to;
    }

    /// The terminal cursor is at buffer position `end` after writing up to
    /// there: if that is the start of a row, make sure it really is there
    /// (and not waiting at the end of the previous row).
    fn settle(&self, out: &mut dyn Write, end: usize) {
        if end > 0 && self.place(end).1 == 0 {
            out.write_str(" \r").ok();
        }
    }

    /// Rewrites the buffer from position `from` (where the terminal cursor
    /// is), blanks `erased` cells after its end (the line got shorter) and
    /// puts the cursor back to `self.cursor`.
    fn refresh(&mut self, out: &mut dyn Write, from: usize, erased: usize) {
        for &c in &self.buf[from..] {
            out.write_char(c).ok();
        }
        for _ in 0..erased {
            out.write_char(' ').ok();
        }
        let end = self.buf.len() + erased;
        self.settle(out, end);
        self.move_between(out, end, self.cursor);
    }

    /// Before output below the line (Enter, Ctrl+C, a completion list): the
    /// cursor goes to the line's last row.
    fn leave_line(&mut self, out: &mut dyn Write) {
        if self.place(self.cursor).0 != self.place(self.buf.len()).0 {
            self.move_to(out, self.buf.len());
        }
    }

    fn move_home(&mut self, out: &mut dyn Write) {
        self.move_to(out, 0);
    }

    fn move_end(&mut self, out: &mut dyn Write) {
        self.move_to(out, self.buf.len());
    }

    /// Replaces the whole line on screen with `new`; the cursor goes to its end.
    fn replace_line(&mut self, new: Vec<char>, out: &mut dyn Write) {
        let old_len = self.buf.len();
        self.move_home(out);
        self.buf = new;
        self.cursor = self.buf.len();
        let erased = old_len.saturating_sub(self.buf.len());
        self.refresh(out, 0, erased);
    }

    /// Prints prompt + buffer again (after clearing the screen or listing completions).
    fn redraw_line(&mut self, out: &mut dyn Write) {
        out.write_str(&self.prompt).ok();
        for &c in &self.buf {
            out.write_char(c).ok();
        }
        self.settle(out, self.buf.len());
        self.move_between(out, self.buf.len(), self.cursor);
    }

    fn history_prev(&mut self, out: &mut dyn Write) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            None => {
                self.saved = self.buf.clone();
                self.history.len() - 1
            }
            Some(0) => return,
            Some(p) => p - 1,
        };
        self.history_pos = Some(pos);
        let entry = self.history[pos].chars().collect();
        self.replace_line(entry, out);
    }

    fn history_next(&mut self, out: &mut dyn Write) {
        let Some(pos) = self.history_pos else { return };
        if pos + 1 < self.history.len() {
            self.history_pos = Some(pos + 1);
            let entry = self.history[pos + 1].chars().collect();
            self.replace_line(entry, out);
        } else {
            self.history_pos = None;
            let saved = core::mem::take(&mut self.saved);
            self.replace_line(saved, out);
        }
    }

    /// Completes the first word (the command name) when the cursor is in it.
    fn complete(&mut self, out: &mut dyn Write, complete: &dyn Fn(&str) -> Vec<String>) {
        let before: String = self.buf[..self.cursor].iter().collect();
        if before.contains(' ') {
            return; // only command names are completed
        }
        let candidates = complete(&before);
        match candidates.len() {
            0 => {}
            1 => {
                let rest: Vec<char> = candidates[0].chars().skip(before.chars().count()).collect();
                for c in rest {
                    self.insert(c, out);
                }
                if self.cursor == self.buf.len() {
                    self.insert(' ', out);
                }
            }
            _ => {
                // extend to the longest common prefix, then list all candidates
                let common = longest_common_prefix(&candidates);
                let rest: Vec<char> = common.chars().skip(before.chars().count()).collect();
                if !rest.is_empty() {
                    for c in rest {
                        self.insert(c, out);
                    }
                } else {
                    self.leave_line(out);
                    out.write_str("\n").ok();
                    for (i, c) in candidates.iter().enumerate() {
                        if i > 0 {
                            out.write_str("  ").ok();
                        }
                        out.write_str(c).ok();
                    }
                    out.write_str("\n").ok();
                    self.redraw_line(out);
                }
            }
        }
    }
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

/// Columns `text` takes on the terminal: escape sequences (`ESC[...x`) take none.
fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if (c as u32) >= 0x20 {
            width += 1;
        }
    }
    width
}

fn longest_common_prefix(words: &[String]) -> String {
    let first = &words[0];
    let mut len = first.len();
    for w in &words[1..] {
        len = len.min(first.bytes().zip(w.bytes()).take_while(|(a, b)| a == b).count());
    }
    while !first.is_char_boundary(len) {
        len -= 1;
    }
    String::from(&first[..len])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn key(k: Key) -> KeyEvent {
        KeyEvent {
            key: k,
            ctrl: false,
            alt: false,
        }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent {
            key: Key::Char(c),
            ctrl: true,
            alt: false,
        }
    }

    fn commands(prefix: &str) -> Vec<String> {
        ["help", "history", "echo", "clear"]
            .iter()
            .filter(|c| c.starts_with(prefix))
            .map(|c| String::from(*c))
            .collect()
    }

    /// Feeds a string as key presses; returns the final result.
    fn type_str(ed: &mut LineEditor, s: &str, out: &mut String) -> EditResult {
        let mut last = EditResult::Pending;
        for c in s.chars() {
            let k = if c == '\n' { key(Key::Enter) } else { key(Key::Char(c)) };
            last = ed.handle(k, out, &commands);
        }
        last
    }

    /// Simulates a terminal: applies the editor's output to a line buffer.
    fn render(out: &str) -> String {
        let mut line: Vec<char> = Vec::new();
        let mut col = 0;
        let mut chars = out.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\x08' => col = col.max(1) - 1,
                '\r' => col = 0,
                '\n' => {
                    line.clear();
                    col = 0;
                }
                '\x1b' => {
                    // CSI sequence: only "n columns right" matters on one row
                    let mut n = 0;
                    for c in chars.by_ref() {
                        if let Some(d) = c.to_digit(10) {
                            n = n * 10 + d as usize;
                        } else if c.is_ascii_alphabetic() {
                            if c == 'C' {
                                col += n.max(1);
                            }
                            break;
                        }
                    }
                }
                c => {
                    if col < line.len() {
                        line[col] = c;
                    } else {
                        line.push(c);
                    }
                    col += 1;
                }
            }
        }
        line.iter().collect::<String>().trim_end().into()
    }

    #[test_case]
    fn typing_and_enter() {
        let mut ed = LineEditor::new();
        let mut out = String::new();
        ed.start("> ", &mut out);
        assert_eq!(
            type_str(&mut ed, "echo hi\n", &mut out),
            EditResult::Line("echo hi".into())
        );
        assert_eq!(out, "> echo hi\n");
    }

    #[test_case]
    fn insert_in_the_middle() {
        let mut ed = LineEditor::new();
        let mut out = String::new();
        ed.start("> ", &mut out);
        type_str(&mut ed, "ac", &mut out);
        ed.handle(key(Key::Left), &mut out, &commands);
        type_str(&mut ed, "b", &mut out);
        assert_eq!(ed.buffer(), "abc");
        assert_eq!(ed.cursor(), 2);
        assert_eq!(render(&out), "> abc");
    }

    #[test_case]
    fn backspace_and_delete() {
        let mut ed = LineEditor::new();
        let mut out = String::new();
        ed.start("> ", &mut out);
        type_str(&mut ed, "abcd", &mut out);
        ed.handle(key(Key::Left), &mut out, &commands);
        ed.handle(key(Key::Left), &mut out, &commands);
        ed.handle(key(Key::Backspace), &mut out, &commands); // removes 'b'
        ed.handle(key(Key::Delete), &mut out, &commands); // removes 'c'
        assert_eq!(ed.buffer(), "ad");
        assert_eq!(render(&out), "> ad");
    }

    #[test_case]
    fn home_end_and_ctrl_u() {
        let mut ed = LineEditor::new();
        let mut out = String::new();
        ed.start("> ", &mut out);
        type_str(&mut ed, "world", &mut out);
        ed.handle(key(Key::Home), &mut out, &commands);
        type_str(&mut ed, "hello ", &mut out);
        ed.handle(key(Key::End), &mut out, &commands);
        type_str(&mut ed, "!", &mut out);
        assert_eq!(ed.buffer(), "hello world!");
        assert_eq!(render(&out), "> hello world!");
        ed.handle(key(Key::Left), &mut out, &commands);
        ed.handle(ctrl('u'), &mut out, &commands);
        assert_eq!(ed.buffer(), "!");
        assert_eq!(render(&out), "> !");
    }

    #[test_case]
    fn history_navigation() {
        let mut ed = LineEditor::new();
        let mut out = String::new();
        for cmd in ["first\n", "second\n", "second\n"] {
            ed.start("> ", &mut out);
            type_str(&mut ed, cmd, &mut out);
        }
        assert_eq!(ed.history(), ["first", "second"]); // duplicate not stored twice
        out.clear();
        ed.start("> ", &mut out);
        type_str(&mut ed, "draft", &mut out);
        ed.handle(key(Key::Up), &mut out, &commands);
        assert_eq!(ed.buffer(), "second");
        ed.handle(key(Key::Up), &mut out, &commands);
        assert_eq!(ed.buffer(), "first");
        ed.handle(key(Key::Up), &mut out, &commands); // stays at oldest
        assert_eq!(render(&out), "> first");
        ed.handle(key(Key::Down), &mut out, &commands);
        ed.handle(key(Key::Down), &mut out, &commands);
        assert_eq!(ed.buffer(), "draft");
        assert_eq!(render(&out), "> draft");
    }

    #[test_case]
    fn tab_completion() {
        let mut ed = LineEditor::new();
        let mut out = String::new();
        ed.start("> ", &mut out);
        type_str(&mut ed, "ec", &mut out);
        ed.handle(key(Key::Tab), &mut out, &commands);
        assert_eq!(ed.buffer(), "echo ");

        let mut ed = LineEditor::new();
        let mut out = String::new();
        ed.start("> ", &mut out);
        type_str(&mut ed, "h", &mut out);
        ed.handle(key(Key::Tab), &mut out, &commands); // help/history: nothing in common beyond "h"
        assert!(out.contains("help  history"));
        assert_eq!(ed.buffer(), "h");
        assert_eq!(render(&out), "> h");
    }

    #[test_case]
    fn ctrl_c_cancels() {
        let mut ed = LineEditor::new();
        let mut out = String::new();
        ed.start("> ", &mut out);
        type_str(&mut ed, "oops", &mut out);
        assert_eq!(ed.handle(ctrl('c'), &mut out, &commands), EditResult::Cancelled);
        assert!(out.ends_with("^C\n"));
        assert!(ed.history().is_empty());
    }

    /// A terminal of `width` columns that understands what the editor sends.
    /// Both kinds of "pending wrap" are modelled: after a character in the
    /// last column the cursor waits there; a backspace then moves to the last
    /// column (`xterm = false`, like our console) or one before it (`xterm`).
    struct Term {
        width: usize,
        xterm: bool,
        rows: Vec<Vec<char>>,
        row: usize,
        col: usize,
    }

    impl Term {
        fn new(width: usize, xterm: bool) -> Term {
            Term { width, xterm, rows: vec![Vec::new()], row: 0, col: 0 }
        }

        fn feed(&mut self, out: &str) {
            let mut chars = out.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\r' => self.col = 0,
                    '\n' => self.down(1, true),
                    '\x08' => {
                        let pending = self.col == self.width;
                        self.col = self.col.saturating_sub(if pending && self.xterm { 2 } else { 1 });
                    }
                    '\x1b' => {
                        assert_eq!(chars.next(), Some('['));
                        let mut n = 0;
                        let cmd = loop {
                            let c = chars.next().unwrap();
                            match c.to_digit(10) {
                                Some(d) => n = n * 10 + d as usize,
                                None => break c,
                            }
                        };
                        if cmd == 'm' {
                            continue; // colours
                        }
                        let n = n.max(1);
                        self.col = self.col.min(self.width - 1);
                        match cmd {
                            'A' => self.row -= n,
                            'B' => self.down(n, false),
                            'C' => self.col = (self.col + n).min(self.width - 1),
                            other => panic!("unexpected CSI {}", other),
                        }
                    }
                    c => {
                        if self.col == self.width {
                            self.down(1, true); // the pending wrap happens now
                        }
                        let line = &mut self.rows[self.row];
                        while line.len() <= self.col {
                            line.push(' ');
                        }
                        line[self.col] = c;
                        self.col += 1;
                    }
                }
            }
        }

        fn down(&mut self, n: usize, to_start: bool) {
            self.row += n;
            while self.rows.len() <= self.row {
                self.rows.push(Vec::new());
            }
            if to_start {
                self.col = 0;
            }
        }

        fn lines(&self) -> Vec<String> {
            let mut lines: Vec<String> = self.rows.iter().map(|r| r.iter().collect::<String>().trim_end().into()).collect();
            while lines.last().is_some_and(|l| l.is_empty()) {
                lines.pop();
            }
            lines
        }

        /// The cursor as (row, column), with a pending wrap shown as the next row's start.
        fn cursor(&self) -> (usize, usize) {
            if self.col == self.width { (self.row + 1, 0) } else { (self.row, self.col) }
        }
    }

    /// Runs `keys` in an editor of width 10 (prompt "> ") on both kinds of
    /// terminal and checks they show the same.
    fn on_both_terminals(keys: &[KeyEvent]) -> (LineEditor, Vec<String>, (usize, usize)) {
        let mut results = Vec::new();
        for xterm in [false, true] {
            let mut ed = LineEditor::new();
            ed.set_width(10);
            let mut out = String::new();
            ed.start("\x1b[92m>\x1b[0m ", &mut out); // colours do not take columns
            for &k in keys {
                ed.handle(k, &mut out, &commands);
            }
            let mut term = Term::new(10, xterm);
            term.feed(&out);
            results.push((ed, term.lines(), term.cursor()));
        }
        let second = results.pop().unwrap();
        let first = results.pop().unwrap();
        assert_eq!((&first.1, first.2), (&second.1, second.2), "the terminals disagree");
        first
    }

    fn chars(s: &str) -> Vec<KeyEvent> {
        s.chars().map(|c| key(Key::Char(c))).collect()
    }

    #[test_case]
    fn long_lines_wrap_over_rows() {
        let (ed, lines, cursor) = on_both_terminals(&chars("abcdefghijklmnopqrstuvwxy"));
        assert_eq!(ed.buffer().len(), 25);
        assert_eq!(lines, ["> abcdefgh", "ijklmnopqr", "stuvwxy"]);
        assert_eq!(cursor, (2, 7));
        // exactly at the end of a row: the cursor is at the start of the next one
        let (_, lines, cursor) = on_both_terminals(&chars("abcdefgh"));
        assert_eq!((lines, cursor), (vec![String::from("> abcdefgh")], (1, 0)));
    }

    #[test_case]
    fn editing_across_rows() {
        let mut keys = chars("abcdefghijklmnopqrstuvwxy");
        keys.push(key(Key::Home));
        keys.extend(chars("XY"));
        let (ed, lines, cursor) = on_both_terminals(&keys);
        assert_eq!(ed.buffer(), "XYabcdefghijklmnopqrstuvwxy");
        assert_eq!(lines, ["> XYabcdef", "ghijklmnop", "qrstuvwxy"]);
        assert_eq!(cursor, (0, 4));
        // left over a row boundary, then delete backwards across it
        let mut keys = chars("abcdefghij");
        keys.extend([key(Key::Left), key(Key::Left), key(Key::Left)]);
        keys.extend([key(Key::Backspace), key(Key::Backspace)]);
        let (ed, lines, cursor) = on_both_terminals(&keys);
        assert_eq!(ed.buffer(), "abcdehij");
        assert_eq!(lines, ["> abcdehij"]);
        assert_eq!(cursor, (0, 7));
        // End and Right from the middle of the first row
        let mut keys = chars("abcdefghijkl");
        keys.extend([key(Key::Home), key(Key::Right), key(Key::End)]);
        keys.extend(chars("!"));
        let (ed, lines, cursor) = on_both_terminals(&keys);
        assert_eq!(ed.buffer(), "abcdefghijkl!");
        assert_eq!(lines, ["> abcdefgh", "ijkl!"]);
        assert_eq!(cursor, (1, 5));
    }

    #[test_case]
    fn a_shorter_line_clears_the_rows_below() {
        let mut keys = chars("abcdefghijklmnopqrstuvwxy");
        keys.push(key(Key::Home));
        keys.extend((0..15).map(|_| key(Key::Delete)));
        let (ed, lines, cursor) = on_both_terminals(&keys);
        assert_eq!(ed.buffer(), "pqrstuvwxy");
        assert_eq!(lines, ["> pqrstuvw", "xy"]);
        assert_eq!(cursor, (0, 2));
        let mut keys = chars("abcdefghijklmnopqrstuvwxy");
        keys.push(ctrl('u'));
        let (ed, lines, _) = on_both_terminals(&keys);
        assert_eq!((ed.buffer(), lines), (String::new(), vec![String::from(">")]));
    }

    #[test_case]
    fn enter_leaves_a_long_line_below_it() {
        let mut keys = chars("abcdefghijklmnopqrstuvwxy");
        keys.push(key(Key::Home));
        keys.push(key(Key::Enter));
        let (_, lines, cursor) = on_both_terminals(&keys);
        assert_eq!(lines, ["> abcdefgh", "ijklmnopqr", "stuvwxy"]);
        assert_eq!(cursor, (3, 0));
    }

    #[test_case]
    fn line_length_is_limited() {
        let mut ed = LineEditor::new();
        let mut out = String::new();
        ed.start("> ", &mut out);
        let long: String = vec!['x'; MAX_LINE + 10].into_iter().collect();
        type_str(&mut ed, &long, &mut out);
        assert_eq!(ed.buffer().len(), MAX_LINE);
    }
}
