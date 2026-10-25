//! System call dispatcher: called by the assembly stub in
//! `arch::x86_64::syscall` with the saved user registers.
//!
//! Errors are returned as negative numbers (`-errno`), like Linux does.
//! Every pointer from user space is checked against the process's page
//! tables before the kernel touches it.
//!
//! File system calls work on the mounted FAT32 root. The process table lock
//! is never held during disk I/O: handlers copy what they need out of the
//! file descriptor, do the I/O, and update the descriptor afterwards.

use super::pipe::{self, PipeEnd, PipeError};
use super::{ExitStatus, Handle, MAX_FDS, Output, with_current};
use crate::arch::x86_64::syscall::SyscallFrame;
use crate::arch::x86_64::without_interrupts;
use crate::drivers::scancode::{Key, KeyEvent};
use crate::fs::{self, fat32::DirEntry, fat32::FsError};
use crate::mem::address_space::USER_END;
use crate::net::{self, NetError, tcp};
use crate::{input, task, time};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::size_of;
use core::sync::atomic::{AtomicU64, Ordering};

pub mod nr {
    /// exit(code) -> never returns
    pub const EXIT: u32 = 0;
    /// write(fd, buf, len) -> bytes written (fd 1 = stdout, 2 = stderr)
    pub const WRITE: u32 = 1;
    /// read(fd, buf, len) -> bytes read (fd 0 = keyboard/serial, blocks)
    pub const READ: u32 = 2;
    /// sleep_ms(ms) -> 0, or -EINTR when a signal arrives
    pub const SLEEP_MS: u32 = 3;
    /// getpid() -> pid
    pub const GETPID: u32 = 4;
    /// uptime_ms() -> milliseconds since boot
    pub const UPTIME_MS: u32 = 5;
    /// yield() -> 0
    pub const YIELD: u32 = 6;
    /// open(path, path_len, flags) -> fd
    pub const OPEN: u32 = 7;
    /// close(fd) -> 0
    pub const CLOSE: u32 = 8;
    /// readdir(fd, *mut DirEntryInfo) -> 1 (entry written) or 0 (end of directory)
    pub const READDIR: u32 = 9;
    /// stat(path, path_len, *mut Stat) -> 0
    pub const STAT: u32 = 10;
    /// getcwd(buf, len) -> length of the path
    pub const GETCWD: u32 = 11;
    /// unlink(path, path_len) -> 0 (files and empty directories)
    pub const UNLINK: u32 = 12;
    /// mkdir(path, path_len) -> 0
    pub const MKDIR: u32 = 13;
    /// brk(new_end) -> the heap end now in effect; 0 only queries it
    pub const BRK: u32 = 14;
    /// spawn(path, path_len, args, args_len) -> pid of the new child
    pub const SPAWN: u32 = 15;
    /// wait(pid, *mut WaitStatus or 0, flags) -> pid, or 0 with flags & WNOHANG while it runs (children only)
    pub const WAIT: u32 = 16;
    /// kill(pid, sig) -> 0: sends signal `sig` (0 = only check that `pid` exists)
    pub const KILL: u32 = 17;
    /// chdir(path, path_len) -> 0
    pub const CHDIR: u32 = 18;
    /// kshell(line, line_len) -> 0 (ok) / 1 (the command failed); runs a kernel
    /// shell built-in in the caller's cwd with the caller's output. Transitional:
    /// gives /bin/sh the kernel's information commands until they get real syscalls.
    pub const KSHELL: u32 = 19;
    /// kcommands(buf, len) -> length: NUL-separated names usable with `kshell`
    pub const KCOMMANDS: u32 = 20;
    /// pipe(*mut [u32; 2]) -> 0; fds[0] = read end, fds[1] = write end
    pub const PIPE: u32 = 21;
    /// dup(fd) -> new fd for the same object (the lowest free one)
    pub const DUP: u32 = 22;
    /// dup2(fd, new_fd) -> new_fd (whatever was at new_fd is closed)
    pub const DUP2: u32 = 23;
    /// isatty(fd) -> 1 if fd is the console, 0 otherwise
    pub const ISATTY: u32 = 24;
    /// sysinfo(*mut SysInfo) -> 0
    pub const SYSINFO: u32 = 25;
    /// proclist(*mut [ProcInfo], max) -> number of entries written (all threads)
    pub const PROCLIST: u32 = 26;
    /// rename(from, from_len, to, to_len) -> 0
    pub const RENAME: u32 = 27;
    /// seek(fd, offset, whence: 0 = start, 1 = current, 2 = end) -> new offset
    pub const SEEK: u32 = 28;
    /// termsize() -> rows << 32 | columns of the console
    pub const TERMSIZE: u32 = 29;
    /// ignore_interrupt(on) -> 0: Ctrl+C is delivered as a byte (0x03) instead of killing us
    pub const IGNORE_INTERRUPT: u32 = 30;
    /// net_info(*mut NetInfo) -> 0
    pub const NET_INFO: u32 = 31;
    /// ping(ipv4 as u32, seq, timeout_ms) -> round trip time in µs
    pub const PING: u32 = 32;
    /// udp_socket(port, 0 = any) -> fd
    pub const UDP_SOCKET: u32 = 33;
    /// sendto(fd, buf, len, ipv4 as u32, port) -> bytes sent
    pub const SENDTO: u32 = 34;
    /// recvfrom(fd, buf, len, *mut SockAddr or 0, timeout_ms) -> bytes received
    pub const RECVFROM: u32 = 35;
    /// clock_us() -> microseconds since boot
    pub const CLOCK_US: u32 = 36;
    /// tcp_connect(ipv4 as u32, port, timeout_ms) -> fd of the connection
    pub const TCP_CONNECT: u32 = 37;
    /// tcp_listen(port) -> fd of the listener
    pub const TCP_LISTEN: u32 = 38;
    /// tcp_accept(listener fd, *mut SockAddr or 0) -> fd of the next connection (blocks)
    pub const TCP_ACCEPT: u32 = 39;
    /// shutdown(fd) -> 0: no more writing (sends FIN), reading goes on
    pub const SHUTDOWN: u32 = 40;
    /// setenv(name, name_len, value, value_len) -> 0; value 0 removes the variable
    pub const SETENV: u32 = 41;
    /// getenv(name, name_len, buf, len) -> length of the value
    pub const GETENV: u32 = 42;
    /// environ(buf, len) -> length of "NAME=value" entries separated by NUL bytes
    pub const ENVIRON: u32 = 43;
    /// sigaction(sig, handler or SIG_DFL/SIG_IGN, trampoline) -> old disposition
    pub const SIGACTION: u32 = 44;
    /// sigreturn() -> 0: the signal handler has finished (called by the trampoline)
    pub const SIGRETURN: u32 = 45;
    /// thread_spawn(entry, stack_top, arg) -> tid: a further thread, starts at `entry` with RDI = arg
    pub const THREAD_SPAWN: u32 = 46;
    /// thread_exit(code) -> never returns (in the main thread: the process ends)
    pub const THREAD_EXIT: u32 = 47;
    /// thread_join(tid) -> exit code of the thread (as u32)
    pub const THREAD_JOIN: u32 = 48;
    /// futex_wait(addr, expected, timeout_ms or 0) -> 0; -EAGAIN if *addr != expected, -ETIMEDOUT, -EINTR
    pub const FUTEX_WAIT: u32 = 49;
    /// futex_wake(addr, n) -> number of threads woken
    pub const FUTEX_WAKE: u32 = 50;
    /// gettid() -> id of the calling thread (getpid() is the process)
    pub const GETTID: u32 = 51;
}

pub mod errno {
    pub const ENOENT: i64 = 2;
    pub const ESRCH: i64 = 3;
    pub const EINTR: i64 = 4;
    pub const EIO: i64 = 5;
    pub const E2BIG: i64 = 7;
    pub const ENOEXEC: i64 = 8;
    pub const ECHILD: i64 = 10;
    pub const ENOMEM: i64 = 12;
    pub const EBADF: i64 = 9;
    pub const EAGAIN: i64 = 11;
    pub const EFAULT: i64 = 14;
    pub const EEXIST: i64 = 17;
    pub const ENOTDIR: i64 = 20;
    pub const EISDIR: i64 = 21;
    pub const EINVAL: i64 = 22;
    pub const EMFILE: i64 = 24;
    pub const EFBIG: i64 = 27;
    pub const ENOSPC: i64 = 28;
    pub const ESPIPE: i64 = 29;
    pub const ERANGE: i64 = 34;
    pub const ENAMETOOLONG: i64 = 36;
    pub const ENOSYS: i64 = 38;
    pub const EPIPE: i64 = 32;
    pub const ENOTEMPTY: i64 = 39;
    pub const ENODEV: i64 = 19;
    pub const EMSGSIZE: i64 = 90;
    pub const EADDRINUSE: i64 = 98;
    pub const EHOSTUNREACH: i64 = 113;
    pub const ETIMEDOUT: i64 = 110;
    pub const ECONNRESET: i64 = 104;
    pub const ECONNREFUSED: i64 = 111;
}

/// `open` flags. At least one of READ / WRITE is required.
pub mod open_flags {
    pub const READ: u64 = 1;
    /// Writes always append to the end of the file.
    pub const WRITE: u64 = 2;
    /// Create the file if it does not exist (needs WRITE).
    pub const CREATE: u64 = 4;
    /// Empty the file first (needs WRITE).
    pub const TRUNCATE: u64 = 8;
    /// Every write goes to the end of the file (needs WRITE).
    pub const APPEND: u64 = 16;
}

pub const KIND_FILE: u32 = 1;
pub const KIND_DIR: u32 = 2;
pub const MAX_PATH: u64 = 1024;
/// Largest chunk one `read` of a file transfers.
const MAX_READ: u64 = 64 * 1024;

/// Result of `stat`; also part of `DirEntryInfo`. Layout shared with `ulib`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stat {
    pub size: u64,
    pub kind: u32,
    pub _reserved: u32,
    /// year, month, day, hour, minute, second
    pub modified: [u16; 6],
    pub _pad: u32,
}

/// One directory entry for `readdir`. Layout shared with `ulib`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirEntryInfo {
    pub stat: Stat,
    pub name_len: u32,
    pub _pad: u32,
    /// UTF-8, not NUL-terminated (FAT32 long names have at most 255 UTF-16 units)
    pub name: [u8; 256],
}

const _: () = assert!(size_of::<Stat>() == 32 && size_of::<DirEntryInfo>() == 296);

pub const WAIT_EXITED: u32 = 1;
pub const WAIT_KILLED: u32 = 2;

/// Result of `wait`. Layout shared with `ulib`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WaitStatus {
    /// `WAIT_EXITED` or `WAIT_KILLED`
    pub kind: u32,
    pub reason_len: u32,
    /// Exit code (for `WAIT_EXITED`)
    pub code: i64,
    /// Why the kernel killed the process (UTF-8, cut to fit)
    pub reason: [u8; 112],
}

const _: () = assert!(size_of::<WaitStatus>() == 128);

fn fs_errno(e: FsError) -> i64 {
    match e {
        FsError::NotFound => errno::ENOENT,
        FsError::NotADirectory => errno::ENOTDIR,
        FsError::IsADirectory => errno::EISDIR,
        FsError::AlreadyExists => errno::EEXIST,
        FsError::DirectoryNotEmpty => errno::ENOTEMPTY,
        FsError::InvalidName | FsError::InvalidPath => errno::EINVAL,
        FsError::NoSpace => errno::ENOSPC,
        FsError::FileTooLarge => errno::EFBIG,
        FsError::NotFat32 | FsError::Corrupt(_) | FsError::Io(_) | FsError::NotMounted => errno::EIO,
    }
}

fn stat_of(entry: &DirEntry) -> Stat {
    let m = entry.modified;
    Stat {
        size: entry.size as u64,
        kind: if entry.is_dir() { KIND_DIR } else { KIND_FILE },
        modified: [
            m.year,
            m.month as u16,
            m.day as u16,
            m.hour as u16,
            m.minute as u16,
            m.second as u16,
        ],
        ..Stat::default()
    }
}

type SysResult = Result<u64, i64>;

#[unsafe(no_mangle)]
pub extern "C" fn dispatch(frame: &mut SyscallFrame) {
    // `sysret` to a non-canonical address would fault in ring 0
    if frame.rip >= USER_END {
        super::kill_current(String::from("syscall from a non-canonical address"));
    }
    let (a0, a1, a2, a3, a4) = (frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8);
    let result = match frame.rax as u32 {
        _ if frame.rax > u32::MAX as u64 => Err(errno::ENOSYS),
        nr::EXIT => super::exit_current(ExitStatus::Exited(a0 as i64)),
        nr::WRITE => sys_write(a0, a1, a2),
        nr::READ => sys_read(a0, a1, a2),
        nr::SLEEP_MS => {
            if time::sleep_ms_or_abort(a0, super::signal_pending) {
                Ok(0)
            } else {
                Err(errno::EINTR)
            }
        }
        nr::GETPID => Ok(task::current_owner()),
        nr::GETTID => Ok(task::current_id()),
        nr::THREAD_SPAWN => sys_thread_spawn(a0, a1, a2),
        nr::THREAD_EXIT => super::exit_thread(a0 as i64),
        nr::THREAD_JOIN => match super::join_thread(a0) {
            None => Err(errno::ESRCH),
            Some(Err(_)) => Err(errno::EINTR),
            Some(Ok(code)) => Ok(code as u32 as u64),
        },
        nr::FUTEX_WAIT => {
            use super::futex::FutexError;
            super::futex::wait(a0, a1 as u32, a2).map(|_| 0).map_err(|e| match e {
                FutexError::WouldBlock => errno::EAGAIN,
                FutexError::TimedOut => errno::ETIMEDOUT,
                FutexError::Interrupted => errno::EINTR,
                FutexError::Fault => errno::EFAULT,
            })
        }
        nr::FUTEX_WAKE => Ok(super::futex::wake(a0, a1.min(u32::MAX as u64) as usize) as u64),
        nr::UPTIME_MS => Ok(time::uptime_ms()),
        nr::YIELD => {
            task::yield_now();
            Ok(0)
        }
        nr::OPEN => sys_open(a0, a1, a2),
        nr::CLOSE => sys_close(a0),
        nr::READDIR => sys_readdir(a0, a1),
        nr::STAT => sys_stat(a0, a1, a2),
        nr::GETCWD => sys_getcwd(a0, a1),
        nr::UNLINK => sys_unlink(a0, a1),
        nr::MKDIR => sys_mkdir(a0, a1),
        nr::BRK => sys_brk(a0),
        nr::SPAWN => sys_spawn(a0, a1, a2, a3),
        nr::WAIT => sys_wait(a0, a1, a2),
        nr::KILL => sys_kill(a0, a1),
        nr::CHDIR => sys_chdir(a0, a1),
        nr::KSHELL => sys_kshell(a0, a1),
        nr::KCOMMANDS => sys_kcommands(a0, a1),
        nr::PIPE => sys_pipe(a0),
        nr::DUP => sys_dup(a0),
        nr::DUP2 => sys_dup2(a0, a1),
        nr::ISATTY => sys_isatty(a0),
        nr::SYSINFO => sys_sysinfo(a0),
        nr::PROCLIST => sys_proclist(a0, a1),
        nr::RENAME => sys_rename(a0, a1, a2, a3),
        nr::SEEK => sys_seek(a0, a1 as i64, a2),
        nr::TERMSIZE => {
            let (cols, rows) = crate::console::with_console(|c| c.size()).unwrap_or((80, 25));
            Ok((rows as u64) << 32 | cols as u64)
        }
        nr::NET_INFO => sys_net_info(a0),
        nr::CLOCK_US => Ok(time::uptime_us()),
        nr::TCP_CONNECT => sys_tcp_connect(a0, a1, a2),
        nr::TCP_LISTEN => sys_tcp_listen(a0),
        nr::TCP_ACCEPT => sys_tcp_accept(a0, a1),
        nr::SHUTDOWN => sys_shutdown(a0),
        nr::SETENV => sys_setenv(a0, a1, a2, a3),
        nr::GETENV => sys_getenv(a0, a1, a2, a3),
        nr::ENVIRON => sys_environ(a0, a1),
        nr::SIGACTION => sys_sigaction(a0, a1, a2),
        nr::SIGRETURN => {
            with_current(|p| p.signals.handler_done());
            Ok(0)
        }
        nr::PING => net::ping((a0 as u32).to_be_bytes(), a1 as u16, a2.min(10_000)).map_err(net_errno),
        nr::UDP_SOCKET => sys_udp_socket(a0),
        nr::SENDTO => sys_sendto(a0, a1, a2, a3, a4),
        nr::RECVFROM => sys_recvfrom(a0, a1, a2, a3, a4),
        nr::IGNORE_INTERRUPT => {
            with_current(|p| p.set_ignore_interrupt(a0 != 0));
            Ok(0)
        }
        _ => Err(errno::ENOSYS),
    };
    frame.rax = match result {
        Ok(v) => v,
        Err(e) => (-e) as u64,
    };
    super::deliver_signals(&mut frame.rip, &mut frame.rsp, frame.rflags);
}

/// Whether the current process may access `[addr, addr + len)`.
fn accessible(addr: u64, len: u64, writable: bool) -> bool {
    with_current(|p| {
        // heap/stack pages the program has not touched yet are mapped now (demand paging)
        if !p.populate(addr, len) {
            return false;
        }
        p.space.as_ref().is_some_and(|s| s.is_accessible(addr, len, writable))
    })
    .unwrap_or(false)
}

/// Copies a user buffer into the kernel (after checking it).
fn copy_from_user(addr: u64, len: u64) -> Result<Vec<u8>, i64> {
    if !accessible(addr, len, false) {
        return Err(errno::EFAULT);
    }
    // The process's page tables are active: the user buffer is directly readable
    let bytes = crate::arch::x86_64::cpu::user_access(|| unsafe {
        core::slice::from_raw_parts(addr as *const u8, len as usize).to_vec()
    });
    Ok(bytes)
}

/// Copies `data` to a user buffer (after checking it).
pub(super) fn write_user(addr: u64, data: &[u8]) -> Result<(), i64> {
    copy_to_user(addr, data)
}

fn copy_to_user(addr: u64, data: &[u8]) -> Result<(), i64> {
    if !accessible(addr, data.len() as u64, true) {
        return Err(errno::EFAULT);
    }
    crate::arch::x86_64::cpu::user_access(|| unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len())
    });
    Ok(())
}

fn copy_struct_to_user<T: Copy>(addr: u64, value: &T) -> Result<(), i64> {
    let bytes = unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) };
    copy_to_user(addr, bytes)
}

/// Reads a path argument and makes it absolute (relative to the process's cwd).
fn user_path(addr: u64, len: u64) -> Result<String, i64> {
    if len > MAX_PATH {
        return Err(errno::ENAMETOOLONG);
    }
    let bytes = copy_from_user(addr, len)?;
    let path = core::str::from_utf8(&bytes).map_err(|_| errno::EINVAL)?;
    if path.is_empty() {
        return Err(errno::ENOENT);
    }
    let cwd = with_current(|p| p.cwd.clone()).unwrap_or_else(|| String::from("/"));
    Ok(fs::normalize(&cwd, path))
}

/// What `read`/`write` need to know about a descriptor (copied out of the table).
enum Target {
    Stdin,
    Stdout(Output),
    File {
        path: String,
        offset: u64,
        readable: bool,
        writable: bool,
        append: bool,
    },
    Dir,
    Pipe(PipeEnd),
    /// A UDP socket: only `sendto`/`recvfrom` (read/write give EBADF)
    Socket,
    Tcp(tcp::ConnId),
}

fn target(fd: u64) -> Result<Target, i64> {
    with_current(|p| {
        let handle = p.files.get(fd as usize).and_then(|h| h.as_ref()).ok_or(errno::EBADF)?;
        Ok(match handle {
            Handle::Stdin => Target::Stdin,
            Handle::Stdout => Target::Stdout(p.stdout.clone()),
            Handle::File {
                path,
                offset,
                readable,
                writable,
                append,
            } => Target::File {
                path: path.clone(),
                offset: offset.load(Ordering::Relaxed),
                readable: *readable,
                writable: *writable,
                append: *append,
            },
            Handle::Dir { .. } => Target::Dir,
            Handle::Pipe(end) => Target::Pipe(end.clone()),
            Handle::Udp(_) | Handle::TcpListener(_) => Target::Socket,
            Handle::Tcp(c) => Target::Tcp(c.0),
        })
    })
    .unwrap_or(Err(errno::EBADF))
}

fn sys_write(fd: u64, buf: u64, len: u64) -> SysResult {
    let target = target(fd)?;
    let data = copy_from_user(buf, len)?;
    if let Target::File {
        path,
        offset,
        writable: true,
        append: false,
        ..
    } = &target
    {
        // positioned write; the descriptor's offset moves on
        fs::with_root(|fs| fs.write_at(path, *offset, &data)).map_err(fs_errno)?;
        set_offset(fd, offset + data.len() as u64);
        return Ok(len);
    }
    write_target(target, &data)
}

fn set_offset(fd: u64, new: u64) {
    with_current(|p| {
        if let Some(Some(Handle::File { offset, .. })) = p.files.get(fd as usize) {
            offset.store(new, Ordering::Relaxed);
        }
    });
}

/// Writes `data` to what a descriptor refers to.
fn write_target(target: Target, data: &[u8]) -> SysResult {
    match target {
        Target::Stdout(Output::Capture(sink)) => without_interrupts(|| sink.lock().extend_from_slice(data)),
        Target::Stdout(Output::Console) => crate::print!("{}", String::from_utf8_lossy(data)),
        Target::File {
            path, writable: true, ..
        } => {
            fs::with_root(|fs| fs.append_file(&path, data)).map_err(fs_errno)?;
        }
        Target::Pipe(end) if end.write => {
            return end
                .write(data, super::signal_pending)
                .map(|n| n as u64)
                .map_err(pipe_errno);
        }
        Target::Tcp(id) => {
            return tcp::send(id, data, super::signal_pending)
                .map(|n| n as u64)
                .map_err(tcp_errno);
        }
        Target::Dir => return Err(errno::EISDIR),
        _ => return Err(errno::EBADF),
    }
    Ok(data.len() as u64)
}

fn pipe_errno(e: PipeError) -> i64 {
    match e {
        PipeError::Broken => errno::EPIPE,
        PipeError::Interrupted => errno::EINTR,
    }
}

fn sys_read(fd: u64, buf: u64, len: u64) -> SysResult {
    match target(fd)? {
        Target::Stdin => read_stdin(buf, len),
        Target::File {
            path,
            offset,
            readable: true,
            ..
        } => {
            if !accessible(buf, len, true) {
                return Err(errno::EFAULT);
            }
            let mut data = alloc::vec![0u8; len.min(MAX_READ) as usize];
            let n = fs::with_root(|fs| fs.read_at(&path, offset, &mut data)).map_err(fs_errno)?;
            copy_to_user(buf, &data[..n])?;
            set_offset(fd, offset + n as u64);
            Ok(n as u64)
        }
        Target::Tcp(id) => {
            if !accessible(buf, len, true) {
                return Err(errno::EFAULT);
            }
            let data = tcp::recv(id, len.min(MAX_READ) as usize, super::signal_pending).map_err(tcp_errno)?;
            copy_to_user(buf, &data)?;
            Ok(data.len() as u64)
        }
        Target::Pipe(end) if !end.write => {
            if !accessible(buf, len, true) {
                return Err(errno::EFAULT);
            }
            let data = end
                .read(len.min(MAX_READ) as usize, super::signal_pending)
                .map_err(pipe_errno)?;
            copy_to_user(buf, &data)?;
            Ok(data.len() as u64)
        }
        Target::Dir => Err(errno::EISDIR),
        _ => Err(errno::EBADF),
    }
}

/// Puts `handle` into the lowest free descriptor slot.
fn install(p: &mut super::Process, handle: Handle) -> Result<u64, i64> {
    let fd = match p.files.iter().position(|h| h.is_none()) {
        Some(free) => free,
        None if p.files.len() < MAX_FDS => {
            p.files.push(None);
            p.files.len() - 1
        }
        None => return Err(errno::EMFILE),
    };
    p.files[fd] = Some(handle);
    Ok(fd as u64)
}

fn sys_pipe(fds_addr: u64) -> SysResult {
    if !accessible(fds_addr, 8, true) {
        return Err(errno::EFAULT);
    }
    let (r, w) = pipe::pipe();
    let fds = with_current(|p| {
        let rfd = install(p, Handle::Pipe(r))?;
        match install(p, Handle::Pipe(w)) {
            Ok(wfd) => Ok([rfd as u32, wfd as u32]),
            Err(e) => {
                p.files[rfd as usize] = None;
                Err(e)
            }
        }
    })
    .unwrap_or(Err(errno::EBADF))?;
    copy_struct_to_user(fds_addr, &fds)?;
    Ok(0)
}

fn sys_dup(fd: u64) -> SysResult {
    with_current(|p| {
        let handle = p.files.get(fd as usize).cloned().flatten().ok_or(errno::EBADF)?;
        install(p, handle)
    })
    .unwrap_or(Err(errno::EBADF))
}

fn sys_dup2(fd: u64, new_fd: u64) -> SysResult {
    if new_fd as usize >= MAX_FDS {
        return Err(errno::EBADF);
    }
    with_current(|p| {
        let handle = p.files.get(fd as usize).cloned().flatten().ok_or(errno::EBADF)?;
        if p.files.len() <= new_fd as usize {
            p.files.resize_with(new_fd as usize + 1, || None);
        }
        p.files[new_fd as usize] = Some(handle); // drops (closes) the old one
        Ok(new_fd)
    })
    .unwrap_or(Err(errno::EBADF))
}

fn sys_isatty(fd: u64) -> SysResult {
    match target(fd)? {
        Target::Stdin | Target::Stdout(Output::Console) => Ok(1),
        _ => Ok(0),
    }
}

fn sys_open(path: u64, path_len: u64, flags: u64) -> SysResult {
    use open_flags::*;
    let path = user_path(path, path_len)?;
    let (read, write) = (flags & READ != 0, flags & WRITE != 0);
    if flags & !(READ | WRITE | CREATE | TRUNCATE | APPEND) != 0 || !(read || write) {
        return Err(errno::EINVAL);
    }
    if flags & (CREATE | TRUNCATE | APPEND) != 0 && !write {
        return Err(errno::EINVAL);
    }
    let handle = fs::with_root(|fs| match fs.lookup(&path) {
        Ok(entry) if entry.is_dir() => {
            if write {
                return Err(FsError::IsADirectory);
            }
            Ok(Handle::Dir {
                entries: fs.read_dir(&path)?,
                next: 0,
            })
        }
        Ok(_) | Err(FsError::NotFound) => {
            let exists = fs.lookup(&path).is_ok();
            if !exists && flags & CREATE == 0 {
                return Err(FsError::NotFound);
            }
            if !exists || flags & TRUNCATE != 0 {
                fs.write_file(&path, b"")?;
            }
            Ok(Handle::File {
                path: path.clone(),
                offset: Arc::new(AtomicU64::new(0)),
                readable: read,
                writable: write,
                append: flags & APPEND != 0,
            })
        }
        Err(e) => Err(e),
    })
    .map_err(fs_errno)?;

    with_current(|p| install(p, handle)).unwrap_or(Err(errno::EBADF))
}

fn sys_close(fd: u64) -> SysResult {
    with_current(|p| match p.files.get_mut(fd as usize) {
        Some(slot @ Some(_)) => {
            *slot = None;
            Ok(0)
        }
        _ => Err(errno::EBADF),
    })
    .unwrap_or(Err(errno::EBADF))
}

fn sys_readdir(fd: u64, info_addr: u64) -> SysResult {
    let entry = with_current(|p| match p.files.get_mut(fd as usize) {
        Some(Some(Handle::Dir { entries, next })) => {
            let e = entries.get(*next).cloned();
            if e.is_some() {
                *next += 1;
            }
            Ok(e)
        }
        Some(Some(_)) => Err(errno::ENOTDIR),
        _ => Err(errno::EBADF),
    })
    .unwrap_or(Err(errno::EBADF))?;
    let Some(entry) = entry else {
        return Ok(0);
    };
    let mut info = DirEntryInfo {
        stat: stat_of(&entry),
        name_len: 0,
        _pad: 0,
        name: [0; 256],
    };
    // A long name can be up to 255 UTF-16 units = more than 256 UTF-8 bytes: cut at a char boundary
    let mut len = entry.name.len().min(info.name.len());
    while !entry.name.is_char_boundary(len) {
        len -= 1;
    }
    info.name[..len].copy_from_slice(&entry.name.as_bytes()[..len]);
    info.name_len = len as u32;
    copy_struct_to_user(info_addr, &info)?;
    Ok(1)
}

fn sys_stat(path: u64, path_len: u64, stat_addr: u64) -> SysResult {
    let path = user_path(path, path_len)?;
    let entry = fs::with_root(|fs| fs.lookup(&path)).map_err(fs_errno)?;
    copy_struct_to_user(stat_addr, &stat_of(&entry))?;
    Ok(0)
}

fn sys_unlink(path: u64, path_len: u64) -> SysResult {
    let path = user_path(path, path_len)?;
    fs::with_root(|fs| fs.remove(&path)).map_err(fs_errno)?;
    Ok(0)
}

fn sys_mkdir(path: u64, path_len: u64) -> SysResult {
    let path = user_path(path, path_len)?;
    fs::with_root(|fs| fs.create_dir(&path)).map_err(fs_errno)?;
    Ok(0)
}

/// Moves the end of the heap to `new_end` (mapping zeroed pages or freeing
/// them). Fails with ENOMEM outside `[heap_start, heap_start + HEAP_MAX]`
/// or when memory runs out.
fn sys_thread_spawn(entry: u64, stack: u64, arg: u64) -> SysResult {
    // both end up in `iretq` to ring 3: user addresses only
    if entry == 0 || entry >= USER_END || stack == 0 || stack >= USER_END {
        return Err(errno::EINVAL);
    }
    super::spawn_thread(entry, stack, arg).ok_or(errno::EAGAIN)
}

fn sys_brk(new_end: u64) -> SysResult {
    use crate::mem::{PAGE_SIZE, VirtAddr, align_up};
    let (start, end) = with_current(|p| (p.heap_start, p.heap_end)).ok_or(errno::EBADF)?;
    if new_end == 0 {
        return Ok(end);
    }
    if new_end < start || new_end > start + super::HEAP_MAX {
        return Err(errno::ENOMEM);
    }
    let (old_top, new_top) = (align_up(end, PAGE_SIZE), align_up(new_end, PAGE_SIZE));
    // Growing only moves the end: pages are mapped on first use (demand
    // paging, see `process::handle_page_fault`). Shrinking frees the pages
    // that were used (unmapped ones are skipped).
    let (freed, root, threads) = with_current(|p| {
        p.heap_end = new_end;
        let root = p.space.as_ref().map(|s| s.root());
        let freed = match p.space.as_mut() {
            Some(space) if new_top < old_top => {
                space.unmap_keeping_frames(VirtAddr(new_top), ((old_top - new_top) / PAGE_SIZE) as usize)
            }
            _ => Vec::new(),
        };
        (freed, root, p.thread_count())
    })
    .ok_or(errno::EBADF)?;
    if threads > 0 && !freed.is_empty() {
        // other threads may run on other CPUs with these pages in their TLB
        if let Some(root) = root {
            crate::mem::paging::shoot_down(root);
        }
    }
    for frame in freed {
        crate::mem::pmm::free_frame(frame);
    }
    Ok(new_end)
}

fn sys_spawn(path: u64, path_len: u64, args: u64, args_len: u64) -> SysResult {
    use super::{ExecError, Options};
    let path = user_path(path, path_len)?;
    if args_len > super::MAX_ARGS_LEN as u64 {
        return Err(errno::E2BIG);
    }
    let args = copy_from_user(args, args_len)?;
    let args = core::str::from_utf8(&args).map_err(|_| errno::EINVAL)?;
    let image = fs::with_root(|fs| fs.read_file(&path)).map_err(fs_errno)?;
    let me = task::current_owner();
    let (cwd, stdout, std_files, (env, signals)) = with_current(|p| {
        let fd = |i: usize| p.files.get(i).cloned().flatten();
        (
            p.cwd.clone(),
            p.stdout.clone(),
            [fd(0), fd(1), fd(2)],
            (p.env.clone(), p.signals.for_child()),
        )
    })
    .ok_or(errno::ESRCH)?;
    let name = path.rsplit('/').next().unwrap_or(&path);
    let opts = Options {
        cwd: &cwd,
        stdout,
        parent: Some(me),
        foreground: false, // the console is lent to a child only while we wait for it
        ignore_interrupt: false,
        std_files: Some(std_files), // the child inherits stdin/stdout/stderr (pipes, redirections)
        env: Some(env),
        signals: Some(signals),
    };
    super::spawn_with(name, &image, args, opts).map_err(|e| match e {
        ExecError::Elf(_) => errno::ENOEXEC,
        ExecError::OutOfMemory => errno::ENOMEM,
        ExecError::ArgsTooLong => errno::E2BIG,
    })
}

pub const WNOHANG: u64 = 1;

fn sys_wait(pid: u64, status_addr: u64, flags: u64) -> SysResult {
    use super::WaitError;
    if status_addr != 0 && !accessible(status_addr, size_of::<WaitStatus>() as u64, true) {
        return Err(errno::EFAULT);
    }
    let status = super::wait_child(pid, flags & WNOHANG != 0).map_err(|e| match e {
        WaitError::NotAChild => errno::ECHILD,
        WaitError::Interrupted => errno::EINTR,
    })?;
    let Some(status) = status else {
        return Ok(0); // still running
    };
    if status_addr != 0 {
        let mut out = WaitStatus {
            kind: WAIT_EXITED,
            reason_len: 0,
            code: 0,
            reason: [0; 112],
        };
        match status {
            ExitStatus::Exited(code) => out.code = code,
            ExitStatus::Killed(reason) => {
                out.kind = WAIT_KILLED;
                out.code = super::signal::number_for_reason(&reason) as i64;
                let mut len = reason.len().min(out.reason.len());
                while !reason.is_char_boundary(len) {
                    len -= 1;
                }
                out.reason[..len].copy_from_slice(&reason.as_bytes()[..len]);
                out.reason_len = len as u32;
            }
        }
        copy_struct_to_user(status_addr, &out)?;
    }
    Ok(pid)
}

/// Any user process may be signalled (a single-user system).
fn sys_kill(pid: u64, sig: u64) -> SysResult {
    if sig >= super::signal::NSIG as u64 {
        return Err(errno::EINVAL);
    }
    let found = if sig == 0 {
        without_interrupts(|| super::PROCESSES.lock().get(&pid).is_some_and(|p| p.status.is_none()))
    } else {
        super::send_signal(pid, sig as u32)
    };
    if found { Ok(0) } else { Err(errno::ESRCH) }
}

fn sys_sigaction(sig: u64, handler: u64, trampoline: u64) -> SysResult {
    // the trampoline becomes a `sysret` target: it must be a user address
    if sig >= super::signal::NSIG as u64 || trampoline >= USER_END {
        return Err(errno::EINVAL);
    }
    with_current(|p| p.signals.set(sig as u32, handler, trampoline))
        .ok_or(errno::ESRCH)?
        .map_err(|_| errno::EINVAL)
}

fn sys_chdir(path: u64, path_len: u64) -> SysResult {
    let path = user_path(path, path_len)?;
    let entry = fs::with_root(|fs| fs.lookup(&path)).map_err(fs_errno)?;
    if !entry.is_dir() {
        return Err(errno::ENOTDIR);
    }
    with_current(|p| p.cwd = path);
    Ok(0)
}

fn sys_kshell(line: u64, line_len: u64) -> SysResult {
    if line_len > 4096 {
        return Err(errno::E2BIG);
    }
    let bytes = copy_from_user(line, line_len)?;
    let line = core::str::from_utf8(&bytes).map_err(|_| errno::EINVAL)?;
    let cwd = with_current(|p| p.cwd.clone()).ok_or(errno::ESRCH)?;
    let mut shell = crate::shell::Shell::new();
    shell.set_cwd(&cwd);
    // Output goes to the caller's fd 1 (console directly, anything else buffered)
    let ok = match target(1)? {
        Target::Stdout(Output::Console) => shell.execute_builtin(line, &mut crate::shell::Stdout),
        other => {
            let mut text = String::new();
            let ok = shell.execute_builtin(line, &mut text);
            write_target(other, text.as_bytes())?;
            ok
        }
    };
    match ok {
        None => Err(errno::ENOENT),
        Some(true) => Ok(0),
        Some(false) => Ok(1),
    }
}

fn sys_kcommands(buf: u64, len: u64) -> SysResult {
    let names = crate::shell::kshell_commands().join("\0");
    if names.len() as u64 > len {
        return Err(errno::ERANGE);
    }
    copy_to_user(buf, names.as_bytes())?;
    Ok(names.len() as u64)
}

/// System overview for `mem`, `uptime`, `df`, `date`. Layout shared with `ulib`.
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
    /// year, month, day, hour, minute, second (CMOS clock)
    pub date: [u16; 6],
    pub _pad2: u32,
    pub fs_label: [u8; 16],
    pub cpu_vendor: [u8; 16],
    pub cpu_brand: [u8; 48],
}

/// One thread for `proclist`. Layout shared with `ulib`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcInfo {
    pub tid: u64,
    pub cpu_ticks: u64,
    /// 0 for kernel threads and processes started by the kernel
    pub parent: u64,
    /// `PROC_*` state
    pub state: u32,
    /// 1 if the thread is a user process
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
pub const PROC_WAIT_DISK: u32 = 8;
pub const PROC_WAIT_LOCK: u32 = 9;
pub const PROC_WAIT_FUTEX: u32 = 10;

const _: () = assert!(size_of::<SysInfo>() == 176 && size_of::<ProcInfo>() == 64);

fn copy_str(dst: &mut [u8], src: &str) {
    let mut len = src.len().min(dst.len());
    while !src.is_char_boundary(len) {
        len -= 1;
    }
    dst[..len].copy_from_slice(&src.as_bytes()[..len]);
}

fn sys_sysinfo(addr: u64) -> SysResult {
    use crate::arch::x86_64::cpu;
    use crate::mem::{heap, pmm};
    let p = pmm::stats();
    let h = heap::stats();
    let d = crate::drivers::rtc::now();
    let mut info = SysInfo {
        uptime_ms: time::uptime_ms(),
        ticks: time::ticks(),
        mem_usable: p.usable_bytes(),
        mem_free: p.free_bytes(),
        heap_total: h.total as u64,
        heap_used: h.used as u64,
        fs_total: 0,
        fs_free: 0,
        fs_cluster_size: 0,
        timer_hz: time::TIMER_HZ as u32,
        cpus: crate::acpi::info().map_or(1, |a| a.madt.local_apics.len() as u32),
        _pad: 0,
        date: [
            d.year,
            d.month as u16,
            d.day as u16,
            d.hour as u16,
            d.minute as u16,
            d.second as u16,
        ],
        _pad2: 0,
        fs_label: [0; 16],
        cpu_vendor: [0; 16],
        cpu_brand: [0; 48],
    };
    if let Ok((label, s)) = fs::with_root(|fs| Ok((String::from(fs.label()), fs.stats()))) {
        info.fs_total = s.total_clusters as u64 * s.cluster_size as u64;
        info.fs_free = s.free_clusters as u64 * s.cluster_size as u64;
        info.fs_cluster_size = s.cluster_size as u32;
        copy_str(&mut info.fs_label, &label);
    }
    copy_str(&mut info.cpu_vendor, &cpu::vendor());
    copy_str(&mut info.cpu_brand, cpu::brand().as_deref().unwrap_or(""));
    copy_struct_to_user(addr, &info)?;
    Ok(0)
}

fn sys_proclist(addr: u64, max: u64) -> SysResult {
    use crate::task::{State, Wait};
    let threads = task::list();
    let n = threads.len().min(max as usize);
    if !accessible(addr, (n * size_of::<ProcInfo>()) as u64, true) {
        return Err(errno::EFAULT);
    }
    for (i, t) in threads.iter().take(n).enumerate() {
        let mut info = ProcInfo {
            tid: t.id,
            cpu_ticks: t.cpu_ticks,
            // the parent of a further thread is its process
            parent: if t.owner != t.id {
                t.owner
            } else {
                super::parent_of(t.id).unwrap_or(0)
            },
            state: match t.state {
                State::Running => PROC_RUNNING,
                State::Ready => PROC_READY,
                State::Blocked(Wait::Until(_)) => PROC_SLEEPING,
                State::Blocked(Wait::Input) => PROC_WAIT_INPUT,
                State::Blocked(Wait::Join(_)) => PROC_JOIN,
                State::Blocked(Wait::Pipe(_)) => PROC_WAIT_PIPE,
                State::Blocked(Wait::Net) => PROC_WAIT_NET,
                State::Blocked(Wait::Disk) => PROC_WAIT_DISK,
                State::Blocked(Wait::Lock(_)) => PROC_WAIT_LOCK,
                State::Blocked(Wait::Futex(_)) => PROC_WAIT_FUTEX,
                State::Dead => PROC_DEAD,
            },
            user: (t.owner != t.id || super::is_process(t.id)) as u32,
            name: [0; 32],
        };
        copy_str(&mut info.name, &t.name);
        copy_struct_to_user(addr + (i * size_of::<ProcInfo>()) as u64, &info)?;
    }
    Ok(n as u64)
}

fn sys_rename(from: u64, from_len: u64, to: u64, to_len: u64) -> SysResult {
    let from = user_path(from, from_len)?;
    let to = user_path(to, to_len)?;
    fs::with_root(|fs| fs.rename(&from, &to)).map_err(fs_errno)?;
    Ok(0)
}

fn sys_seek(fd: u64, offset: i64, whence: u64) -> SysResult {
    let Target::File {
        path, offset: current, ..
    } = target(fd)?
    else {
        return Err(errno::ESPIPE);
    };
    let base = match whence {
        0 => 0,
        1 => current as i64,
        2 => fs::with_root(|fs| fs.lookup(&path)).map_err(fs_errno)?.size as i64,
        _ => return Err(errno::EINVAL),
    };
    let new = base.checked_add(offset).filter(|&o| o >= 0).ok_or(errno::EINVAL)?;
    set_offset(fd, new as u64);
    Ok(new as u64)
}

/// Network configuration for `netinfo`. Layout shared with `ulib`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NetInfo {
    pub mac: [u8; 6],
    pub link_up: u8,
    /// 1 = configured by DHCP, 0 = static fallback
    pub dhcp: u8,
    pub ip: [u8; 4],
    pub netmask: [u8; 4],
    pub gateway: [u8; 4],
    pub dns: [u8; 4],
    pub rx_frames: u64,
    pub tx_frames: u64,
}

/// Source of a received datagram. Layout shared with `ulib`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SockAddr {
    pub ip: [u8; 4],
    pub port: u16,
    pub _pad: u16,
}

const _: () = assert!(size_of::<NetInfo>() == 40 && size_of::<SockAddr>() == 8);

fn net_errno(e: NetError) -> i64 {
    match e {
        NetError::NoDevice => errno::ENODEV,
        NetError::Unreachable => errno::EHOSTUNREACH,
        NetError::TimedOut => errno::ETIMEDOUT,
        NetError::AddressInUse => errno::EADDRINUSE,
        NetError::TooLong => errno::EMSGSIZE,
        NetError::Interrupted => errno::EINTR,
    }
}

fn tcp_errno(e: tcp::TcpError) -> i64 {
    match e {
        tcp::TcpError::Refused => errno::ECONNREFUSED,
        tcp::TcpError::Reset => errno::ECONNRESET,
        tcp::TcpError::TimedOut => errno::ETIMEDOUT,
        tcp::TcpError::AddressInUse => errno::EADDRINUSE,
        tcp::TcpError::NotConnected => errno::EPIPE,
        tcp::TcpError::Interrupted => errno::EINTR,
        tcp::TcpError::Net(e) => net_errno(e),
    }
}

fn install_handle(handle: Handle) -> SysResult {
    with_current(|p| install(p, handle)).unwrap_or(Err(errno::EBADF))
}

fn sys_tcp_connect(ip: u64, port: u64, timeout_ms: u64) -> SysResult {
    let port = u16::try_from(port).map_err(|_| errno::EINVAL)?;
    let id = tcp::connect(
        (ip as u32).to_be_bytes(),
        port,
        timeout_ms.clamp(100, 60_000),
        super::signal_pending,
    )
    .map_err(tcp_errno)?;
    let conn = alloc::sync::Arc::new(super::TcpConnection(id));
    install_handle(Handle::Tcp(conn))
}

fn sys_tcp_listen(port: u64) -> SysResult {
    let port = u16::try_from(port).ok().filter(|&p| p != 0).ok_or(errno::EINVAL)?;
    tcp::listen(port).map_err(tcp_errno)?;
    install_handle(Handle::TcpListener(alloc::sync::Arc::new(super::TcpListener(port))))
}

fn sys_tcp_accept(fd: u64, from: u64) -> SysResult {
    let port = with_current(|p| match p.files.get(fd as usize) {
        Some(Some(Handle::TcpListener(l))) => Ok(l.0),
        Some(Some(_)) => Err(errno::EINVAL),
        _ => Err(errno::EBADF),
    })
    .unwrap_or(Err(errno::EBADF))?;
    if from != 0 && !accessible(from, size_of::<SockAddr>() as u64, true) {
        return Err(errno::EFAULT);
    }
    let (id, ip, peer_port) = tcp::accept(port, super::signal_pending).map_err(tcp_errno)?;
    let conn = alloc::sync::Arc::new(super::TcpConnection(id));
    if from != 0 {
        copy_struct_to_user(
            from,
            &SockAddr {
                ip,
                port: peer_port,
                _pad: 0,
            },
        )?;
    }
    install_handle(Handle::Tcp(conn))
}

fn sys_shutdown(fd: u64) -> SysResult {
    match target(fd)? {
        Target::Tcp(id) => {
            tcp::shutdown(id);
            Ok(0)
        }
        _ => Err(errno::EINVAL),
    }
}

fn sys_net_info(addr: u64) -> SysResult {
    let cfg = net::config().ok_or(errno::ENODEV)?;
    let (rx, tx, up) = net::stats().unwrap_or((0, 0, false));
    let info = NetInfo {
        mac: cfg.mac,
        link_up: up as u8,
        ip: cfg.ip,
        netmask: cfg.netmask,
        gateway: cfg.gateway,
        dns: cfg.dns,
        dhcp: cfg.dhcp as u8,
        rx_frames: rx,
        tx_frames: tx,
        ..NetInfo::default()
    };
    copy_struct_to_user(addr, &info)?;
    Ok(0)
}

fn sys_udp_socket(port: u64) -> SysResult {
    let port = u16::try_from(port).map_err(|_| errno::EINVAL)?;
    let socket = net::udp_bind(port).map_err(net_errno)?;
    with_current(|p| install(p, Handle::Udp(socket))).unwrap_or(Err(errno::EBADF))
}

fn udp_socket_of(fd: u64) -> Result<alloc::sync::Arc<net::UdpSocket>, i64> {
    with_current(|p| match p.files.get(fd as usize) {
        Some(Some(Handle::Udp(s))) => Ok(s.clone()),
        Some(Some(_)) => Err(errno::EINVAL),
        _ => Err(errno::EBADF),
    })
    .unwrap_or(Err(errno::EBADF))
}

fn sys_sendto(fd: u64, buf: u64, len: u64, ip: u64, port: u64) -> SysResult {
    let socket = udp_socket_of(fd)?;
    let data = copy_from_user(buf, len)?;
    let port = u16::try_from(port).map_err(|_| errno::EINVAL)?;
    socket
        .send_to((ip as u32).to_be_bytes(), port, &data)
        .map_err(net_errno)?;
    Ok(len)
}

fn sys_recvfrom(fd: u64, buf: u64, len: u64, from: u64, timeout_ms: u64) -> SysResult {
    let socket = udp_socket_of(fd)?;
    if !accessible(buf, len, true) || (from != 0 && !accessible(from, size_of::<SockAddr>() as u64, true)) {
        return Err(errno::EFAULT);
    }
    let d = socket
        .recv(timeout_ms.min(60_000), super::signal_pending)
        .map_err(net_errno)?;
    let n = d.data.len().min(len as usize); // the rest of a longer datagram is dropped
    copy_to_user(buf, &d.data[..n])?;
    if from != 0 {
        copy_struct_to_user(
            from,
            &SockAddr {
                ip: d.src,
                port: d.src_port,
                _pad: 0,
            },
        )?;
    }
    Ok(n as u64)
}

/// Reads a variable name: non-empty UTF-8 without '=' or NUL.
fn env_name(addr: u64, len: u64) -> Result<String, i64> {
    if len == 0 || len > 256 {
        return Err(errno::EINVAL);
    }
    let bytes = copy_from_user(addr, len)?;
    let name = String::from_utf8(bytes).map_err(|_| errno::EINVAL)?;
    if name.contains('=') || name.contains('\0') {
        return Err(errno::EINVAL);
    }
    Ok(name)
}

fn sys_setenv(name: u64, name_len: u64, value: u64, value_len: u64) -> SysResult {
    let name = env_name(name, name_len)?;
    let value = if value == 0 {
        None
    } else {
        if value_len > 4096 {
            return Err(errno::E2BIG);
        }
        let v = String::from_utf8(copy_from_user(value, value_len)?).map_err(|_| errno::EINVAL)?;
        if v.contains('\0') {
            return Err(errno::EINVAL);
        }
        Some(v)
    };
    with_current(|p| {
        let too_many = p.env.len() >= 256 && !p.env.contains_key(&name);
        match value {
            Some(_) if too_many => Err(errno::E2BIG),
            Some(v) => {
                p.env.insert(name, v);
                Ok(0)
            }
            None => {
                p.env.remove(&name);
                Ok(0)
            }
        }
    })
    .unwrap_or(Err(errno::ESRCH))
}

fn sys_getenv(name: u64, name_len: u64, buf: u64, len: u64) -> SysResult {
    let name = env_name(name, name_len)?;
    let value = with_current(|p| p.env.get(&name).cloned())
        .flatten()
        .ok_or(errno::ENOENT)?;
    if value.len() as u64 > len {
        return Err(errno::ERANGE);
    }
    copy_to_user(buf, value.as_bytes())?;
    Ok(value.len() as u64)
}

fn sys_environ(buf: u64, len: u64) -> SysResult {
    let all = with_current(|p| {
        let entries: Vec<String> = p.env.iter().map(|(k, v)| alloc::format!("{}={}", k, v)).collect();
        entries.join("\0")
    })
    .unwrap_or_default();
    if all.len() as u64 > len {
        return Err(errno::ERANGE);
    }
    copy_to_user(buf, all.as_bytes())?;
    Ok(all.len() as u64)
}

fn sys_getcwd(buf: u64, len: u64) -> SysResult {
    let cwd = with_current(|p| p.cwd.clone()).unwrap_or_default();
    if (cwd.len() as u64) > len {
        return Err(errno::ERANGE);
    }
    copy_to_user(buf, cwd.as_bytes())?;
    Ok(cwd.len() as u64)
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
        // like a VT100/xterm terminal
        Key::Up => out.extend_from_slice(b"\x1b[A"),
        Key::Down => out.extend_from_slice(b"\x1b[B"),
        Key::Right => out.extend_from_slice(b"\x1b[C"),
        Key::Left => out.extend_from_slice(b"\x1b[D"),
        Key::Home => out.extend_from_slice(b"\x1b[H"),
        Key::End => out.extend_from_slice(b"\x1b[F"),
        Key::Insert => out.extend_from_slice(b"\x1b[2~"),
        Key::Delete => out.extend_from_slice(b"\x1b[3~"),
        Key::PageUp => out.extend_from_slice(b"\x1b[5~"),
        Key::PageDown => out.extend_from_slice(b"\x1b[6~"),
        Key::F(_) => {}
    }
}

fn read_stdin(buf: u64, len: u64) -> SysResult {
    if !accessible(buf, len, true) {
        return Err(errno::EFAULT);
    }
    if len == 0 {
        return Ok(0);
    }
    let mut bytes = with_current(|p| core::mem::take(&mut p.stdin_pending)).unwrap_or_default();
    while bytes.is_empty() {
        // blocks until a key arrives; a kill (e.g. Ctrl+C) aborts the wait
        match input::read_key_or_abort(super::signal_pending) {
            Some(event) => key_bytes(event, &mut bytes),
            None => return Err(errno::EINTR),
        }
    }
    let n = bytes.len().min(len as usize);
    let rest = bytes.split_off(n);
    // checked again: another thread may have changed the mapping while we waited
    if let Err(e) = copy_to_user(buf, &bytes) {
        with_current(|p| p.stdin_pending = [bytes, rest.clone()].concat());
        return Err(e);
    }
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
        assert_eq!(out, b"a\xC3\xA4\x03\n\x1b[D\x08");
    }
}
