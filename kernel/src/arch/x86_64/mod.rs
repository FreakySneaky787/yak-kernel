pub mod port;

/// Hält die CPU an, bis der nächste Interrupt kommt – für immer.
pub fn halt_loop() -> ! {
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
    }
}
