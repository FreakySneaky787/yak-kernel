//! I/O ports: besides memory, x86 has a second, 16-bit address space for devices, reached with the `in` and `out`
//! instructions. The serial port, the interrupt controller and the timer all live there - so the two functions that
//! were private to `serial.rs` until now get a module of their own.

/// Writes `value` to an I/O port (the x86 `out` instruction).
pub unsafe fn outb(port: u16, value: u8) {
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) };
}

/// Reads a byte from an I/O port (the x86 `in` instruction).
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Writes 32 bits to an I/O port - PCI's configuration ports take whole double words (chapter 16).
pub unsafe fn outl(port: u16, value: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags))
    };
}

/// Reads 32 bits from an I/O port.
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags))
    };
    value
}
