//! Exit QEMU through the `isa-debug-exit` device (port 0xF4).
//! QEMU exits with status `(value << 1) | 1`.

use crate::arch::x86_64::port::outl;

#[derive(Clone, Copy)]
#[repr(u32)]
pub enum ExitCode {
    Success = 0x10, // -> 33
    Failed = 0x11,  // -> 35
}

pub fn exit(code: ExitCode) -> ! {
    unsafe { outl(0xF4, code as u32) };
    // If the device is missing (e.g. real hardware), just halt
    crate::arch::x86_64::halt_loop()
}
