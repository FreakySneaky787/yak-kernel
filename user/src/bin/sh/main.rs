//! /bin/sh: the shell as a user program. The kernel starts it as the first
//! process (init); it is immune to Ctrl+C, its children are not.
//!
//! - Line editing and word splitting reuse the kernel shell's code
//!   (`line_editor.rs`, `parser.rs` are compiled into this program).
//! - Built-ins: cd, pwd, echo, run, history, help, exit.
//! - Other words: a kernel built-in (mem, ps, write, ...) runs through the
//!   `kshell` system call; otherwise a program path or a name in /bin is
//!   started as a child and waited for.

#![no_std]
#![no_main]

extern crate alloc;

mod keys;
#[allow(dead_code)]
#[path = "../../../../kernel/src/shell/line_editor.rs"]
mod line_editor;
#[path = "../../../../kernel/src/shell/parser.rs"]
mod parser;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use line_editor::{EditResult, LineEditor};
use ulib::fs::{self, Dir};
use ulib::io::Stdout;
use ulib::process::{self, ExitStatus};
use ulib::{print, println, sys};

ulib::entry!(main);

const BIN_DIR: &str = "/bin";

/// name, usage, help
const BUILTINS: &[(&str, &str, &str)] = &[
    ("cd", "cd [dir]", "change the current directory (default: /)"),
    ("pwd", "pwd", "print the current directory"),
    ("echo", "echo [words...]", "print the arguments"),
    (
        "run",
        "run <program> [args...]",
        "run a program (also: just type its name if it is in /bin)",
    ),
    ("history", "history", "show the command history"),
    ("help", "help", "list built-ins, kernel commands and programs"),
    ("exit", "exit [code]", "leave the shell"),
];

struct Shell {
    editor: LineEditor,
    /// Commands the kernel runs for us (`kshell`).
    kernel_commands: Vec<String>,
}

fn error(text: &str) {
    println!("\x1b[91merror:\x1b[0m {}", text);
}

fn cwd() -> String {
    let mut buf = [0u8; 1024];
    fs::cwd(&mut buf)
        .map(String::from)
        .unwrap_or_else(|_| String::from("?"))
}

fn kernel_commands() -> Vec<String> {
    let mut buf = [0u8; 1024];
    let n = sys::kcommands(&mut buf);
    if n <= 0 {
        return Vec::new();
    }
    let names = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    names.split('\0').map(String::from).collect()
}

fn programs() -> Vec<String> {
    match Dir::open(BIN_DIR) {
        Ok(dir) => dir
            .flatten()
            .filter(|e| !e.is_dir())
            .map(|e| e.name().to_string())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Tab completion candidates: built-ins, kernel commands and programs.
fn completions(kernel_commands: &[String], prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = BUILTINS.iter().map(|b| String::from(b.0)).collect();
    names.extend(kernel_commands.iter().cloned());
    names.extend(programs());
    names.retain(|n| n.starts_with(prefix));
    names.sort();
    names.dedup();
    names
}

impl Shell {
    /// Reads and runs lines until `exit`; returns the exit code.
    fn run(&mut self) -> i32 {
        let mut decoder = keys::Decoder::new();
        loop {
            let prompt = alloc::format!("\x1b[92msh\x1b[0m:\x1b[94m{}\x1b[0m> ", cwd());
            self.editor.start(&prompt, &mut Stdout);
            'line: loop {
                let mut buf = [0u8; 16];
                let n = sys::read(sys::STDIN, &mut buf);
                if n <= 0 {
                    continue;
                }
                for &byte in &buf[..n as usize] {
                    let Some(event) = decoder.feed(byte) else { continue };
                    let commands = &self.kernel_commands;
                    let complete = |prefix: &str| completions(commands, prefix);
                    match self.editor.handle(event, &mut Stdout, &complete) {
                        EditResult::Pending => {}
                        EditResult::Cancelled => break 'line,
                        EditResult::Line(line) => {
                            if let Some(code) = self.execute(&line) {
                                return code;
                            }
                            break 'line;
                        }
                    }
                }
            }
        }
    }

    /// Runs one line. `Some(code)` = the shell should exit.
    fn execute(&mut self, line: &str) -> Option<i32> {
        let words = match parser::split(line) {
            Ok(w) => w,
            Err(e) => {
                error(&e.to_string());
                return None;
            }
        };
        let Some(name) = words.first() else { return None };
        let args = &words[1..];
        match name.as_str() {
            "cd" => {
                let dir = args.first().map_or("/", |s| s.as_str());
                if let Err(e) = fs::chdir(dir) {
                    error(&alloc::format!("{}: {}", dir, e));
                }
            }
            "pwd" => println!("{}", cwd()),
            "echo" => println!("{}", args.join(" ")),
            "history" => {
                for (i, l) in self.editor.history().iter().enumerate() {
                    println!("{:4}  {}", i + 1, l);
                }
            }
            "help" => self.help(),
            "exit" => return Some(args.first().and_then(|c| c.parse().ok()).unwrap_or(0)),
            "run" => match args.first() {
                Some(path) => run_program(path, &args[1..]),
                None => error("usage: run <program> [args...]"),
            },
            _ if self.kernel_commands.iter().any(|c| c == name) => {
                sys::kshell(line); // the kernel prints its own errors
            }
            _ => match find_program(name) {
                Some(path) => run_program(&path, args),
                None => error(&alloc::format!("unknown command '{}' (try 'help')", name)),
            },
        }
        None
    }

    fn help(&self) {
        println!("Shell built-ins (/bin/sh):");
        for (_, usage, help) in BUILTINS {
            println!("  \x1b[96m{:<18}\x1b[0m {}", usage, help);
        }
        println!("Kernel commands:");
        sys::kshell("help");
        print!("Programs in {}:", BIN_DIR);
        for p in programs() {
            print!(" {}", p);
        }
        println!();
    }
}

/// A path (contains '/') or a name in /bin.
fn find_program(name: &str) -> Option<String> {
    let path = if name.contains('/') {
        String::from(name)
    } else {
        alloc::format!("{}/{}", BIN_DIR, name)
    };
    fs::stat(&path).ok().filter(|s| s.kind == sys::KIND_FILE).map(|_| path)
}

fn run_program(path: &str, args: &[String]) {
    let name = path.rsplit('/').next().unwrap_or(path);
    match process::run(path, args.iter().map(String::as_str)) {
        Ok(ExitStatus::Exited(0)) => {}
        Ok(ExitStatus::Exited(code)) => error(&alloc::format!("{} exited with code {}", name, code)),
        Ok(ExitStatus::Killed(reason)) => error(&alloc::format!("{} was killed: {}", name, reason)),
        Err(e) => error(&alloc::format!("{}: {}", path, e)),
    }
}

fn main(_args: ulib::Args) -> i32 {
    let mut shell = Shell {
        editor: LineEditor::new(),
        kernel_commands: kernel_commands(),
    };
    shell.run()
}
