//! System call dispatcher: called by the assembly stub in
//! `arch::x86_64::syscall` with the saved user registers.
//!
//! Errors are returned as negative numbers (`-errno`), like Linux does.
//! Every pointer from user space is checked against the process's page
//! tables before the kernel touches it.

use super::{ExitStatus, Output, with_current};
use crate::arch::x86_64::syscall::SyscallFrame;
use crate::arch::x86_64::without_interrupts;
use crate::drivers::scancode::{Key, KeyEvent};
use crate::mem::address_space::USER_END;
use crate::{input, task, time};
use alloc::string::String;
use alloc::vec::Vec;

pub mod nr {
    /// exit(code) -> never returns
    pub const EXIT: u32 = 0;
    /// write(fd, buf, len) -> bytes written (fd 1 = stdout, 2 = stderr)
    pub const WRITE: u32 = 1;
    /// read(fd, buf, len) -> bytes read (fd 0 = keyboard/serial, blocks)
    pub const READ: u32 = 2;
    /// sleep_ms(ms) -> 0
    pub const SLEEP_MS: u32 = 3;
    /// getpid() -> pid
    pub const GETPID: u32 = 4;
    /// uptime_ms() -> milliseconds since boot
    pub const UPTIME_MS: u32 = 5;
    /// yield() -> 0
    pub const YIELD: u32 = 6;
}

pub mod errno {
    pub const EBADF: i64 = 9;
    pub const EFAULT: i64 = 14;
    pub const ENOSYS: i64 = 38;
}

type SysResult = Result<u64, i64>;

#[unsafe(no_mangle)]
pub extern "C" fn dispatch(frame: &mut SyscallFrame) {
    // `sysret` to a non-canonical address would fault in ring 0
    if frame.rip >= USER_END {
        super::kill_current(String::from("syscall from a non-canonical address"));
    }
    let (a0, a1, a2) = (frame.rdi, frame.rsi, frame.rdx);
    let result = match frame.rax as u32 {
        _ if frame.rax > u32::MAX as u64 => Err(errno::ENOSYS),
        nr::EXIT => super::exit_current(ExitStatus::Exited(a0 as i64)),
        nr::WRITE => sys_write(a0, a1, a2),
        nr::READ => sys_read(a0, a1, a2),
        nr::SLEEP_MS => {
            time::sleep_ms(a0);
            Ok(0)
        }
        nr::GETPID => Ok(task::current_id()),
        nr::UPTIME_MS => Ok(time::uptime_ms()),
        nr::YIELD => {
            task::yield_now();
            Ok(0)
        }
        _ => Err(errno::ENOSYS),
    };
    frame.rax = match result {
        Ok(v) => v,
        Err(e) => (-e) as u64,
    };
    super::check_killed();
}

/// Whether the current process may access `[addr, addr + len)`.
fn accessible(addr: u64, len: u64, writable: bool) -> bool {
    with_current(|p| p.space.as_ref().is_some_and(|s| s.is_accessible(addr, len, writable))).unwrap_or(false)
}

fn sys_write(fd: u64, buf: u64, len: u64) -> SysResult {
    if fd != 1 && fd != 2 {
        return Err(errno::EBADF);
    }
    if !accessible(buf, len, false) {
        return Err(errno::EFAULT);
    }
    // The process's page tables are active: the user buffer is directly readable
    let data: Vec<u8> = unsafe { core::slice::from_raw_parts(buf as *const u8, len as usize) }.to_vec();
    match with_current(|p| p.stdout.clone()) {
        Some(Output::Capture(sink)) => without_interrupts(|| sink.lock().extend_from_slice(&data)),
        _ => crate::print!("{}", String::from_utf8_lossy(&data)),
    }
    Ok(len)
}

/// The bytes a key press produces on stdin (like a terminal would send).
fn key_bytes(event: KeyEvent, out: &mut Vec<u8>) {
    match event.key {
        Key::Char(c) if event.ctrl && c.is_ascii_alphabetic() => out.push(c.to_ascii_lowercase() as u8 & 0x1F),
        Key::Char(c) => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        Key::Enter => out.push(b'\n'),
        Key::Backspace => out.push(0x08),
        Key::Tab => out.push(b'\t'),
        Key::Escape => out.push(0x1B),
        _ => {}
    }
}

fn sys_read(fd: u64, buf: u64, len: u64) -> SysResult {
    if fd != 0 {
        return Err(errno::EBADF);
    }
    if !accessible(buf, len, true) {
        return Err(errno::EFAULT);
    }
    if len == 0 {
        return Ok(0);
    }
    let mut bytes = with_current(|p| core::mem::take(&mut p.stdin_pending)).unwrap_or_default();
    while bytes.is_empty() {
        key_bytes(input::read_key(), &mut bytes); // blocks until a key arrives
    }
    let n = bytes.len().min(len as usize);
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n) };
    let rest = bytes.split_off(n);
    with_current(|p| p.stdin_pending = rest);
    Ok(n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn keys_become_terminal_bytes() {
        let ev = |key, ctrl| KeyEvent { key, ctrl, alt: false };
        let mut out = Vec::new();
        for e in [
            ev(Key::Char('a'), false),
            ev(Key::Char('ä'), false),
            ev(Key::Char('c'), true),
            ev(Key::Enter, false),
            ev(Key::Left, false),
            ev(Key::Backspace, false),
        ] {
            key_bytes(e, &mut out);
        }
        assert_eq!(out, b"a\xC3\xA4\x03\n\x08");
    }
}
