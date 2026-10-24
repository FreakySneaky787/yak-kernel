pub mod gdt;
pub mod port;

use core::arch::asm;

/// Hält die CPU an, bis der nächste Interrupt kommt – für immer.
pub fn halt_loop() -> ! {
    loop {
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}

/// Wartet mit `hlt` auf Interrupts (Interrupts bleiben an).
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

/// Führt `f` mit gesperrten Interrupts aus und stellt den vorherigen Zustand
/// wieder her. Nötig, wenn Code einen Lock hält, den auch ein
/// Interrupt-Handler nehmen könnte (sonst Deadlock).
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
