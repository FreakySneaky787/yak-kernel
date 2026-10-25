//! Signals: install handlers, ignore signals, send them.
//!
//! The kernel runs a handler by pushing `sig`, `handler`, `rflags` and the
//! interrupted `rip` onto the stack and jumping to `trampoline` below. It
//! saves every register and the SSE state, calls `handler(sig)`, tells the
//! kernel that the handler is done (`sigreturn`, which lets the next one
//! run), restores everything and returns to the interrupted code with
//! `popfq; ret`. This needs code without a red zone (`disable-redzone` in
//! the target), because the kernel writes right below RSP.
//!
//! A handler interrupts the program at any instruction: it should only set
//! flags (atomics) or call system calls. A blocking system call that is
//! interrupted by a handled signal returns `-EINTR`.

use crate::sys;

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

/// Names for `kill -l` and `kill -NAME` (without the "SIG").
pub const NAMES: &[(&str, u32)] = &[
    ("HUP", SIGHUP),
    ("INT", SIGINT),
    ("QUIT", SIGQUIT),
    ("KILL", SIGKILL),
    ("USR1", SIGUSR1),
    ("SEGV", SIGSEGV),
    ("USR2", SIGUSR2),
    ("PIPE", SIGPIPE),
    ("ALRM", SIGALRM),
    ("TERM", SIGTERM),
];

/// "TERM", "SIGTERM", "term" or "15" -> 15.
pub fn parse(name: &str) -> Option<u32> {
    if let Ok(n) = name.parse::<u32>() {
        return (n < 32).then_some(n);
    }
    let upper = name.trim_start_matches("SIG").trim_start_matches("sig");
    NAMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(upper))
        .map(|&(_, s)| s)
}

pub type Handler = extern "C" fn(sig: u32);

core::arch::global_asm!(
    ".global ulib_signal_trampoline",
    "ulib_signal_trampoline:",
    // [rsp] = sig, [rsp+8] = handler, [rsp+16] = rflags, [rsp+24] = rip
    "push rax",
    "push rbx",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push rbp",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov rbx, rsp", // callee-saved: survives the handler
    "and rsp, -16",
    "sub rsp, 512",
    "fxsave [rsp]",
    "cld",
    "mov edi, dword ptr [rbx + 120]",
    "call qword ptr [rbx + 128]",
    "fxrstor [rsp]",
    "mov eax, {sigreturn}",
    "syscall", // clobbers rax, rcx, r11: restored below
    "mov rsp, rbx",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rbp",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rbx",
    "pop rax",
    "lea rsp, [rsp + 16]", // skip sig and handler (lea keeps the flags)
    "popfq",
    "ret",
    sigreturn = const sys::SIGRETURN,
);

unsafe extern "C" {
    fn ulib_signal_trampoline();
}

fn sigaction(sig: u32, disposition: u64) -> Result<u64, crate::fs::Error> {
    let ret = sys::sigaction(sig, disposition, ulib_signal_trampoline as *const () as usize as u64);
    if ret < 0 {
        Err(crate::fs::Error(-ret))
    } else {
        Ok(ret as u64)
    }
}

/// Runs `handler` when `sig` arrives. Returns the previous disposition.
pub fn set_handler(sig: u32, handler: Handler) -> Result<u64, crate::fs::Error> {
    sigaction(sig, handler as usize as u64)
}

/// `sig` is dropped from now on (SIGKILL cannot be ignored).
pub fn ignore(sig: u32) -> Result<u64, crate::fs::Error> {
    sigaction(sig, SIG_IGN)
}

/// Back to the default action (ending the process).
pub fn reset(sig: u32) -> Result<u64, crate::fs::Error> {
    sigaction(sig, SIG_DFL)
}

/// Sends `sig` to process `pid` (0 = only check that it exists).
pub fn send(pid: u64, sig: u32) -> Result<(), crate::fs::Error> {
    let ret = sys::kill(pid, sig);
    if ret < 0 { Err(crate::fs::Error(-ret)) } else { Ok(()) }
}

/// Sends `sig` to this process; a handler runs before this returns.
pub fn raise(sig: u32) -> Result<(), crate::fs::Error> {
    send(sys::getpid(), sig)
}
