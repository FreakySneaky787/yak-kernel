//! Child processes: `spawn` a program, `wait` for it, `kill` it.

use crate::fs::Error;
use crate::sys::{self, WaitStatus};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitStatus {
    Exited(i64),
    /// Ended by the kernel; the reason (e.g. "page fault at 0x0 ...", "interrupted").
    Killed(String),
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExitStatus::Exited(code) => write!(f, "exited with code {}", code),
            ExitStatus::Killed(reason) => write!(f, "killed: {}", reason),
        }
    }
}

fn check(ret: i64) -> Result<u64, Error> {
    if ret < 0 { Err(Error(-ret)) } else { Ok(ret as u64) }
}

/// Starts the program at `path` with `args`; returns its pid. The child
/// inherits the current directory and the output.
pub fn spawn<'a>(path: &str, args: impl IntoIterator<Item = &'a str>) -> Result<u64, Error> {
    let mut block: Vec<u8> = Vec::new();
    for (i, arg) in args.into_iter().enumerate() {
        if i > 0 {
            block.push(0);
        }
        block.extend_from_slice(arg.as_bytes());
    }
    check(sys::spawn(path, &block))
}

/// Waits until child `pid` has ended.
pub fn wait(pid: u64) -> Result<ExitStatus, Error> {
    let mut st = WaitStatus {
        kind: 0,
        reason_len: 0,
        code: 0,
        reason: [0; 112],
    };
    check(sys::wait(pid, &mut st))?;
    Ok(if st.kind == sys::WAIT_KILLED {
        let reason = &st.reason[..st.reason_len as usize];
        ExitStatus::Killed(String::from(core::str::from_utf8(reason).unwrap_or("?")))
    } else {
        ExitStatus::Exited(st.code)
    })
}

/// Asks child `pid` to end (it is woken up if it is blocked).
pub fn kill(pid: u64) -> Result<(), Error> {
    check(sys::kill(pid)).map(|_| ())
}

/// `spawn` + `wait`.
pub fn run<'a>(path: &str, args: impl IntoIterator<Item = &'a str>) -> Result<ExitStatus, Error> {
    wait(spawn(path, args)?)
}
