//! System calls. Numbers and conventions must match the kernel
//! (`kernel/src/process/syscall.rs`): number in RAX, arguments in RDI, RSI,
//! RDX; result in RAX (negative = -errno). RCX and R11 are clobbered.

use core::arch::asm;

pub const EXIT: u64 = 0;
pub const WRITE: u64 = 1;
pub const READ: u64 = 2;
pub const SLEEP_MS: u64 = 3;
pub const GETPID: u64 = 4;
pub const UPTIME_MS: u64 = 5;
pub const YIELD: u64 = 6;

pub const STDIN: u64 = 0;
pub const STDOUT: u64 = 1;
pub const STDERR: u64 = 2;

/// # Safety
/// The arguments must be valid for system call `nr`.
pub unsafe fn syscall3(nr: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as i64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        )
    };
    ret
}

pub fn exit(code: i64) -> ! {
    unsafe { syscall3(EXIT, code as u64, 0, 0) };
    unreachable!("exit returned")
}

pub fn write(fd: u64, buf: &[u8]) -> i64 {
    unsafe { syscall3(WRITE, fd, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Blocks until at least one byte is available.
pub fn read(fd: u64, buf: &mut [u8]) -> i64 {
    unsafe { syscall3(READ, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

pub fn sleep_ms(ms: u64) {
    unsafe { syscall3(SLEEP_MS, ms, 0, 0) };
}

pub fn getpid() -> u64 {
    unsafe { syscall3(GETPID, 0, 0, 0) as u64 }
}

pub fn uptime_ms() -> u64 {
    unsafe { syscall3(UPTIME_MS, 0, 0, 0) as u64 }
}

pub fn yield_now() {
    unsafe { syscall3(YIELD, 0, 0, 0) };
}
