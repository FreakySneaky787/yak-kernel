pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod ioapic;
pub mod lapic;
pub mod msr;
pub mod irq;
pub mod pic;
pub mod pit;
pub mod port;
pub mod power;

use core::arch::asm;

/// Halts the CPU forever with interrupts disabled.
pub fn halt_loop() -> ! {
    loop {
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}

/// Waits for interrupts with `hlt` forever (interrupts stay enabled).
pub fn idle_loop() -> ! {
    loop {
        unsafe { asm!("sti; hlt", options(nomem, nostack)) };
    }
}

pub fn interrupts_enabled() -> bool {
    let rflags: u64;
    unsafe { asm!("pushfq; pop {}", out(reg) rflags, options(nomem, preserves_flags)) };
    rflags & (1 << 9) != 0 // IF-Flag
}

#[inline]
pub fn enable_interrupts() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}

#[inline]
pub fn disable_interrupts() {
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Switches to the stack whose top is `top` and calls `entry` on it.
/// The old stack is abandoned; `entry` must never return.
///
/// # Safety
/// `top` must be the 16-byte aligned top of a mapped, otherwise unused stack.
pub unsafe fn switch_stack(top: u64, entry: extern "C" fn() -> !) -> ! {
    unsafe {
        asm!(
            "mov rsp, {top}",
            "xor ebp, ebp", // end of the frame pointer chain (for backtraces)
            "call {entry}",
            "ud2",
            top = in(reg) top,
            entry = in(reg) entry,
            options(noreturn),
        )
    }
}

/// Runs `f` with interrupts disabled and restores the previous state
/// afterwards. Needed whenever code holds a lock that an
/// interrupt handler might also take (otherwise: deadlock).
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let was_enabled = interrupts_enabled();
    if was_enabled {
        disable_interrupts();
    }
    let result = f();
    if was_enabled {
        enable_interrupts();
    }
    result
}
