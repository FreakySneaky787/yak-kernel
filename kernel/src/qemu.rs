//! Beenden von QEMU über das `isa-debug-exit`-Gerät (Port 0xF4).
//! QEMU beendet sich mit Exitcode `(wert << 1) | 1`.

use crate::arch::x86_64::port::outl;

#[derive(Clone, Copy)]
#[repr(u32)]
pub enum ExitCode {
    Success = 0x10, // → 33
    Failed = 0x11,  // → 35
}

pub fn exit(code: ExitCode) -> ! {
    unsafe { outl(0xF4, code as u32) };
    // Falls das Gerät fehlt (z. B. echte Hardware), einfach anhalten
    crate::arch::x86_64::halt_loop()
}
