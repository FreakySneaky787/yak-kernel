//! The shell: print a prompt, read a line, split it into words, look up the first word as a command, run it -
//! and again.
//!
//! Every piece writes to a `core::fmt::Write`, not straight to the screen: the shell writes to the screen, the
//! tests into a `String` they can compare.

use crate::block::{BlockDevice, SECTOR_SIZE};
use crate::scancode::Key;
use crate::words::{self, UnclosedQuote};
use crate::{ahci, console, fs, heap, mem, pmm, task, time};
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
    Command {
        name: "ls",
        usage: "ls [dir]",
        help: "the files of a directory (/ if none)",
        run: ls,
    },
    Command {
        name: "cat",
        usage: "cat <file>",
        help: "print a file",
        run: cat,
    },
    Command {
        name: "write",
        usage: "write <file> [words]",
        help: "make a file of the words (replaces it)",
        run: write,
    },
    Command {
        name: "append",
        usage: "append <file> [words]",
        help: "add the words to the end of a file",
        run: append,
    },
    Command {
        name: "mkdir",
        usage: "mkdir <dir>",
        help: "make a directory",
        run: mkdir,
    },
    Command {
        name: "rm",
        usage: "rm <path>",
        help: "remove a file or an empty directory",
        run: rm,
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

/// A path as the file system wants it: absolute. The shell has no current directory yet, so "a.txt" is "/a.txt".
fn absolute(path: &str) -> String {
    if path.starts_with('/') {
        String::from(path)
    } else {
        alloc::format!("/{}", path)
    }
}

fn ls(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let path = match args {
        [] => String::from("/"),
        [path] => absolute(path),
        _ => return Err(String::from("usage: ls [dir]")),
    };
    let mut guard = fs::FS.lock();
    let fs = guard.as_mut().ok_or("no file system")?;
    let entries = fs.read_dir(&path).map_err(|e| alloc::format!("{}: {:?}", path, e))?;
    for entry in entries {
        if entry.is_dir {
            let _ = writeln!(out, "   <dir>  {}/", entry.name);
        } else {
            let _ = writeln!(out, "{:>8}  {}", entry.size, entry.name);
        }
    }
    Ok(())
}

fn cat(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let [path] = args else {
        return Err(String::from("usage: cat <file>"));
    };
    let path = absolute(path);
    let data = fs::FS
        .lock()
        .as_mut()
        .ok_or("no file system")?
        .read_file(&path)
        .map_err(|e| alloc::format!("{}: {:?}", path, e))?;
    let _ = out.write_str(&String::from_utf8_lossy(&data));
    Ok(())
}

/// The words after the file name as one line, as `echo` would print them.
fn line_of(words: &[String]) -> String {
    let mut line = words.join(" ");
    line.push('\n');
    line
}

/// Runs `f` on the mounted file system; an error is reported with the path.
fn on_fs(
    path: &str,
    f: impl FnOnce(&mut crate::fat32::FatFs<fs::SystemDisk>, &str) -> crate::fat32::Result<()>,
) -> Result<(), String> {
    let path = absolute(path);
    let mut guard = fs::FS.lock();
    let fs = guard.as_mut().ok_or("no file system")?;
    f(fs, &path).map_err(|e| alloc::format!("{}: {:?}", path, e))
}

fn write(args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let [path, words @ ..] = args else {
        return Err(String::from("usage: write <file> [words]"));
    };
    on_fs(path, |fs, path| fs.write_file(path, line_of(words).as_bytes()))
}

fn append(args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let [path, words @ ..] = args else {
        return Err(String::from("usage: append <file> [words]"));
    };
    on_fs(path, |fs, path| fs.append(path, line_of(words).as_bytes()))
}

fn mkdir(args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let [path] = args else {
        return Err(String::from("usage: mkdir <dir>"));
    };
    on_fs(path, |fs, path| fs.create_dir(path))
}

fn rm(args: &[String], _: &mut dyn Write) -> Result<(), String> {
    let [path] = args else {
        return Err(String::from("usage: rm <path>"));
    };
    on_fs(path, |fs, path| fs.remove(path))
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
        // sector 0 is the boot sector now: a jump instruction, then the name of the tool that made the volume
        let text = output("read 0");
        assert_eq!(text.lines().count(), 4, "{}", text);
        assert!(text.starts_with("0000  eb 58 90 6d 6b 66 73 2e 66 61 74"), "{}", text);
        assert!(text.lines().next().unwrap().contains("|.X.mkfs.fat"), "{}", text);
        assert_eq!(output("read 131072"), "read: sector 131072: OutOfRange\n");
        assert!(output("disk").starts_with("QEMU HARDDISK on port 0: 131072 sectors"));
    }

    #[test_case]
    fn ls_and_cat_show_the_disk() {
        let root = output("ls");
        assert!(root.contains("      21  hello.txt\n"), "{}", root);
        assert!(root.contains("   <dir>  docs/\n"), "{}", root);
        // in the order of the directory: mkdisk.sh made deeper/ before it copied notes.txt
        assert_eq!(output("ls docs"), "   <dir>  deeper/\n      27  notes.txt\n");
        assert_eq!(output("cat hello.txt"), "Hello from the disk!\n");
        assert_eq!(output("cat /docs/deeper/end.txt"), "The end of the path.\n");
        assert_eq!(output("cat nope"), "cat: /nope: NotFound\n");
        assert_eq!(output("cat docs"), "cat: /docs: IsADirectory\n");
        assert_eq!(output("ls hello.txt"), "ls: /hello.txt: NotADirectory\n");
    }

    #[test_case]
    fn write_append_mkdir_and_rm() {
        assert_eq!(output("mkdir notes"), "");
        assert_eq!(output("write notes/todo.txt buy milk"), "");
        assert_eq!(output("append notes/todo.txt write chapter 19"), "");
        assert_eq!(output("cat notes/todo.txt"), "buy milk\nwrite chapter 19\n");
        assert_eq!(output("rm notes"), "rm: /notes: NotEmpty\n");
        assert_eq!(output("write Long-Name.txt x"), "write: /Long-Name.txt: BadName\n");
        assert_eq!(output("rm notes/todo.txt"), "");
        assert_eq!(output("rm notes"), "");
        assert_eq!(output("cat notes/todo.txt"), "cat: /notes/todo.txt: NotFound\n");
    }

    #[test_case]
    fn uptime_and_mem_report_numbers() {
        assert!(output("uptime").starts_with("up "));
        let text = output("mem");
        assert!(text.starts_with("frames: ") && text.contains("\nheap: "), "{}", text);
    }
}
