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
pub const CHDIR: u64 = 18;
pub const KSHELL: u64 = 19;
pub const KCOMMANDS: u64 = 20;
pub const PIPE: u64 = 21;
pub const DUP: u64 = 22;
pub const DUP2: u64 = 23;
pub const ISATTY: u64 = 24;
pub const SYSINFO: u64 = 25;
pub const PROCLIST: u64 = 26;
pub const RENAME: u64 = 27;
pub const SEEK: u64 = 28;
pub const TERMSIZE: u64 = 29;
pub const IGNORE_INTERRUPT: u64 = 30;
pub const NET_INFO: u64 = 31;
pub const PING: u64 = 32;
pub const UDP_SOCKET: u64 = 33;
pub const SENDTO: u64 = 34;
pub const RECVFROM: u64 = 35;

pub const STDIN: u64 = 0;
pub const STDOUT: u64 = 1;
pub const STDERR: u64 = 2;

/// # Safety
/// The arguments must be valid for system call `nr`.
pub unsafe fn syscall5(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as i64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8") a4,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        )
    };
    ret
}

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
/// Every write goes to the end of the file.
pub const O_APPEND: u64 = 16;

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

pub fn chdir(path: &str) -> i64 {
    unsafe { syscall3(CHDIR, path.as_ptr() as u64, path.len() as u64, 0) }
}

/// Runs a kernel shell built-in (transitional, see the kernel's `nr::KSHELL`):
/// 0 = ok, 1 = the command failed, -ENOENT = no such built-in.
pub fn kshell(line: &str) -> i64 {
    unsafe { syscall3(KSHELL, line.as_ptr() as u64, line.len() as u64, 0) }
}

/// NUL-separated names of the kernel built-ins usable with `kshell`.
pub fn kcommands(buf: &mut [u8]) -> i64 {
    unsafe { syscall3(KCOMMANDS, buf.as_mut_ptr() as u64, buf.len() as u64, 0) }
}

/// Creates a pipe: `fds[0]` reads, `fds[1]` writes.
pub fn pipe(fds: &mut [u32; 2]) -> i64 {
    unsafe { syscall3(PIPE, fds.as_mut_ptr() as u64, 0, 0) }
}

/// A new descriptor for the same object as `fd`.
pub fn dup(fd: u64) -> i64 {
    unsafe { syscall3(DUP, fd, 0, 0) }
}

/// Makes `new_fd` refer to the same object as `fd` (closing what was there).
pub fn dup2(fd: u64, new_fd: u64) -> i64 {
    unsafe { syscall3(DUP2, fd, new_fd, 0) }
}

/// Whether `fd` is the console (keyboard/screen) rather than a file or pipe.
pub fn isatty(fd: u64) -> bool {
    unsafe { syscall3(ISATTY, fd, 0, 0) == 1 }
}

/// Same layout as the kernel's `process::syscall::SysInfo`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SysInfo {
    pub uptime_ms: u64,
    pub ticks: u64,
    pub mem_usable: u64,
    pub mem_free: u64,
    pub heap_total: u64,
    pub heap_used: u64,
    pub fs_total: u64,
    pub fs_free: u64,
    pub fs_cluster_size: u32,
    pub timer_hz: u32,
    pub cpus: u32,
    pub _pad: u32,
    /// year, month, day, hour, minute, second
    pub date: [u16; 6],
    pub _pad2: u32,
    pub fs_label: [u8; 16],
    pub cpu_vendor: [u8; 16],
    pub cpu_brand: [u8; 48],
}

/// Same layout as the kernel's `process::syscall::ProcInfo`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcInfo {
    pub tid: u64,
    pub cpu_ticks: u64,
    pub parent: u64,
    pub state: u32,
    pub user: u32,
    pub name: [u8; 32],
}

pub const PROC_RUNNING: u32 = 0;
pub const PROC_READY: u32 = 1;
pub const PROC_SLEEPING: u32 = 2;
pub const PROC_WAIT_INPUT: u32 = 3;
pub const PROC_JOIN: u32 = 4;
pub const PROC_WAIT_PIPE: u32 = 5;
pub const PROC_DEAD: u32 = 6;
pub const PROC_WAIT_NET: u32 = 7;

const _: () = assert!(core::mem::size_of::<SysInfo>() == 176 && core::mem::size_of::<ProcInfo>() == 64);

/// A zero-terminated (or completely filled) byte field as text.
pub fn field_str(bytes: &[u8]) -> &str {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..len]).unwrap_or("?")
}

pub fn sysinfo() -> SysInfo {
    let mut info: SysInfo = unsafe { core::mem::zeroed() };
    unsafe { syscall3(SYSINFO, &mut info as *mut SysInfo as u64, 0, 0) };
    info
}

/// Fills `out` with one entry per thread; returns how many were written.
pub fn proclist(out: &mut [ProcInfo]) -> usize {
    let n = unsafe { syscall3(PROCLIST, out.as_mut_ptr() as u64, out.len() as u64, 0) };
    n.max(0) as usize
}

pub fn rename(from: &str, to: &str) -> i64 {
    unsafe {
        syscall4(
            RENAME,
            from.as_ptr() as u64,
            from.len() as u64,
            to.as_ptr() as u64,
            to.len() as u64,
        )
    }
}

pub const SEEK_SET: u64 = 0;
pub const SEEK_CUR: u64 = 1;
pub const SEEK_END: u64 = 2;

/// Moves the read/write position of a file; returns the new offset or -errno.
pub fn seek(fd: u64, offset: i64, whence: u64) -> i64 {
    unsafe { syscall3(SEEK, fd, offset as u64, whence) }
}

/// Console size: rows << 32 | columns.
pub fn termsize() -> u64 {
    unsafe { syscall3(TERMSIZE, 0, 0, 0) as u64 }
}

/// With `true`, Ctrl+C arrives as byte 0x03 instead of killing the process.
pub fn ignore_interrupt(on: bool) {
    unsafe { syscall3(IGNORE_INTERRUPT, on as u64, 0, 0) };
}

/// Same layout as the kernel's `process::syscall::NetInfo`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NetInfo {
    pub mac: [u8; 6],
    pub link_up: u8,
    pub _pad: u8,
    pub ip: [u8; 4],
    pub netmask: [u8; 4],
    pub gateway: [u8; 4],
    pub _pad2: u32,
    pub rx_frames: u64,
    pub tx_frames: u64,
}

/// Same layout as the kernel's `process::syscall::SockAddr`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SockAddr {
    pub ip: [u8; 4],
    pub port: u16,
    pub _pad: u16,
}

const _: () = assert!(core::mem::size_of::<NetInfo>() == 40 && core::mem::size_of::<SockAddr>() == 8);

/// 0 or -ENODEV (no network card).
pub fn net_info(info: &mut NetInfo) -> i64 {
    unsafe { syscall3(NET_INFO, info as *mut NetInfo as u64, 0, 0) }
}

/// One ICMP echo; returns the round trip time in ms or -errno.
pub fn ping(ip: [u8; 4], seq: u16, timeout_ms: u64) -> i64 {
    unsafe { syscall3(PING, u32::from_be_bytes(ip) as u64, seq as u64, timeout_ms) }
}

/// A UDP socket bound to `port` (0 = any free port); returns the fd or -errno.
pub fn udp_socket(port: u16) -> i64 {
    unsafe { syscall3(UDP_SOCKET, port as u64, 0, 0) }
}

pub fn sendto(fd: u64, data: &[u8], ip: [u8; 4], port: u16) -> i64 {
    unsafe {
        syscall5(
            SENDTO,
            fd,
            data.as_ptr() as u64,
            data.len() as u64,
            u32::from_be_bytes(ip) as u64,
            port as u64,
        )
    }
}

/// Waits up to `timeout_ms` for a datagram; returns its length or -errno (-110 = timed out).
pub fn recvfrom(fd: u64, buf: &mut [u8], from: &mut SockAddr, timeout_ms: u64) -> i64 {
    unsafe {
        syscall5(
            RECVFROM,
            fd,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            from as *mut SockAddr as u64,
            timeout_ms,
        )
    }
}

/// "10.0.2.2" -> [10, 0, 2, 2]
pub fn parse_ip(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for b in &mut out {
        *b = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}
