//! /bin/sh: the shell as a user program. The kernel starts it as the first
//! process (init); it is immune to Ctrl+C, its children are not.
//!
//! - Line editing and word splitting reuse the kernel shell's code
//!   (`line_editor.rs`, `parser.rs` are compiled into this program).
//! - Built-ins: cd, pwd, echo, run, history, help, exit.
//! - Other words: a kernel built-in (mem, ps, write, ...) runs through the
//!   `kshell` system call; otherwise a program path or a name in /bin is
//!   started as a child and waited for.
//! - Pipelines and redirections: `a | b | c`, `> file`, `>> file`, `< file`.
//!   The operators must be separate words (`ls > x`, not `ls>x`). Before each
//!   stage starts, stdin/stdout are pointed at the pipe or file with `dup2`
//!   (children inherit fds 0-2); afterwards the shell's own are restored.
//!   Built-ins in a pipeline run inside the shell, so their output must fit
//!   into the pipe (64 KiB) - it is written before the next stage starts.

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
use ulib::{eprintln, print, println, sys};

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

/// Errors go to stderr: they must not end up in a pipe or file.
fn error(text: &str) {
    eprintln!("\x1b[91merror:\x1b[0m {}", text);
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
        if words.is_empty() {
            return None;
        }
        let stages = match parse_pipeline(&words) {
            Ok(s) => s,
            Err(e) => {
                error(&e);
                return None;
            }
        };
        let saved = (sys::dup(sys::STDIN) as u64, sys::dup(sys::STDOUT) as u64);
        let mut prev_read: Option<u64> = None;
        let mut children: Vec<(String, u64)> = Vec::new();
        let mut exit = None;
        for (i, stage) in stages.iter().enumerate() {
            // stdin: the previous pipe, a file, or the shell's own
            match prev_read.take() {
                Some(r) => {
                    sys::dup2(r, sys::STDIN);
                    sys::close(r);
                }
                None => {
                    sys::dup2(saved.0, sys::STDIN);
                }
            }
            // stdout: a new pipe to the next stage, or the shell's own
            if i + 1 < stages.len() {
                let mut fds = [0u32; 2];
                if sys::pipe(&mut fds) < 0 {
                    restore(saved);
                    error("cannot create a pipe");
                    break;
                }
                sys::dup2(fds[1] as u64, sys::STDOUT);
                sys::close(fds[1] as u64);
                prev_read = Some(fds[0] as u64);
            } else {
                sys::dup2(saved.1, sys::STDOUT);
            }
            if let Err(e) = stage.redirect() {
                restore(saved);
                error(&e);
                break;
            }
            match self.run_stage(&stage.words) {
                Stage::Done => {}
                Stage::Exit(code) => exit = Some(code),
                Stage::Spawned(name, pid) => children.push((name, pid)),
                Stage::Failed(e) => error(&e), // stderr: still the console
            }
        }
        restore(saved);
        if let Some(r) = prev_read {
            sys::close(r);
        }
        sys::close(saved.0);
        sys::close(saved.1);
        wait_all(&children);
        exit
    }

    /// Runs one stage with the already redirected stdin/stdout.
    fn run_stage(&mut self, words: &[String]) -> Stage {
        let name = words[0].as_str();
        let args = &words[1..];
        match name {
            "cd" => {
                let dir = args.first().map_or("/", |s| s.as_str());
                if let Err(e) = fs::chdir(dir) {
                    return Stage::Failed(alloc::format!("{}: {}", dir, e));
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
            "exit" => return Stage::Exit(args.first().and_then(|c| c.parse().ok()).unwrap_or(0)),
            "run" => match args.first() {
                Some(path) => return spawn_program(path, &args[1..]),
                None => return Stage::Failed(String::from("usage: run <program> [args...]")),
            },
            _ if self.kernel_commands.iter().any(|c| c == name) => {
                sys::kshell(&quote_line(words)); // the kernel prints its own errors
            }
            _ => {
                return match find_program(name) {
                    Some(path) => spawn_program(&path, args),
                    None => Stage::Failed(alloc::format!("unknown command '{}' (try 'help')", name)),
                };
            }
        }
        Stage::Done
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

enum Stage {
    Done,
    Exit(i32),
    Spawned(String, u64),
    Failed(String),
}

/// One command of a pipeline with its redirections.
struct Command {
    words: Vec<String>,
    input: Option<String>,
    /// file, append?
    output: Option<(String, bool)>,
}

impl Command {
    /// Points stdin/stdout at the redirection files (after the pipes are set up).
    fn redirect(&self) -> Result<(), String> {
        if let Some(path) = &self.input {
            let fd = sys::open(path, sys::O_READ);
            if fd < 0 {
                return Err(alloc::format!("{}: {}", path, fs::Error(-fd)));
            }
            sys::dup2(fd as u64, sys::STDIN);
            sys::close(fd as u64);
        }
        if let Some((path, append)) = &self.output {
            let mut flags = sys::O_WRITE | sys::O_CREATE;
            flags |= if *append { sys::O_APPEND } else { sys::O_TRUNCATE };
            let fd = sys::open(path, flags);
            if fd < 0 {
                return Err(alloc::format!("{}: {}", path, fs::Error(-fd)));
            }
            sys::dup2(fd as u64, sys::STDOUT);
            sys::close(fd as u64);
        }
        Ok(())
    }
}

/// Splits words at `|` and pulls out `<`, `>`, `>>` with their file names.
fn parse_pipeline(words: &[String]) -> Result<Vec<Command>, String> {
    let mut commands = Vec::new();
    for part in words.split(|w| w == "|") {
        let mut cmd = Command {
            words: Vec::new(),
            input: None,
            output: None,
        };
        let mut it = part.iter();
        while let Some(w) = it.next() {
            match w.as_str() {
                "<" | ">" | ">>" => {
                    let file = it
                        .next()
                        .ok_or_else(|| alloc::format!("missing file name after '{}'", w))?;
                    if w == "<" {
                        cmd.input = Some(file.clone());
                    } else {
                        cmd.output = Some((file.clone(), w == ">>"));
                    }
                }
                _ => cmd.words.push(w.clone()),
            }
        }
        if cmd.words.is_empty() {
            return Err(String::from("missing command in pipeline"));
        }
        commands.push(cmd);
    }
    Ok(commands)
}

/// Puts the shell's own stdin/stdout back.
fn restore(saved: (u64, u64)) {
    sys::dup2(saved.0, sys::STDIN);
    sys::dup2(saved.1, sys::STDOUT);
}

/// Rebuilds a command line for the kernel's parser (every word quoted).
fn quote_line(words: &[String]) -> String {
    let mut line = String::new();
    for w in words {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push('"');
        for c in w.chars() {
            if c == '"' || c == '\\' {
                line.push('\\');
            }
            line.push(c);
        }
        line.push('"');
    }
    line
}

fn spawn_program(path: &str, args: &[String]) -> Stage {
    let name = String::from(path.rsplit('/').next().unwrap_or(path));
    match process::spawn(path, args.iter().map(String::as_str)) {
        Ok(pid) => Stage::Spawned(name, pid),
        Err(e) => Stage::Failed(alloc::format!("{}: {}", path, e)),
    }
}

/// Waits for all children of a pipeline (first to last). The last one's exit
/// code and every kill are reported; after a Ctrl+C the rest is killed too.
fn wait_all(children: &[(String, u64)]) {
    let mut interrupted = false;
    for (i, (name, pid)) in children.iter().enumerate() {
        if interrupted {
            let _ = process::kill(*pid);
        }
        match process::wait(*pid) {
            Ok(ExitStatus::Exited(0)) => {}
            Ok(ExitStatus::Exited(code)) if i + 1 == children.len() => {
                error(&alloc::format!("{} exited with code {}", name, code))
            }
            Ok(ExitStatus::Exited(_)) => {}
            Ok(ExitStatus::Killed(reason)) => {
                if reason == "interrupted" {
                    interrupted = true;
                }
                error(&alloc::format!("{} was killed: {}", name, reason))
            }
            Err(e) => error(&alloc::format!("wait for {}: {}", name, e)),
        }
    }
}

fn main(_args: ulib::Args) -> i32 {
    let mut shell = Shell {
        editor: LineEditor::new(),
        kernel_commands: kernel_commands(),
    };
    shell.run()
}
