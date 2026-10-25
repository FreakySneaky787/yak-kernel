//! /bin/sh: the shell as a user program. The kernel starts it as the first
//! process (init).
//!
//! - Line editing and word splitting reuse the kernel shell's code
//!   (`line_editor.rs`, `parser.rs` are compiled into this program).
//! - Grammar (`syntax.rs`): `;` and newlines separate commands, `a && b`,
//!   `a || b`, `! a`, pipelines, `if/elif/else/fi`, `while`/`until ... do
//!   ... done`, `for NAME in words; do ... done`, `{ ...; }`, functions
//!   `name() { ...; }` with `$1`.. and `return`. An unfinished command
//!   continues on the next line (prompt `> `); scripts are parsed as a whole.
//! - Built-ins: cd, pwd, echo, run, history, help, exit, export, unset, set,
//!   source (`.`), jobs, wait, fg, kill, true, false, `:`, test (`[`), read,
//!   break, continue, return, shift.
//! - Other words: a function, a kernel built-in (lspci, acpi, ...) run
//!   through the `kshell` system call, or a program path / a name found in
//!   `$PATH` (default `/bin`), started as a child. A file starting with `#!`
//!   runs as a shell script.
//! - Variables: `NAME=value`, `export NAME=value`, `unset NAME`; they live in
//!   the kernel's per-process environment, so children inherit them.
//!   `$NAME`, `${NAME}`, `$?` (last status), `$$`, `$#`, `$0`..`$9`, and
//!   `$((expression))` (`arith.rs`) are expanded when a command runs - not
//!   inside '...'; `\$` keeps a dollar. `#` at the start of a word begins a
//!   comment. Unquoted `*` and `?` match file names (`glob.rs`); without a
//!   match the word stays as it is.
//! - Pipelines and redirections: `a | b`, `> f`, `>> f`, `< f`, `2> f`,
//!   `2>> f`, `2>&1`, also after compound commands (`done < file`).
//!   Operators must be separate words. Before each stage, fds 0-2 are
//!   pointed at pipes/files with `dup2` (children inherit them), afterwards
//!   the shell's own are restored. Built-ins, functions and compound
//!   commands in a pipeline run inside the shell, one after the other, so
//!   what they write into a pipe must fit into it (64 KiB).
//! - Jobs: a command ending in `&` runs in the background (`[n] pid`); a
//!   compound or `&&`/`||` command does so in a shell of its own (`sh -c`).
//!   `jobs` lists them, `wait [%n]` waits, `fg [%n]` waits in the foreground
//!   (Ctrl+C then reaches it). Finished jobs are reported before the prompt.
//! - Ctrl+C: SIGINT (the shell has a handler). At the prompt it discards the
//!   line; while the shell runs a loop - or a child ended by Ctrl+C - the
//!   running command list stops (scripts exit with 130).
//! - Scripts: `sh file [args]`, `sh -c "commands"`, `source file`;
//!   `set -e` stops at the first failing command (not in conditions).

#![no_std]
#![no_main]

extern crate alloc;

mod arith;
mod glob;
mod keys;
#[allow(dead_code)]
#[path = "../../../../kernel/src/shell/line_editor.rs"]
mod line_editor;
#[path = "../../../../kernel/src/shell/parser.rs"]
mod parser;
mod syntax;
mod testcmd;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use line_editor::{EditResult, LineEditor};
use syntax::{AndOr, Command, Compound, List, Pipeline, SyntaxError, is_name};
use ulib::fs::{self, Dir, File};
use ulib::io::Stdout;
use ulib::process::{self, ExitStatus};
use ulib::{env, eprintln, io, print, println, signal, sys};

ulib::entry!(main);

const DEFAULT_PATH: &str = "/bin";
const ENOEXEC: i64 = 8;
/// Function calls nested deeper than this fail (the stack is limited).
const MAX_CALL_DEPTH: usize = 64;

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
    (
        "kill",
        "kill [-SIG] pid|%job... | kill -l",
        "send a signal (default TERM); -l lists the signals",
    ),
    (
        "test",
        "test expr (or [ expr ])",
        "compare strings/numbers, check files",
    ),
    ("read", "read [NAME...]", "read a line into variables (default REPLY)"),
    ("break", "break [n]", "leave the innermost (n-th) loop"),
    ("continue", "continue [n]", "next round of the innermost (n-th) loop"),
    ("return", "return [code]", "leave a function"),
    ("shift", "shift [n]", "drop the first n arguments ($1 ...)"),
    ("true", "true (or :)", "do nothing, successfully"),
    ("false", "false", "do nothing, unsuccessfully"),
];

/// Set by the SIGINT handler (Ctrl+C while the shell itself runs) and when a
/// foreground child was ended by Ctrl+C: the running command list stops.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_interrupt(_sig: u32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// How a command list ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Next,
    Exit(i32),
    Break(u32),
    Continue(u32),
    Return(i32),
    Interrupted,
}

struct Job {
    id: usize,
    command: String,
    /// (name, pid, final status once collected)
    procs: Vec<(String, u64, Option<ExitStatus>)>,
}

struct Shell {
    editor: LineEditor,
    decoder: keys::Decoder,
    /// Commands the kernel runs for us (`kshell`).
    kernel_commands: Vec<String>,
    /// Exit status of the last command (`$?`).
    status: i32,
    /// `$0`, `$1`, ... (script or function name and arguments)
    positional: Vec<String>,
    jobs: Vec<Job>,
    next_job: usize,
    exit_on_error: bool,
    functions: BTreeMap<String, Rc<List>>,
    /// > 0 while an `if`/`while` condition runs (`set -e` does not apply).
    condition_depth: usize,
    call_depth: usize,
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

/// `NAME=value` with a valid name.
fn assignment(word: &str) -> Option<(&str, &str)> {
    word.split_once('=').filter(|(n, _)| is_name(n))
}

/// The status a finished process leaves in `$?`.
fn status_code(status: &ExitStatus) -> i32 {
    status.code()
}

/// `n` for break/continue/shift: a positive number, default 1.
fn count_arg(args: &[String]) -> Result<u32, String> {
    match args.first() {
        None => Ok(1),
        Some(a) => a
            .parse::<u32>()
            .ok()
            .filter(|&n| n >= 1)
            .ok_or_else(|| alloc::format!("'{}' is not a positive number", a)),
    }
}

impl Shell {
    fn new(positional: Vec<String>) -> Shell {
        // Ctrl+C reaches us as SIGINT (instead of the raw byte or our end)
        let _ = signal::set_handler(signal::SIGINT, on_interrupt);
        Shell {
            editor: LineEditor::new(),
            decoder: keys::Decoder::new(),
            kernel_commands: kernel_commands(),
            status: 0,
            positional,
            jobs: Vec::new(),
            next_job: 1,
            exit_on_error: false,
            functions: BTreeMap::new(),
            condition_depth: 0,
            call_depth: 0,
        }
    }

    /// Reads one line with the line editor; `None` if it was cancelled (Ctrl+C).
    fn read_line(&mut self, prompt: &str) -> Option<String> {
        self.editor.start(prompt, &mut Stdout);
        loop {
            let mut buf = [0u8; 16];
            let n = if INTERRUPTED.swap(false, Ordering::SeqCst) {
                buf[0] = 0x03; // Ctrl+C arrived as a signal: the editor cancels the line
                1
            } else {
                sys::read(sys::STDIN, &mut buf)
            };
            if n <= 0 {
                continue;
            }
            for &byte in &buf[..n as usize] {
                let Some(event) = self.decoder.feed(byte) else { continue };
                let commands = &self.kernel_commands;
                let complete = |prefix: &str| completions(commands, prefix);
                match self.editor.handle(event, &mut Stdout, &complete) {
                    EditResult::Pending => {}
                    EditResult::Cancelled => return None,
                    EditResult::Line(line) => return Some(line),
                }
            }
        }
    }

    /// Reads and runs commands until `exit`; returns the exit code.
    fn interactive(&mut self) -> i32 {
        loop {
            self.report_finished_jobs();
            let prompt = alloc::format!("\x1b[92msh\x1b[0m:\x1b[94m{}\x1b[0m> ", cwd());
            let mut text = String::new();
            let list = loop {
                let Some(line) = self.read_line(if text.is_empty() { &prompt } else { "> " }) else {
                    break None;
                };
                text.push_str(&line);
                text.push('\n');
                match syntax::parse(&text) {
                    Err(SyntaxError::Incomplete) => continue, // more lines
                    Err(e) => {
                        error(&e.to_string());
                        self.status = 2;
                        break None;
                    }
                    Ok(list) => break Some(list),
                }
            };
            if let Some(list) = list {
                let flow = self.run_list(&list);
                INTERRUPTED.store(false, Ordering::SeqCst);
                if let Flow::Exit(code) = flow {
                    return code;
                }
            }
        }
    }

    /// Parses and runs a whole script.
    fn run_text(&mut self, text: &str) -> Flow {
        match syntax::parse(text) {
            Ok(list) => self.run_list(&list),
            Err(e) => {
                error(&e.to_string());
                self.status = 2;
                Flow::Exit(2)
            }
        }
    }

    /// The exit code of a script or `sh -c` that ended with `flow`.
    fn exit_code(&self, flow: Flow) -> i32 {
        match flow {
            Flow::Exit(code) => code,
            Flow::Interrupted => 130,
            _ => self.status,
        }
    }

    fn script(&mut self, path: &str) -> i32 {
        match io::read_input(Some(path)) {
            Ok(data) => {
                let text = String::from_utf8_lossy(&data).into_owned();
                let flow = self.run_text(&text);
                self.exit_code(flow)
            }
            Err(e) => {
                error(&alloc::format!("{}: {}", path, e));
                127
            }
        }
    }

    fn run_list(&mut self, list: &List) -> Flow {
        for and_or in list {
            if interrupted() {
                return Flow::Interrupted;
            }
            let flow = self.run_and_or(and_or);
            if flow != Flow::Next {
                return flow;
            }
        }
        Flow::Next
    }

    fn run_and_or(&mut self, and_or: &AndOr) -> Flow {
        if and_or.background {
            return self.run_background(and_or);
        }
        // all but the last pipeline are conditions (no `set -e` for them)
        let mut flow = self.run_pipeline(&and_or.first, None);
        let mut last_ran = and_or.rest.is_empty();
        for (i, (and, pipeline)) in and_or.rest.iter().enumerate() {
            if flow != Flow::Next {
                return flow;
            }
            if *and == (self.status == 0) {
                flow = self.run_pipeline(pipeline, None);
                last_ran = i + 1 == and_or.rest.len();
            }
        }
        let negated = and_or.rest.last().map_or(&and_or.first, |r| &r.1).negate;
        if flow == Flow::Next
            && last_ran
            && !negated
            && self.status != 0
            && self.exit_on_error
            && self.condition_depth == 0
        {
            return Flow::Exit(self.status);
        }
        flow
    }

    /// `command &`: a plain pipeline runs its programs as a job; anything
    /// else (compound commands, `&&`) runs in a shell of its own.
    fn run_background(&mut self, and_or: &AndOr) -> Flow {
        let plain = and_or.rest.is_empty()
            && !and_or.first.negate
            && and_or.first.commands.iter().all(|c| matches!(c, Command::Simple(_)));
        if plain {
            return self.run_pipeline(&and_or.first, Some(&and_or.text));
        }
        match process::spawn("/bin/sh", ["-c", and_or.text.as_str()]) {
            Ok(pid) => self.add_job(&and_or.text, alloc::vec![(String::from("sh"), pid)]),
            Err(e) => {
                error(&alloc::format!("/bin/sh: {}", e));
                self.status = 1;
            }
        }
        Flow::Next
    }

    fn add_job(&mut self, text: &str, children: Vec<(String, u64)>) {
        let id = self.next_job;
        self.next_job += 1;
        let pids: Vec<String> = children.iter().map(|(_, p)| alloc::format!("{}", p)).collect();
        println!("[{}] {}", id, pids.join(" "));
        self.jobs.push(Job {
            id,
            command: String::from(text),
            procs: children.into_iter().map(|(n, p)| (n, p, None)).collect(),
        });
        self.status = 0;
    }

    /// Runs the stages of a pipeline; with `background` (the job's text) the
    /// started programs become a job instead of being waited for.
    fn run_pipeline(&mut self, pipeline: &Pipeline, background: Option<&str>) -> Flow {
        let stages = &pipeline.commands;
        let saved = [0, 1, 2].map(|fd| sys::dup(fd) as u64);
        let mut prev_read: Option<u64> = None;
        let mut children: Vec<(String, u64)> = Vec::new();
        let mut flow = Flow::Next;
        let mut status = 0;
        let mut last_is_child = false;
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
            last_is_child = false;
            match self.run_command(stage) {
                Stage::Done(code) => status = code,
                Stage::Flow(f) => {
                    status = self.status;
                    if flow == Flow::Next {
                        flow = f;
                    }
                }
                Stage::Spawned(name, pid) => {
                    children.push((name, pid));
                    last_is_child = true;
                }
                Stage::Failed(e) => {
                    restore(&saved);
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
        if let Some(text) = background {
            if !children.is_empty() {
                self.add_job(text, children);
                return flow;
            }
        }
        if !children.is_empty() {
            let code = wait_all(&children);
            if last_is_child {
                status = code;
            }
        }
        if pipeline.negate {
            status = (status == 0) as i32;
        }
        self.status = status;
        flow
    }

    /// Expands raw words (variables, arithmetic), splits the result and
    /// replaces words with wildcards by the matching file names.
    fn expand_words(&self, raw: &[String]) -> Result<Vec<String>, String> {
        let expanded = self.expand(&raw.join(" "), true)?;
        let words = parser::split(&expanded).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for word in words {
            if glob::has_wildcards(&word) {
                let matches = glob::expand(&word, &cwd());
                if matches.is_empty() {
                    out.push(glob::literal(&word));
                } else {
                    out.extend(matches);
                }
            } else {
                out.push(word);
            }
        }
        Ok(out)
    }

    /// Runs one stage with the already prepared fds 0-2.
    fn run_command(&mut self, command: &Command) -> Stage {
        match command {
            Command::Simple(raw) => {
                let words = match self.expand_words(raw) {
                    Ok(w) => w,
                    Err(e) => return Stage::Failed(e),
                };
                if !words.is_empty() && words.iter().all(|w| assignment(w).is_some()) {
                    for (name, value) in words.iter().filter_map(|w| assignment(w)) {
                        env::set_var(name, value);
                    }
                    return Stage::Done(0);
                }
                let cmd = match parse_redirections(&words) {
                    Ok(c) => c,
                    Err(e) => return Stage::Failed(e),
                };
                if let Err(e) = cmd.redirect() {
                    return Stage::Failed(e);
                }
                if cmd.words.is_empty() {
                    return Stage::Done(0); // only redirections (`> file` empties it), or an empty `$VAR`
                }
                self.run_stage(&cmd.words)
            }
            Command::Compound(compound, raw) => {
                let redirections = match self.expand_words(raw).and_then(|w| parse_redirections(&w)) {
                    Ok(c) => c,
                    Err(e) => return Stage::Failed(e),
                };
                if let Some(w) = redirections.words.first() {
                    return Stage::Failed(alloc::format!("unexpected '{}' after a compound command", w));
                }
                if let Err(e) = redirections.redirect() {
                    return Stage::Failed(e);
                }
                match self.run_compound(compound) {
                    Flow::Next => Stage::Done(self.status),
                    flow => Stage::Flow(flow),
                }
            }
            Command::Function(name, body) => {
                self.functions.insert(name.clone(), body.clone());
                Stage::Done(0)
            }
        }
    }

    fn run_compound(&mut self, compound: &Compound) -> Flow {
        match compound {
            Compound::If { branches, otherwise } => {
                for (condition, body) in branches {
                    let flow = self.run_condition(condition);
                    if flow != Flow::Next {
                        return flow;
                    }
                    if self.status == 0 {
                        return self.run_list(body);
                    }
                }
                match otherwise {
                    Some(list) => self.run_list(list),
                    None => {
                        self.status = 0;
                        Flow::Next
                    }
                }
            }
            Compound::Loop { until, condition, body } => {
                let mut status = 0;
                loop {
                    if interrupted() {
                        return Flow::Interrupted;
                    }
                    let flow = self.run_condition(condition);
                    if flow != Flow::Next {
                        return flow;
                    }
                    if (self.status == 0) == *until {
                        break;
                    }
                    match self.loop_body(body) {
                        Ok(true) => status = self.status,
                        Ok(false) => break,
                        Err(flow) => return flow,
                    }
                }
                self.status = status;
                Flow::Next
            }
            Compound::For { var, items, body } => {
                let items = match items {
                    Some(raw) => match self.expand_words(raw) {
                        Ok(w) => w,
                        Err(e) => {
                            error(&e);
                            self.status = 1;
                            return Flow::Next;
                        }
                    },
                    None => self.positional.iter().skip(1).cloned().collect(),
                };
                let mut status = 0;
                for item in items {
                    if interrupted() {
                        return Flow::Interrupted;
                    }
                    env::set_var(var, &item);
                    match self.loop_body(body) {
                        Ok(true) => status = self.status,
                        Ok(false) => break,
                        Err(flow) => return flow,
                    }
                }
                self.status = status;
                Flow::Next
            }
            Compound::Group(list) => self.run_list(list),
        }
    }

    /// The condition of `if`/`while`: `set -e` does not apply inside.
    fn run_condition(&mut self, condition: &List) -> Flow {
        self.condition_depth += 1;
        let flow = self.run_list(condition);
        self.condition_depth -= 1;
        flow
    }

    /// One round of a loop: `Ok(true)` = go on, `Ok(false)` = `break`,
    /// `Err(flow)` = leave the loop with `flow` (outer break/continue, exit, ...).
    fn loop_body(&mut self, body: &List) -> Result<bool, Flow> {
        match self.run_list(body) {
            Flow::Next | Flow::Continue(1) => Ok(true),
            Flow::Break(1) => {
                self.status = 0;
                Ok(false)
            }
            Flow::Break(n) => Err(Flow::Break(n - 1)),
            Flow::Continue(n) => Err(Flow::Continue(n - 1)),
            flow => Err(flow),
        }
    }

    /// Replaces `$...` outside single quotes and drops `#` comments. With
    /// `glob`, unquoted `*` and `?` become `glob::STAR` / `glob::ANY`.
    fn expand(&self, line: &str, glob: bool) -> Result<String, String> {
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
                '$' if chars.get(i + 1) == Some(&'(') && chars.get(i + 2) == Some(&'(') => {
                    let end = arith_end(&chars, i + 3).ok_or("missing '))' after '$(('")?;
                    let inner: String = chars[i + 3..end].iter().collect();
                    let value = arith::eval(&self.expand(&inner, false)?, &|name| self.value(name))?;
                    out.push_str(&alloc::format!("{}", value));
                    i = end + 1; // the second ')'
                }
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
                '*' if glob && !double => out.push(glob::STAR),
                '?' if glob && !double => out.push(glob::ANY),
                _ => out.push(c),
            }
            word_start = !single && !double && (c.is_whitespace() || (word_start && c == '#'));
            i += 1;
        }
        Ok(out)
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

    /// Runs one simple command with the already redirected fds.
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
            "true" | ":" => {}
            "false" => return Stage::Done(1),
            "history" => {
                for (i, l) in self.editor.history().iter().enumerate() {
                    println!("{:4}  {}", i + 1, l);
                }
            }
            "help" => self.help(),
            "exit" => {
                let code = args.first().and_then(|c| c.parse().ok()).unwrap_or(self.status);
                return Stage::Flow(Flow::Exit(code));
            }
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
                        return match self.run_text(&text) {
                            Flow::Next | Flow::Break(_) | Flow::Continue(_) => Stage::Done(self.status),
                            Flow::Return(code) => Stage::Done(code),
                            flow => Stage::Flow(flow),
                        };
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
            "kill" => return self.kill(args),
            "test" | "[" => {
                return match testcmd::run(name, args) {
                    Ok(code) => Stage::Done(code),
                    Err(e) => {
                        error(&e);
                        Stage::Done(2)
                    }
                };
            }
            "read" => return Stage::Done(self.read(args)),
            "break" | "continue" => {
                return match count_arg(args) {
                    Ok(n) if name == "break" => Stage::Flow(Flow::Break(n)),
                    Ok(n) => Stage::Flow(Flow::Continue(n)),
                    Err(e) => Stage::Failed(alloc::format!("{}: {}", name, e)),
                };
            }
            "return" => {
                let code = args.first().and_then(|c| c.parse().ok()).unwrap_or(self.status);
                self.status = code;
                return Stage::Flow(Flow::Return(code));
            }
            "shift" => match count_arg(args) {
                Ok(n) if (n as usize) < self.positional.len() => {
                    self.positional.drain(1..1 + n as usize);
                }
                Ok(_) => return Stage::Done(1),
                Err(e) => return Stage::Failed(alloc::format!("shift: {}", e)),
            },
            "run" => match args.first() {
                Some(path) => return spawn_program(path, &args[1..]),
                None => return Stage::Failed(String::from("usage: run <program> [args...]")),
            },
            _ if self.functions.contains_key(name) => return self.call(name, args),
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

    /// Calls a shell function: `$0` = its name, `$1`... = `args`.
    fn call(&mut self, name: &str, args: &[String]) -> Stage {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Stage::Failed(alloc::format!("{}: functions nested too deeply", name));
        }
        let body = self.functions[name].clone();
        let positional = core::iter::once(name)
            .chain(args.iter().map(String::as_str))
            .map(String::from)
            .collect();
        let saved = core::mem::replace(&mut self.positional, positional);
        self.call_depth += 1;
        let flow = self.run_list(&body);
        self.call_depth -= 1;
        self.positional = saved;
        match flow {
            Flow::Return(code) => Stage::Done(code),
            Flow::Exit(_) | Flow::Interrupted => Stage::Flow(flow),
            _ => Stage::Done(self.status),
        }
    }

    /// `read [NAME...]`: one line from stdin, split at whitespace; the last
    /// name gets the rest of the line. Status 1 at the end of the input.
    fn read(&mut self, names: &[String]) -> i32 {
        let mut buf = [0u8; 4096];
        let line = match io::read_line(&mut buf) {
            Some(line) => String::from(line),
            None if interrupted() => return 130,
            None => return 1,
        };
        let reply = [String::from("REPLY")];
        let names = if names.is_empty() { &reply[..] } else { names };
        let mut rest = line.trim_start();
        for (i, name) in names.iter().enumerate() {
            if !is_name(name) {
                error(&alloc::format!("read: '{}' is not a valid name", name));
                return 2;
            }
            let value = if i + 1 == names.len() {
                core::mem::take(&mut rest).trim_end()
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let (word, tail) = rest.split_at(end);
                rest = tail.trim_start();
                word
            };
            env::set_var(name, value);
        }
        if interrupted() { 130 } else { 0 }
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

    /// `kill [-SIG | -s SIG] pid|%job...`, `kill -l`.
    fn kill(&mut self, args: &[String]) -> Stage {
        let mut args = args;
        if args.first().is_some_and(|a| a == "-l") {
            for (name, num) in signal::NAMES {
                println!("{:2} {}", num, name);
            }
            return Stage::Done(0);
        }
        let mut sig = signal::SIGTERM;
        if let Some(first) = args.first().filter(|a| a.starts_with('-')) {
            let (name, rest) = if first == "-s" {
                (
                    args.get(1).map(String::as_str).unwrap_or(""),
                    &args[2.min(args.len())..],
                )
            } else {
                (&first[1..], &args[1..])
            };
            match signal::parse(name) {
                Some(s) => sig = s,
                None => return Stage::Failed(alloc::format!("kill: unknown signal '{}'", name)),
            }
            args = rest;
        }
        if args.is_empty() {
            return Stage::Failed(String::from("usage: kill [-SIG] pid|%job..."));
        }
        let mut status = 0;
        for target in args {
            let pids: Vec<u64> = match target.strip_prefix('%') {
                Some(n) => match self.jobs.iter().find(|j| n.parse() == Ok(j.id)) {
                    Some(job) => job.procs.iter().filter(|p| p.2.is_none()).map(|p| p.1).collect(),
                    None => {
                        error(&alloc::format!("kill: no job {}", target));
                        status = 1;
                        continue;
                    }
                },
                None => match target.parse() {
                    Ok(pid) => alloc::vec![pid],
                    Err(_) => {
                        error(&alloc::format!("kill: '{}' is not a pid or %job", target));
                        status = 1;
                        continue;
                    }
                },
            };
            for pid in pids {
                if let Err(e) = signal::send(pid, sig) {
                    error(&alloc::format!("kill {}: {}", pid, e));
                    status = 1;
                }
            }
        }
        Stage::Done(status)
    }

    fn help(&self) {
        println!("Shell built-ins (/bin/sh):");
        for (_, usage, help) in BUILTINS {
            println!("  \x1b[96m{:<34}\x1b[0m {}", usage, help);
        }
        println!("Shell syntax: a; b   a && b   a || b   ! a   a | b   a &   if/elif/else/fi");
        println!("  while/until ... do ... done   for x in ...; do ... done   f() {{ ...; }}   $((1 + 2))");
        println!("Kernel commands:");
        sys::kshell("help");
        print!("Programs in $PATH:");
        for p in programs() {
            print!(" {}", p);
        }
        println!();
    }
}

/// Index of the first ')' of the "))" that closes a `$((` whose contents
/// start at `start`.
fn arith_end(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            ')' if chars.get(i + 1) == Some(&')') => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
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
    /// A built-in changes the control flow (exit, break, return, ...).
    Flow(Flow),
    Spawned(String, u64),
    Failed(String),
}

/// The words of one command with its redirections taken out.
struct Redirected {
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

impl Redirected {
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

/// Pulls the redirection operators (and their files) out of `words`.
fn parse_redirections(words: &[String]) -> Result<Redirected, String> {
    let mut cmd = Redirected {
        words: Vec::new(),
        input: None,
        output: None,
        errors: None,
        errors_to_output: false,
    };
    let mut it = words.iter();
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
    Ok(cmd)
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
/// code and every kill are reported; after a Ctrl+C the rest is killed too
/// and the shell stops what it is running (`INTERRUPTED`). Returns the
/// status of the last one.
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
                    ExitStatus::Killed(reason, sig) => {
                        if sig == signal::SIGINT {
                            interrupted = true;
                        }
                        error(&alloc::format!("{} was killed: {}", name, reason))
                    }
                }
            }
            Err(e) => error(&alloc::format!("wait for {}: {}", name, e)),
        }
    }
    if interrupted {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    status
}

fn main(mut args: ulib::Args) -> i32 {
    match args.next() {
        None => Shell::new(alloc::vec![String::from("sh")]).interactive(),
        Some("-c") => {
            let text = args.next().unwrap_or("");
            let mut shell = Shell::new(core::iter::once("sh").chain(args).map(String::from).collect());
            let flow = shell.run_text(text);
            shell.exit_code(flow)
        }
        Some(script) => {
            let positional: Vec<String> = core::iter::once(script).chain(args).map(String::from).collect();
            let mut shell = Shell::new(positional);
            shell.script(script)
        }
    }
}
