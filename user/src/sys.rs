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
pub const OPEN: u64 = 7;
pub const CLOSE: u64 = 8;
pub const READDIR: u64 = 9;
pub const STAT: u64 = 10;
pub const GETCWD: u64 = 11;
pub const UNLINK: u64 = 12;
pub const MKDIR: u64 = 13;
pub const BRK: u64 = 14;
pub const SPAWN: u64 = 15;
pub const WAIT: u64 = 16;
pub const KILL: u64 = 17;

pub const STDIN: u64 = 0;
pub const STDOUT: u64 = 1;
pub const STDERR: u64 = 2;

/// # Safety
/// The arguments must be valid for system call `nr`.
pub unsafe fn syscall4(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as i64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        )
    };
    ret
}

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

/// `open` flags (see the kernel's `open_flags`).
pub const O_READ: u64 = 1;
/// Writes always append to the end of the file.
pub const O_WRITE: u64 = 2;
pub const O_CREATE: u64 = 4;
pub const O_TRUNCATE: u64 = 8;

pub const KIND_FILE: u32 = 1;
pub const KIND_DIR: u32 = 2;

/// Same layout as the kernel's `process::syscall::Stat`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat {
    pub size: u64,
    pub kind: u32,
    pub _reserved: u32,
    /// year, month, day, hour, minute, second
    pub modified: [u16; 6],
    pub _pad: u32,
}

/// Same layout as the kernel's `process::syscall::DirEntryInfo`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirEntryInfo {
    pub stat: Stat,
    pub name_len: u32,
    pub _pad: u32,
    pub name: [u8; 256],
}

const _: () = assert!(core::mem::size_of::<Stat>() == 32 && core::mem::size_of::<DirEntryInfo>() == 296);

/// Returns a file descriptor or -errno.
pub fn open(path: &str, flags: u64) -> i64 {
    unsafe { syscall3(OPEN, path.as_ptr() as u64, path.len() as u64, flags) }
}

pub fn close(fd: u64) -> i64 {
    unsafe { syscall3(CLOSE, fd, 0, 0) }
}

/// 1 = `info` filled, 0 = end of the directory, < 0 = -errno.
pub fn readdir(fd: u64, info: &mut DirEntryInfo) -> i64 {
    unsafe { syscall3(READDIR, fd, info as *mut DirEntryInfo as u64, 0) }
}

pub fn stat(path: &str, stat: &mut Stat) -> i64 {
    unsafe { syscall3(STAT, path.as_ptr() as u64, path.len() as u64, stat as *mut Stat as u64) }
}

/// Length of the path written to `buf`, or -errno.
pub fn getcwd(buf: &mut [u8]) -> i64 {
    unsafe { syscall3(GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0) }
}

/// Removes a file or an empty directory.
pub fn unlink(path: &str) -> i64 {
    unsafe { syscall3(UNLINK, path.as_ptr() as u64, path.len() as u64, 0) }
}

pub fn mkdir(path: &str) -> i64 {
    unsafe { syscall3(MKDIR, path.as_ptr() as u64, path.len() as u64, 0) }
}

/// Moves the end of the heap to `new_end` (0 = just query). Returns the end
/// now in effect, or -ENOMEM.
pub fn brk(new_end: u64) -> i64 {
    unsafe { syscall3(BRK, new_end, 0, 0) }
}

pub const WAIT_EXITED: u32 = 1;
pub const WAIT_KILLED: u32 = 2;

/// Same layout as the kernel's `process::syscall::WaitStatus`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WaitStatus {
    pub kind: u32,
    pub reason_len: u32,
    pub code: i64,
    pub reason: [u8; 112],
}

const _: () = assert!(core::mem::size_of::<WaitStatus>() == 128);

/// Starts `path` with the NUL-separated argument block `args`; returns the pid or -errno.
pub fn spawn(path: &str, args: &[u8]) -> i64 {
    unsafe {
        syscall4(
            SPAWN,
            path.as_ptr() as u64,
            path.len() as u64,
            args.as_ptr() as u64,
            args.len() as u64,
        )
    }
}

/// Waits for child `pid`; returns `pid` or -errno.
pub fn wait(pid: u64, status: &mut WaitStatus) -> i64 {
    unsafe { syscall3(WAIT, pid, status as *mut WaitStatus as u64, 0) }
}

pub fn kill(pid: u64) -> i64 {
    unsafe { syscall3(KILL, pid, 0, 0) }
}
