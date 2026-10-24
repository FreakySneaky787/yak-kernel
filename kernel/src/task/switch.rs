//! The context switch: saves the callee-saved registers of the current
//! thread on its own stack, stores its stack pointer, loads the stack pointer
//! of the next thread and restores that thread's registers.
//!
//! Caller-saved registers (rax, rcx, ...) do not need saving: the compiler
//! already assumes `switch_context` clobbers them, like any function call.
//! The kernel is built without SSE, so there is no FPU/vector state either.

core::arch::global_asm!(
    ".global kernel_switch_context",
    "kernel_switch_context:",
    // rdi = where to store the old stack pointer, rsi = new stack pointer
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
    // First code a new thread runs: `ret` above lands here with the entry
    // argument in r12 (see `initial_stack`).
    ".global kernel_thread_trampoline",
    "kernel_thread_trampoline:",
    "mov rdi, r12",
    "call {start}",
    "ud2",
    start = sym super::thread_start,
);

unsafe extern "C" {
    /// Saves the current context, stores RSP into `*old_rsp` and resumes the
    /// context whose saved stack pointer is `new_rsp`.
    pub fn kernel_switch_context(old_rsp: *mut u64, new_rsp: u64);
    fn kernel_thread_trampoline();
}

/// Prepares a fresh stack so that switching to it "returns" into the
/// trampoline, which calls `thread_start(arg)`. Returns the initial RSP.
///
/// Layout (growing down from `top`):
/// `[ret = trampoline] [rbp] [rbx] [r12 = arg] [r13] [r14] [r15]` <- RSP
pub fn initial_stack(top: u64, arg: u64) -> u64 {
    assert!(top % 16 == 0);
    let frame: [u64; 7] = [
        0,
        0,
        0,
        arg,
        0,
        0,
        kernel_thread_trampoline as unsafe extern "C" fn() as usize as u64,
    ];
    // frame[0] = r15 (lowest address) ... frame[6] = return address
    let rsp = top - frame.len() as u64 * 8;
    for (i, value) in frame.iter().enumerate() {
        unsafe { ((rsp + i as u64 * 8) as *mut u64).write(*value) };
    }
    rsp
}
