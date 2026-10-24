//! The `syscall` / `sysret` fast system call path and the first jump into
//! ring 3.
//!
//! `syscall` (executed by user code) loads RIP from LSTAR and the kernel
//! CS/SS from STAR, saves the user RIP in RCX and RFLAGS in R11 and clears
//! the RFLAGS bits in FMASK (we clear IF, so the entry runs with interrupts
//! off). It does **not** switch stacks: the entry stub swaps RSP to the
//! running thread's kernel stack itself.
//!
//! The stub needs two per-CPU slots (user RSP scratch, kernel stack top).
//! It reaches them through GS: IA32_KERNEL_GS_BASE points at this CPU's
//! `CpuLocal`, `swapgs` makes it the active GS base. The stub swaps back
//! right after it has the kernel stack, so GS is in its "user" state
//! whenever interrupts are enabled - a thread may continue on another CPU
//! after being preempted inside a system call.
//!
//! Register convention (like Linux): RAX = number, arguments in RDI, RSI,
//! RDX, R10, R8, R9; result in RAX. RCX and R11 are clobbered by the CPU,
//! every other register is preserved.

use super::gdt::{KERNEL_CODE_SELECTOR, KERNEL_DATA_SELECTOR, USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use super::msr;
use super::smp::MAX_CPUS;
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

/// Per-CPU data of the syscall stub, reached through GS (see module docs).
#[repr(C)]
struct CpuLocal {
    /// Scratch slot for the user RSP until it is pushed (gs:[0]).
    user_rsp: AtomicU64,
    /// Kernel stack top of the thread running on this CPU, same as TSS.RSP0 (gs:[8]).
    kernel_rsp: AtomicU64,
}

static CPU_LOCAL: [CpuLocal; MAX_CPUS] = [const {
    CpuLocal {
        user_rsp: AtomicU64::new(0),
        kernel_rsp: AtomicU64::new(0),
    }
}; MAX_CPUS];

const RFLAGS_IF: u64 = 1 << 9;
const RFLAGS_TF: u64 = 1 << 8;
const RFLAGS_DF: u64 = 1 << 10;
const RFLAGS_AC: u64 = 1 << 18;

core::arch::global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "swapgs", // GS base = this CPU's CpuLocal
    "mov gs:[0], rsp",
    "mov rsp, gs:[8]",
    // build a SyscallFrame (pushed in reverse order)
    "push qword ptr gs:[0]",
    "swapgs", // back to the user's GS before interrupts can happen
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
    dispatch = sym crate::process::syscall::dispatch,
    user_ss = const USER_DATA_SELECTOR as u64,
    user_cs = const USER_CODE_SELECTOR as u64,
    rflags = const RFLAGS_IF | 0x2, // bit 1 is always set
);

unsafe extern "C" {
    fn syscall_entry();
    fn enter_user_mode(rip: u64, rsp: u64, arg0: u64, arg1: u64) -> !;
}

/// Enables `syscall` on CPU `cpu` and points it at our entry stub.
pub fn init(cpu: usize) {
    unsafe {
        // GS stays 0 for user code; the stub swaps this in for itself
        msr::write(msr::IA32_KERNEL_GS_BASE, &CPU_LOCAL[cpu] as *const CpuLocal as u64);
        msr::write(msr::IA32_GS_BASE, 0);
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
    CPU_LOCAL[super::smp::cpu_index()]
        .kernel_rsp
        .store(top, Ordering::Relaxed);
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
