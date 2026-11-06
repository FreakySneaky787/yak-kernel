//! The shell: print a prompt, read a line, split it into words, look up the first word as a command, run it -
//! and again.
//!
//! Every piece writes to a `core::fmt::Write`, not straight to the screen: the shell writes to the screen, the
//! tests into a `String` they can compare.

use crate::block::{BlockDevice, SECTOR_SIZE};
use crate::scancode::Key;
use crate::words::{self, UnclosedQuote};
use crate::{ahci, console, heap, mem, pmm, task, time};
use alloc::string::String;
use core::fmt::Write;

pub const PROMPT: &str = "yak> ";
/// The longest line. With the prompt it fits into a terminal of 80 columns, so a line never wraps - and Backspace,
/// which only moves left, never has to go up a row.
pub const MAX_LINE: usize = 72;

/// Collects the keys of one line.
pub struct LineEditor {
    line: String,
}

impl LineEditor {
    pub const fn new() -> LineEditor {
        LineEditor { line: String::new() }
    }

    /// Takes one key and shows its effect on `out`. On Enter, gives back the whole line and starts a new one.
    pub fn feed(&mut self, key: Key, out: &mut dyn Write) -> Option<String> {
        match key {
            Key::Char(c) if self.line.chars().count() < MAX_LINE => {
                self.line.push(c);
                let _ = out.write_char(c);
            }
            Key::Char(_) => {} // the line is full: the key is dropped
            Key::Backspace => {
                // at the start of the line there is nothing to erase - and the prompt must stay
                if self.line.pop().is_some() {
                    let _ = out.write_str("\x08 \x08");
                }
            }
            Key::Enter => {
                let _ = out.write_char('\n');
                return Some(core::mem::take(&mut self.line));
            }
        }
        None
    }
}

/// A command gets its arguments (the words after its name) and where to write. An `Err` is printed after its name.
type Run = fn(&[String], &mut dyn Write) -> Result<(), String>;

struct Command {
    name: &'static str,
    usage: &'static str,
    help: &'static str,
    run: Run,
}

/// Every command the shell knows. A new one is one more entry here.
static COMMANDS: &[Command] = &[
    Command {
        name: "help",
        usage: "help",
        help: "list the commands",
        run: help,
    },
    Command {
        name: "echo",
        usage: "echo [words]",
        help: "print the words, one space between them",
        run: echo,
    },
    Command {
        name: "clear",
        usage: "clear",
        help: "clear the screen",
        run: clear,
    },
    Command {
        name: "uptime",
        usage: "uptime",
        help: "time since boot",
        run: uptime,
    },
    Command {
        name: "sleep",
        usage: "sleep <ms>",
        help: "wait for <ms> milliseconds",
        run: sleep,
    },
    Command {
        name: "mem",
        usage: "mem",
        help: "free frames and the heap",
        run: memory,
    },
    Command {
        name: "threads",
        usage: "threads",
        help: "every thread and what it does",
        run: threads,
    },
    Command {
        name: "disk",
        usage: "disk",
        help: "the SATA disk: model and size",
        run: disk,
    },
    Command {
        name: "read",
        usage: "read <sector>",
        help: "show the first 64 bytes of a sector",
        run: read,
    },
];

/// Runs one line: split it, find the command, call it.
pub fn execute(line: &str, out: &mut dyn Write) {
    let words = match words::split(line) {
        Ok(words) => words,
        Err(UnclosedQuote(quote)) => {
            let _ = writeln!(out, "the {} quote is never closed", quote);
            return;
        }
    };
    let Some((name, args)) = words.split_first() else {
        return; // an empty line: nothing to do
    };
    match COMMANDS.iter().find(|command| command.name == name) {
        Some(command) => {
            if let Err(message) = (command.run)(args, out) {
                let _ = writeln!(out, "{}: {}", name, message);
            }
        }
        None => {
            let _ = writeln!(out, "unknown command: {} (try help)", name);
        }
    }
}

fn help(_: &[String], out: &mut dyn Write) -> Result<(), String> {
    for command in COMMANDS {
        let _ = writeln!(out, "  {:<12} {}", command.usage, command.help);
    }
    Ok(())
}

fn echo(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let _ = writeln!(out, "{}", args.join(" "));
    Ok(())
}

fn clear(_: &[String], _: &mut dyn Write) -> Result<(), String> {
    if let Some(console) = console::CONSOLE.lock().as_mut() {
        console.clear();
    }
    Ok(())
}

fn uptime(_: &[String], out: &mut dyn Write) -> Result<(), String> {
    let ms = time::uptime_ms();
    let _ = writeln!(out, "up {}.{:02} s", ms / 1000, ms % 1000 / 10);
    Ok(())
}

fn sleep(args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let [ms] = args else {
        return Err(String::from("usage: sleep <ms>"));
    };
    let ms: u64 = ms.parse().map_err(|_| alloc::format!("not a number: {}", ms))?;
    time::sleep_ms(ms);
    Ok(())
}

fn memory(_: &[String], out: &mut dyn Write) -> Result<(), String> {
    let mib = |frames: usize| frames as u64 * mem::PAGE_SIZE / (1024 * 1024);
    let (free, usable) = (pmm::free_frames(), pmm::info().usable_frames);
    let _ = writeln!(
        out,
        "frames: {} of {} free ({} of {} MiB)",
        free,
        usable,
        mib(free),
        mib(usable)
    );
    let _ = writeln!(
        out,
        "heap: {} bytes in use, {} KiB mapped",
        heap::used(),
        heap::mapped() / 1024
    );
    Ok(())
}

fn threads(_: &[String], out: &mut dyn Write) -> Result<(), String> {
    for (id, name, state, cpu) in task::list() {
        let on = cpu.map(|cpu| alloc::format!(", on CPU {}", cpu)).unwrap_or_default();
        let _ = writeln!(out, "{:>4}  {:<8} {:?}{}", id, name, state, on);
    }
    Ok(())
}

fn disk(_: &[String], out: &mut dyn Write) -> Result<(), String> {
    let guard = ahci::DISK.lock();
    let disk = guard.as_ref().ok_or("no SATA disk")?;
    let _ = writeln!(
        out,
        "{} on port {}: {} sectors of {} bytes",
        disk.model(),
        disk.port(),
        disk.sector_count(),
        SECTOR_SIZE
    );
    Ok(())
}

fn read(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let [lba] = args else {
        return Err(String::from("usage: read <sector>"));
    };
    let lba: u64 = lba.parse().map_err(|_| alloc::format!("not a number: {}", lba))?;
    let mut sector = [0u8; SECTOR_SIZE];
    let mut guard = ahci::DISK.lock();
    let disk = guard.as_mut().ok_or("no SATA disk")?;
    disk.read(lba, &mut sector)
        .map_err(|e| alloc::format!("sector {}: {:?}", lba, e))?;
    drop(guard);
    hex_dump(&sector[..64], out);
    Ok(())
}

/// 16 bytes a line: where, the bytes in hex, and as text - `.` for what is not printable ASCII.
fn hex_dump(bytes: &[u8], out: &mut dyn Write) {
    for (i, line) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "{:04x} ", i * 16);
        for byte in line {
            let _ = write!(out, " {:02x}", byte);
        }
        let text: String = line
            .iter()
            .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' })
            .collect();
        let _ = writeln!(out, "  |{}|", text);
    }
}

/// The screen (and the serial port) as a `Write`.
#[cfg(not(test))]
struct Screen;

#[cfg(not(test))]
impl Write for Screen {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        console::_print(format_args!("{}", s));
        Ok(())
    }
}

/// The shell's loop. It never ends: there is nothing to return to.
#[cfg(not(test))]
pub fn run() -> ! {
    let mut editor = LineEditor::new();
    loop {
        crate::print!("{}", PROMPT);
        let line = loop {
            if let Some(line) = editor.feed(crate::keyboard::read_key(), &mut Screen) {
                break line;
            }
        };
        execute(&line, &mut Screen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `line` writes.
    fn output(line: &str) -> String {
        let mut out = String::new();
        execute(line, &mut out);
        out
    }

    #[test_case]
    fn the_line_editor_echoes_and_erases() {
        let mut editor = LineEditor::new();
        let mut out = String::new();
        let keys = [
            Key::Char('l'),
            Key::Char('s'),
            Key::Backspace,
            Key::Backspace,
            Key::Backspace, // one too many: the line is empty, nothing happens
            Key::Char('h'),
            Key::Char('i'),
        ];
        for key in keys {
            assert_eq!(editor.feed(key, &mut out), None);
        }
        assert_eq!(editor.feed(Key::Enter, &mut out), Some(String::from("hi")));
        assert_eq!(out, "ls\x08 \x08\x08 \x08hi\n");
        assert_eq!(editor.feed(Key::Enter, &mut out), Some(String::new())); // the next line starts empty
    }

    #[test_case]
    fn a_full_line_takes_no_more_keys() {
        let mut editor = LineEditor::new();
        let mut out = String::new();
        for _ in 0..MAX_LINE + 5 {
            editor.feed(Key::Char('x'), &mut out);
        }
        assert_eq!(out.len(), MAX_LINE);
        assert_eq!(editor.feed(Key::Enter, &mut out).unwrap().len(), MAX_LINE);
    }

    #[test_case]
    fn echo_joins_its_words() {
        assert_eq!(output("echo  a   'b  c'"), "a b  c\n");
        assert_eq!(output("echo"), "\n");
    }

    #[test_case]
    fn empty_lines_and_unknown_commands() {
        assert_eq!(output(""), "");
        assert_eq!(output("   "), "");
        assert_eq!(output("nope 1 2"), "unknown command: nope (try help)\n");
        assert_eq!(output("echo 'oops"), "the ' quote is never closed\n");
    }

    #[test_case]
    fn help_lists_every_command() {
        let text = output("help");
        assert_eq!(text.lines().count(), COMMANDS.len());
        for command in COMMANDS {
            assert!(text.contains(command.usage), "{} is missing", command.name);
        }
    }

    #[test_case]
    fn sleep_checks_its_argument_and_sleeps() {
        assert_eq!(output("sleep"), "sleep: usage: sleep <ms>\n");
        assert_eq!(output("sleep 1 2"), "sleep: usage: sleep <ms>\n");
        assert_eq!(output("sleep soon"), "sleep: not a number: soon\n");
        let before = time::uptime_ms();
        assert_eq!(output("sleep 50"), "");
        assert!(time::uptime_ms() - before >= 50);
    }

    #[test_case]
    fn threads_lists_the_shell_itself() {
        // the test runs in thread 0, like the shell: it is the one running - on whichever CPU has it now
        assert!(
            output("threads").contains("   0  main     Running, on CPU "),
            "{}",
            output("threads")
        );
    }

    #[test_case]
    fn read_shows_a_sector_in_hex_and_as_text() {
        let text = output("read 5");
        let lines: alloc::vec::Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{}", text);
        assert_eq!(
            lines[0],
            "0000  59 41 4b 20 74 65 73 74 20 64 69 73 6b 2c 20 73  |YAK test disk, s|"
        );
        assert!(lines[1].ends_with("|ector 5.........|"), "{}", lines[1]);
        assert_eq!(output("read 2048"), "read: sector 2048: OutOfRange\n");
        assert_eq!(output("read x"), "read: not a number: x\n");
        assert!(output("disk").starts_with("QEMU HARDDISK on port 0: 2048 sectors"));
    }

    #[test_case]
    fn uptime_and_mem_report_numbers() {
        assert!(output("uptime").starts_with("up "));
        let text = output("mem");
        assert!(text.starts_with("frames: ") && text.contains("\nheap: "), "{}", text);
    }
}
