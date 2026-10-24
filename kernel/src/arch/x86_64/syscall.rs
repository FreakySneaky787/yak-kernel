//! The `syscall` / `sysret` fast system call path and the first jump into
//! ring 3.
//!
//! `syscall` (executed by user code) loads RIP from LSTAR and the kernel
//! CS/SS from STAR, saves the user RIP in RCX and RFLAGS in R11 and clears
//! the RFLAGS bits in FMASK (we clear IF, so the entry runs with interrupts
//! off). It does **not** switch stacks: the entry stub swaps RSP to the
//! running thread's kernel stack itself.
//!
//! Register convention (like Linux): RAX = number, arguments in RDI, RSI,
//! RDX, R10, R8, R9; result in RAX. RCX and R11 are clobbered by the CPU,
//! every other register is preserved.

use super::gdt::{KERNEL_CODE_SELECTOR, KERNEL_DATA_SELECTOR, USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use super::msr;
use core::sync::atomic::{AtomicU64, Ordering};

/// Everything the entry stub saves, lowest address first. The dispatcher may
/// change `rax` (the return value); the other fields are restored as they are.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SyscallFrame {
    pub r9: u64,
    pub r8: u64,
    pub r10: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rax: u64,
    /// User RFLAGS (saved by the CPU in R11).
    pub rflags: u64,
    /// User return address (saved by the CPU in RCX).
    pub rip: u64,
    pub rsp: u64,
}

/// Kernel stack top of the running thread (same value as TSS.RSP0).
static KERNEL_RSP: AtomicU64 = AtomicU64::new(0);
/// Scratch slot for the user RSP until it is pushed (single CPU, IF = 0).
static USER_RSP: AtomicU64 = AtomicU64::new(0);

const RFLAGS_IF: u64 = 1 << 9;
const RFLAGS_TF: u64 = 1 << 8;
const RFLAGS_DF: u64 = 1 << 10;
const RFLAGS_AC: u64 = 1 << 18;

core::arch::global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "mov [rip + {user_rsp}], rsp",
    "mov rsp, [rip + {kernel_rsp}]",
    // build a SyscallFrame (pushed in reverse order)
    "push qword ptr [rip + {user_rsp}]",
    "push rcx",
    "push r11",
    "push rax",
    "push rdi",
    "push rsi",
    "push rdx",
    "push r10",
    "push r8",
    "push r9",
    // 10 pushes from a 16-byte aligned top: RSP is aligned again for the call
    "mov rdi, rsp",
    "sti",
    "call {dispatch}",
    "cli",
    "pop r9",
    "pop r8",
    "pop r10",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    "pop rax",
    "pop r11",
    "pop rcx",
    "pop rsp",
    "sysretq",
    // enter_user_mode(rip = rdi, rsp = rsi, arg0 = rdx, arg1 = rcx) -> !
    ".global enter_user_mode",
    "enter_user_mode:",
    "cli",
    "push {user_ss}",
    "push rsi",
    "push {rflags}",
    "push {user_cs}",
    "push rdi",
    "mov rdi, rdx",
    "mov rsi, rcx",
    // no kernel values may leak into user space
    "xor eax, eax",
    "xor ebx, ebx",
    "xor ecx, ecx",
    "xor edx, edx",
    "xor ebp, ebp",
    "xor r8d, r8d",
    "xor r9d, r9d",
    "xor r10d, r10d",
    "xor r11d, r11d",
    "xor r12d, r12d",
    "xor r13d, r13d",
    "xor r14d, r14d",
    "xor r15d, r15d",
    "iretq",
    user_rsp = sym USER_RSP,
    kernel_rsp = sym KERNEL_RSP,
    dispatch = sym crate::process::syscall::dispatch,
    user_ss = const USER_DATA_SELECTOR as u64,
    user_cs = const USER_CODE_SELECTOR as u64,
    rflags = const RFLAGS_IF | 0x2, // bit 1 is always set
);

unsafe extern "C" {
    fn syscall_entry();
    fn enter_user_mode(rip: u64, rsp: u64, arg0: u64, arg1: u64) -> !;
}

/// Enables `syscall` and points it at our entry stub.
pub fn init() {
    unsafe {
        msr::write(msr::IA32_EFER, msr::read(msr::IA32_EFER) | 1);
        // sysret: CS = base + 16, SS = base + 8 (both with RPL 3), see gdt.rs
        let star = (KERNEL_DATA_SELECTOR as u64) << 48 | (KERNEL_CODE_SELECTOR as u64) << 32;
        msr::write(msr::IA32_STAR, star);
        msr::write(msr::IA32_LSTAR, syscall_entry as unsafe extern "C" fn() as usize as u64);
        msr::write(msr::IA32_FMASK, RFLAGS_IF | RFLAGS_TF | RFLAGS_DF | RFLAGS_AC);
    }
}

/// Tells the CPU (TSS.RSP0) and the syscall stub which kernel stack belongs
/// to the thread that runs next. Call with interrupts disabled.
pub fn set_kernel_stack(top: u64) {
    super::gdt::set_kernel_stack(top);
    KERNEL_RSP.store(top, Ordering::Relaxed);
}

/// Drops to ring 3 at `rip` with stack `rsp`; `arg0`/`arg1` arrive in RDI/RSI.
/// The current kernel stack is abandoned (the next interrupt from user mode
/// starts again at its top).
///
/// # Safety
/// The active address space must map `rip` and `rsp` as user pages, and the
/// kernel stack must have been registered with `set_kernel_stack`.
pub unsafe fn enter_user(rip: u64, rsp: u64, arg0: u64, arg1: u64) -> ! {
    unsafe { enter_user_mode(rip, rsp, arg0, arg1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn msrs_are_programmed() {
        assert_eq!(msr::read(msr::IA32_EFER) & 1, 1, "SCE not set");
        assert_eq!(
            msr::read(msr::IA32_LSTAR),
            syscall_entry as unsafe extern "C" fn() as usize as u64
        );
        assert_eq!(msr::read(msr::IA32_FMASK) & RFLAGS_IF, RFLAGS_IF);
        assert_eq!(core::mem::size_of::<SyscallFrame>(), 10 * 8);
    }
}
