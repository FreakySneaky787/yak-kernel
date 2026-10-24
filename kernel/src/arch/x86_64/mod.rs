pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod lapic;
pub mod msr;
pub mod irq;
pub mod pic;
pub mod pit;
pub mod port;

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
