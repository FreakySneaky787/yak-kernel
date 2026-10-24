//! Interactive line editor: cursor movement, history and tab completion.
//!
//! The editor only produces a terminal-compatible byte stream (`\x08` moves
//! left, characters overwrite), so it renders the same on our framebuffer
//! console and on a serial terminal. It is pure logic and fully unit-tested
//! with a `String` as output.

use crate::drivers::scancode::{Key, KeyEvent};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// Maximum line length (keeps lines on one console row: prompt + 120 < 160 columns).
pub const MAX_LINE: usize = 120;
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
        }
    }

    /// Starts a new line: prints the prompt and resets the buffer.
    pub fn start(&mut self, prompt: &str, out: &mut dyn Write) {
        self.prompt = String::from(prompt);
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
                    self.cursor -= 1;
                    self.buf.remove(self.cursor);
                    out.write_str("\x08").ok();
                    self.redraw_tail(out, 1);
                }
            }
            Key::Delete => {
                if self.cursor < self.buf.len() {
                    self.buf.remove(self.cursor);
                    self.redraw_tail(out, 1);
                }
            }
            Key::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    out.write_str("\x08").ok();
                }
            }
            Key::Right => {
                if self.cursor < self.buf.len() {
                    out.write_char(self.buf[self.cursor]).ok();
                    self.cursor += 1;
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
                self.buf.drain(..self.cursor);
                self.move_home_raw(out, removed);
                self.redraw_tail(out, removed);
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
        self.buf.insert(self.cursor, c);
        out.write_char(c).ok();
        self.cursor += 1;
        self.redraw_tail(out, 0);
    }

    /// Rewrites everything after the cursor, blanks `erased` extra cells
    /// (when the line got shorter) and moves the cursor back.
    fn redraw_tail(&mut self, out: &mut dyn Write, erased: usize) {
        let tail = &self.buf[self.cursor..];
        for &c in tail {
            out.write_char(c).ok();
        }
        for _ in 0..erased {
            out.write_char(' ').ok();
        }
        for _ in 0..tail.len() + erased {
            out.write_char('\x08').ok();
        }
    }

    fn move_home_raw(&mut self, out: &mut dyn Write, steps: usize) {
        for _ in 0..steps {
            out.write_char('\x08').ok();
        }
        self.cursor = 0;
    }

    fn move_home(&mut self, out: &mut dyn Write) {
        let steps = self.cursor;
        self.move_home_raw(out, steps);
    }

    fn move_end(&mut self, out: &mut dyn Write) {
        for &c in &self.buf[self.cursor..] {
            out.write_char(c).ok();
        }
        self.cursor = self.buf.len();
    }

    /// Replaces the whole line on screen with `new`.
    fn replace_line(&mut self, new: Vec<char>, out: &mut dyn Write) {
        let old_len = self.buf.len();
        self.move_home(out);
        self.buf = new;
        let erased = old_len.saturating_sub(self.buf.len());
        self.redraw_tail(out, erased);
        self.move_end(out);
    }

    /// Prints prompt + buffer again (after clearing the screen or listing completions).
    fn redraw_line(&mut self, out: &mut dyn Write) {
        out.write_str(&self.prompt).ok();
        for &c in &self.buf {
            out.write_char(c).ok();
        }
        for _ in self.cursor..self.buf.len() {
            out.write_char('\x08').ok();
        }
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
                '\n' => {
                    line.clear();
                    col = 0;
                }
                '\x1b' => {
                    // skip CSI sequence
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
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
