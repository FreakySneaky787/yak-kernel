//! /bin/sh: the shell as a user program. The kernel starts it as the first
//! process (init); it is immune to Ctrl+C, its children are not.
//!
//! - Line editing and word splitting reuse the kernel shell's code
//!   (`line_editor.rs`, `parser.rs` are compiled into this program).
//! - Built-ins: cd, pwd, echo, run, history, help, exit, export, unset, set,
//!   source (`.`), jobs, wait, fg, true, false.
//! - Other words: a kernel built-in (lspci, acpi, ...) runs through the
//!   `kshell` system call; otherwise a program path or a name found in
//!   `$PATH` (default `/bin`) is started as a child. A file starting with
//!   `#!` runs as a shell script.
//! - Variables: `NAME=value`, `export NAME=value`, `unset NAME`; they live in
//!   the kernel's per-process environment, so children inherit them.
//!   `$NAME`, `${NAME}`, `$?` (last status), `$$`, `$#`, `$0`..`$9` (script
//!   arguments) are expanded before the line is split into words - not inside
//!   '...'; `\$` keeps a dollar. `#` at the start of a word begins a comment.
//! - Pipelines and redirections: `a | b`, `> f`, `>> f`, `< f`, `2> f`,
//!   `2>> f`, `2>&1`. Operators must be separate words. Before each stage,
//!   fds 0-2 are pointed at pipes/files with `dup2` (children inherit them),
//!   afterwards the shell's own are restored. Built-ins in a pipeline run
//!   inside the shell, so their output must fit into a pipe (64 KiB).
//! - Jobs: a line ending in `&` runs in the background (`[n] pid`); `jobs`
//!   lists them, `wait [%n]` waits, `fg [%n]` waits in the foreground (Ctrl+C
//!   then reaches it). Finished jobs are reported before the next prompt.
//! - Scripts: `sh file [args]`, `sh -c "command line"`, `source file`; one
//!   command per line, `set -e` stops at the first failing command.

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
use ulib::fs::{self, Dir, File};
use ulib::io::Stdout;
use ulib::process::{self, ExitStatus};
use ulib::{env, eprintln, io, print, println, sys};

ulib::entry!(main);

const DEFAULT_PATH: &str = "/bin";
const ENOEXEC: i64 = 8;

/// name, usage, help
const BUILTINS: &[(&str, &str, &str)] = &[
    ("cd", "cd [dir]", "change the current directory (default: $HOME)"),
    ("pwd", "pwd", "print the current directory"),
    ("echo", "echo [words...]", "print the arguments"),
    (
        "run",
        "run <program> [args...]",
        "run a program (also: just type its name)",
    ),
    ("history", "history", "show the command history"),
    ("help", "help", "list built-ins, kernel commands and programs"),
    ("exit", "exit [code]", "leave the shell (default code: $?)"),
    (
        "export",
        "export NAME=value...",
        "set environment variables (NAME=value alone works too)",
    ),
    ("unset", "unset NAME...", "remove environment variables"),
    (
        "set",
        "set -e | set +e",
        "stop a script at the first failing command (or not)",
    ),
    ("source", "source <file> (or . <file>)", "run a script in this shell"),
    ("jobs", "jobs", "list background jobs (started with &)"),
    ("wait", "wait [%n]", "wait for background jobs"),
    ("fg", "fg [%n]", "wait for a job in the foreground (Ctrl+C reaches it)"),
    ("true", "true", "do nothing, successfully"),
    ("false", "false", "do nothing, unsuccessfully"),
];

struct Job {
    id: usize,
    command: String,
    /// (name, pid, final status once collected)
    procs: Vec<(String, u64, Option<ExitStatus>)>,
}

struct Shell {
    editor: LineEditor,
    /// Commands the kernel runs for us (`kshell`).
    kernel_commands: Vec<String>,
    /// Exit status of the last command (`$?`).
    status: i32,
    /// `$0`, `$1`, ... (script name and arguments)
    positional: Vec<String>,
    jobs: Vec<Job>,
    next_job: usize,
    exit_on_error: bool,
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

fn path_dirs() -> Vec<String> {
    let path = env::var("PATH").unwrap_or_else(|| String::from(DEFAULT_PATH));
    path.split(':').filter(|d| !d.is_empty()).map(String::from).collect()
}

fn programs() -> Vec<String> {
    let mut names = Vec::new();
    for dir in path_dirs() {
        if let Ok(d) = Dir::open(&dir) {
            names.extend(d.flatten().filter(|e| !e.is_dir()).map(|e| e.name().to_string()));
        }
    }
    names
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

fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `NAME=value` with a valid name.
fn assignment(word: &str) -> Option<(&str, &str)> {
    word.split_once('=').filter(|(n, _)| is_name(n))
}

/// The status a finished process leaves in `$?`.
fn status_code(status: &ExitStatus) -> i32 {
    match status {
        ExitStatus::Exited(code) => *code as i32,
        ExitStatus::Killed(reason) if reason == "interrupted" => 130,
        ExitStatus::Killed(_) => 137,
    }
}

impl Shell {
    fn new(positional: Vec<String>) -> Shell {
        Shell {
            editor: LineEditor::new(),
            kernel_commands: kernel_commands(),
            status: 0,
            positional,
            jobs: Vec::new(),
            next_job: 1,
            exit_on_error: false,
        }
    }

    /// Reads and runs lines until `exit`; returns the exit code.
    fn interactive(&mut self) -> i32 {
        let mut decoder = keys::Decoder::new();
        loop {
            self.report_finished_jobs();
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

    /// Runs every line of `text`; `Some(code)` if the script asked to exit
    /// (or `set -e` stopped it).
    fn run_lines(&mut self, text: &str) -> Option<i32> {
        for line in text.lines() {
            if let Some(code) = self.execute(line) {
                return Some(code);
            }
        }
        None
    }

    fn script(&mut self, path: &str) -> i32 {
        match io::read_input(Some(path)) {
            Ok(data) => {
                let text = String::from_utf8_lossy(&data).into_owned();
                self.run_lines(&text).unwrap_or(self.status)
            }
            Err(e) => {
                error(&alloc::format!("{}: {}", path, e));
                127
            }
        }
    }

    /// Replaces `$...` outside single quotes and drops `#` comments.
    fn expand(&self, line: &str) -> String {
        let chars: Vec<char> = line.chars().collect();
        let mut out = String::new();
        let (mut single, mut double) = (false, false);
        let mut word_start = true;
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if single {
                out.push(c);
                single = c != '\'';
                i += 1;
                continue;
            }
            match c {
                '\\' => {
                    out.push(c);
                    if let Some(&next) = chars.get(i + 1) {
                        out.push(next);
                        i += 1;
                    }
                }
                '\'' if !double => {
                    single = true;
                    out.push(c);
                }
                '"' => {
                    double = !double;
                    out.push(c);
                }
                '#' if !double && word_start => break,
                '$' => {
                    let (name, used) = match chars.get(i + 1) {
                        Some('{') => {
                            let end = chars[i + 2..].iter().position(|&c| c == '}').map(|p| i + 2 + p);
                            match end {
                                Some(e) => (chars[i + 2..e].iter().collect::<String>(), e - i),
                                None => (String::new(), 0),
                            }
                        }
                        Some(&c) if c == '?' || c == '#' || c == '$' || c.is_ascii_digit() => (String::from(c), 1),
                        _ => {
                            let n: String = chars[i + 1..]
                                .iter()
                                .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                                .collect();
                            let len = n.chars().count();
                            (n, len)
                        }
                    };
                    if name.is_empty() {
                        out.push('$');
                    } else {
                        out.push_str(&self.value(&name));
                        i += used;
                    }
                }
                _ => out.push(c),
            }
            word_start = !single && !double && (c.is_whitespace() || (word_start && c == '#'));
            i += 1;
        }
        out
    }

    fn value(&self, name: &str) -> String {
        match name {
            "?" => alloc::format!("{}", self.status),
            "$" => alloc::format!("{}", sys::getpid()),
            "#" => alloc::format!("{}", self.positional.len().saturating_sub(1)),
            n if n.chars().all(|c| c.is_ascii_digit()) => n
                .parse::<usize>()
                .ok()
                .and_then(|i| self.positional.get(i).cloned())
                .unwrap_or_default(),
            n => env::var(n).unwrap_or_default(),
        }
    }

    /// Runs one line. `Some(code)` = the shell should exit.
    fn execute(&mut self, line: &str) -> Option<i32> {
        let expanded = self.expand(line);
        let mut words = match parser::split(&expanded) {
            Ok(w) => w,
            Err(e) => {
                error(&e.to_string());
                self.status = 2;
                return self.check_exit_on_error();
            }
        };
        if words.is_empty() {
            return None;
        }
        let background = words.last().is_some_and(|w| w == "&");
        if background {
            words.pop();
            if words.is_empty() {
                return None;
            }
        }
        if words.len() == 1 {
            if let Some((name, value)) = assignment(&words[0]) {
                env::set_var(name, value);
                self.status = 0;
                return None;
            }
        }
        let stages = match parse_pipeline(&words) {
            Ok(s) => s,
            Err(e) => {
                error(&e);
                self.status = 2;
                return self.check_exit_on_error();
            }
        };
        let saved = [0, 1, 2].map(|fd| sys::dup(fd) as u64);
        let mut prev_read: Option<u64> = None;
        let mut children: Vec<(String, u64)> = Vec::new();
        let mut exit = None;
        let mut status = 0;
        for (i, stage) in stages.iter().enumerate() {
            // stdin: the previous pipe, or the shell's own; stdout: a new pipe or the shell's own
            match prev_read.take() {
                Some(r) => {
                    sys::dup2(r, sys::STDIN);
                    sys::close(r);
                }
                None => {
                    sys::dup2(saved[0], sys::STDIN);
                }
            }
            sys::dup2(saved[2], sys::STDERR);
            if i + 1 < stages.len() {
                let mut fds = [0u32; 2];
                if sys::pipe(&mut fds) < 0 {
                    restore(&saved);
                    error("cannot create a pipe");
                    status = 1;
                    break;
                }
                sys::dup2(fds[1] as u64, sys::STDOUT);
                sys::close(fds[1] as u64);
                prev_read = Some(fds[0] as u64);
            } else {
                sys::dup2(saved[1], sys::STDOUT);
            }
            if let Err(e) = stage.redirect() {
                restore(&saved);
                error(&e);
                status = 1;
                break;
            }
            match self.run_stage(&stage.words) {
                Stage::Done(code) => status = code,
                Stage::Exit(code) => exit = Some(code),
                Stage::Spawned(name, pid) => children.push((name, pid)),
                Stage::Failed(e) => {
                    error(&e);
                    status = 1;
                }
            }
        }
        restore(&saved);
        if let Some(r) = prev_read {
            sys::close(r);
        }
        for fd in saved {
            sys::close(fd);
        }
        if background && !children.is_empty() {
            let id = self.next_job;
            self.next_job += 1;
            let pids: Vec<String> = children.iter().map(|(_, p)| alloc::format!("{}", p)).collect();
            println!("[{}] {}", id, pids.join(" "));
            self.jobs.push(Job {
                id,
                command: words.join(" "),
                procs: children.into_iter().map(|(n, p)| (n, p, None)).collect(),
            });
            self.status = 0;
            return exit;
        }
        if !children.is_empty() {
            status = wait_all(&children);
        }
        self.status = status;
        exit.or_else(|| self.check_exit_on_error())
    }

    fn check_exit_on_error(&self) -> Option<i32> {
        (self.exit_on_error && self.status != 0).then_some(self.status)
    }

    /// Runs one stage with the already redirected fds.
    fn run_stage(&mut self, words: &[String]) -> Stage {
        let name = words[0].as_str();
        let args = &words[1..];
        match name {
            "cd" => {
                let dir = args
                    .first()
                    .cloned()
                    .or_else(|| env::var("HOME"))
                    .unwrap_or_else(|| String::from("/"));
                if let Err(e) = fs::chdir(&dir) {
                    return Stage::Failed(alloc::format!("{}: {}", dir, e));
                }
            }
            "pwd" => println!("{}", cwd()),
            "echo" => println!("{}", args.join(" ")),
            "true" => {}
            "false" => return Stage::Done(1),
            "history" => {
                for (i, l) in self.editor.history().iter().enumerate() {
                    println!("{:4}  {}", i + 1, l);
                }
            }
            "help" => self.help(),
            "exit" => return Stage::Exit(args.first().and_then(|c| c.parse().ok()).unwrap_or(self.status)),
            "export" => {
                for a in args {
                    if let Some((n, v)) = assignment(a) {
                        env::set_var(n, v);
                    } else if !is_name(a) {
                        return Stage::Failed(alloc::format!("export: '{}' is not a valid name", a));
                    }
                }
            }
            "unset" => args.iter().for_each(|a| env::remove_var(a)),
            "set" => match args.first().map(|s| s.as_str()) {
                Some("-e") => self.exit_on_error = true,
                Some("+e") => self.exit_on_error = false,
                _ => {
                    for (k, v) in env::vars() {
                        println!("{}={}", k, v);
                    }
                }
            },
            "source" | "." => match args.first() {
                Some(path) => match io::read_input(Some(path)) {
                    Ok(data) => {
                        let text = String::from_utf8_lossy(&data).into_owned();
                        if let Some(code) = self.run_lines(&text) {
                            return Stage::Exit(code);
                        }
                        return Stage::Done(self.status);
                    }
                    Err(e) => return Stage::Failed(alloc::format!("{}: {}", path, e)),
                },
                None => return Stage::Failed(String::from("usage: source <file>")),
            },
            "jobs" => {
                self.poll_jobs();
                for job in &self.jobs {
                    let done = job.procs.iter().all(|p| p.2.is_some());
                    println!(
                        "[{}] {:<8} {}",
                        job.id,
                        if done { "Done" } else { "Running" },
                        job.command
                    );
                }
            }
            "wait" | "fg" => return self.wait_jobs(args.first().map(|s| s.as_str())),
            "run" => match args.first() {
                Some(path) => return spawn_program(path, &args[1..]),
                None => return Stage::Failed(String::from("usage: run <program> [args...]")),
            },
            _ if self.kernel_commands.iter().any(|c| c == name) => {
                return Stage::Done(sys::kshell(&quote_line(words)) as i32); // the kernel prints its own errors
            }
            _ => {
                return match find_program(name) {
                    Some(path) => spawn_program(&path, args),
                    None => Stage::Failed(alloc::format!("unknown command '{}' (try 'help')", name)),
                };
            }
        }
        Stage::Done(0)
    }

    /// Collects statuses of finished background processes (without blocking).
    fn poll_jobs(&mut self) {
        for job in &mut self.jobs {
            for p in job.procs.iter_mut().filter(|p| p.2.is_none()) {
                if let Ok(Some(status)) = process::try_wait(p.1) {
                    p.2 = Some(status);
                }
            }
        }
    }

    /// "[n] Done  command" for jobs that ended since the last prompt.
    fn report_finished_jobs(&mut self) {
        self.poll_jobs();
        self.jobs.retain(|job| {
            let done = job.procs.iter().all(|p| p.2.is_some());
            if done {
                let last = job.procs.last().and_then(|p| p.2.clone());
                match last {
                    Some(ExitStatus::Exited(0)) | None => println!("[{}] Done     {}", job.id, job.command),
                    Some(s) => println!("[{}] {}  {}", job.id, s, job.command),
                }
            }
            !done
        });
    }

    /// `wait` / `fg`: blocks until the job (or all jobs) ended.
    fn wait_jobs(&mut self, which: Option<&str>) -> Stage {
        let id: Option<usize> = match which {
            Some(w) => match w.trim_start_matches('%').parse() {
                Ok(n) => Some(n),
                Err(_) => return Stage::Failed(alloc::format!("wait: bad job '{}'", w)),
            },
            None => None,
        };
        if id.is_some_and(|n| !self.jobs.iter().any(|j| j.id == n)) {
            return Stage::Failed(alloc::format!("wait: no job {}", id.unwrap()));
        }
        let mut status = 0;
        for job in self.jobs.iter_mut().filter(|j| id.is_none_or(|n| j.id == n)) {
            for p in job.procs.iter_mut().filter(|p| p.2.is_none()) {
                // a blocking wait lends the console to the job: Ctrl+C reaches it
                if let Ok(s) = process::wait(p.1) {
                    p.2 = Some(s);
                }
            }
            if let Some(Some(s)) = job.procs.last().map(|p| p.2.clone()) {
                status = status_code(&s);
            }
        }
        self.report_finished_jobs();
        Stage::Done(status)
    }

    fn help(&self) {
        println!("Shell built-ins (/bin/sh):");
        for (_, usage, help) in BUILTINS {
            println!("  \x1b[96m{:<28}\x1b[0m {}", usage, help);
        }
        println!("Kernel commands:");
        sys::kshell("help");
        print!("Programs in $PATH:");
        for p in programs() {
            print!(" {}", p);
        }
        println!();
    }
}

/// A path (contains '/') or a name found in `$PATH`.
fn find_program(name: &str) -> Option<String> {
    let candidates: Vec<String> = if name.contains('/') {
        alloc::vec![String::from(name)]
    } else {
        path_dirs()
            .iter()
            .map(|d| alloc::format!("{}/{}", d.trim_end_matches('/'), name))
            .collect()
    };
    candidates
        .into_iter()
        .find(|p| fs::stat(p).is_ok_and(|s| s.kind == sys::KIND_FILE))
}

enum Stage {
    /// A built-in ran, with this status.
    Done(i32),
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
    errors: Option<(String, bool)>,
    errors_to_output: bool,
}

fn open_for_writing(path: &str, append: bool) -> Result<u64, String> {
    let flags = sys::O_WRITE | sys::O_CREATE | if append { sys::O_APPEND } else { sys::O_TRUNCATE };
    let fd = sys::open(path, flags);
    if fd < 0 {
        return Err(alloc::format!("{}: {}", path, fs::Error(-fd)));
    }
    Ok(fd as u64)
}

impl Command {
    /// Points fds 0-2 at the redirection files (after the pipes are set up).
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
            let fd = open_for_writing(path, *append)?;
            sys::dup2(fd, sys::STDOUT);
            sys::close(fd);
        }
        if let Some((path, append)) = &self.errors {
            let fd = open_for_writing(path, *append)?;
            sys::dup2(fd, sys::STDERR);
            sys::close(fd);
        }
        if self.errors_to_output {
            sys::dup2(sys::STDOUT, sys::STDERR);
        }
        Ok(())
    }
}

/// Splits words at `|` and pulls out the redirection operators.
fn parse_pipeline(words: &[String]) -> Result<Vec<Command>, String> {
    let mut commands = Vec::new();
    for part in words.split(|w| w == "|") {
        let mut cmd = Command {
            words: Vec::new(),
            input: None,
            output: None,
            errors: None,
            errors_to_output: false,
        };
        let mut it = part.iter();
        while let Some(w) = it.next() {
            match w.as_str() {
                "2>&1" => cmd.errors_to_output = true,
                "<" | ">" | ">>" | "2>" | "2>>" => {
                    let file = it
                        .next()
                        .ok_or_else(|| alloc::format!("missing file name after '{}'", w))?
                        .clone();
                    match w.as_str() {
                        "<" => cmd.input = Some(file),
                        ">" | ">>" => cmd.output = Some((file, w == ">>")),
                        _ => cmd.errors = Some((file, w == "2>>")),
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

/// Puts the shell's own fds 0-2 back.
fn restore(saved: &[u64; 3]) {
    for (fd, &copy) in saved.iter().enumerate() {
        sys::dup2(copy, fd as u64);
    }
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

/// Whether the file starts with "#!" (a script).
fn is_script(path: &str) -> bool {
    let mut head = [0u8; 2];
    File::open(path).is_ok_and(|mut f| f.read(&mut head) == Ok(2) && &head == b"#!")
}

fn spawn_program(path: &str, args: &[String]) -> Stage {
    let name = String::from(path.rsplit('/').next().unwrap_or(path));
    let result = match process::spawn(path, args.iter().map(String::as_str)) {
        Err(fs::Error(ENOEXEC)) if is_script(path) => {
            // scripts run in a new shell: sh <script> <args>
            let shell_args = core::iter::once(path).chain(args.iter().map(String::as_str));
            process::spawn("/bin/sh", shell_args)
        }
        r => r,
    };
    match result {
        Ok(pid) => Stage::Spawned(name, pid),
        Err(e) => Stage::Failed(alloc::format!("{}: {}", path, e)),
    }
}

/// Waits for all children of a pipeline (first to last). The last one's exit
/// code and every kill are reported; after a Ctrl+C the rest is killed too.
/// Returns the status of the last one.
fn wait_all(children: &[(String, u64)]) -> i32 {
    let mut interrupted = false;
    let mut status = 0;
    for (i, (name, pid)) in children.iter().enumerate() {
        if interrupted {
            let _ = process::kill(*pid);
        }
        let last = i + 1 == children.len();
        match process::wait(*pid) {
            Ok(s) => {
                if last {
                    status = status_code(&s);
                }
                match s {
                    ExitStatus::Exited(0) => {}
                    ExitStatus::Exited(code) if last => error(&alloc::format!("{} exited with code {}", name, code)),
                    ExitStatus::Exited(_) => {}
                    ExitStatus::Killed(reason) => {
                        if reason == "interrupted" {
                            interrupted = true;
                        }
                        error(&alloc::format!("{} was killed: {}", name, reason))
                    }
                }
            }
            Err(e) => error(&alloc::format!("wait for {}: {}", name, e)),
        }
    }
    status
}

fn main(mut args: ulib::Args) -> i32 {
    match args.next() {
        None => Shell::new(alloc::vec![String::from("sh")]).interactive(),
        Some("-c") => {
            let line = args.next().unwrap_or("");
            let mut shell = Shell::new(core::iter::once("sh").chain(args).map(String::from).collect());
            shell.execute(line).unwrap_or(shell.status)
        }
        Some(script) => {
            let positional: Vec<String> = core::iter::once(script).chain(args).map(String::from).collect();
            let mut shell = Shell::new(positional);
            shell.script(script)
        }
    }
}
