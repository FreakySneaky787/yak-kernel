//! Model specific registers: CPU configuration registers, read/written
//! with `rdmsr`/`wrmsr` (number in ECX, value in EDX:EAX).

use core::arch::asm;

pub const IA32_APIC_BASE: u32 = 0x1B;
/// Extended feature enable register (bit 0 = SCE: `syscall`/`sysret` enabled).
pub const IA32_EFER: u32 = 0xC000_0080;
/// Segment selectors loaded by `syscall` (bits 32..47) and `sysret` (48..63).
pub const IA32_STAR: u32 = 0xC000_0081;
/// Entry point of `syscall` in 64-bit mode.
pub const IA32_LSTAR: u32 = 0xC000_0082;
/// RFLAGS bits that `syscall` clears.
pub const IA32_FMASK: u32 = 0xC000_0084;
/// Base of the GS segment (the active one).
pub const IA32_GS_BASE: u32 = 0xC000_0101;
/// The other GS base, exchanged with the active one by `swapgs`.
pub const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

pub fn read(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags)) };
    (high as u64) << 32 | low as u64
}

pub unsafe fn write(msr: u32, value: u64) {
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32,
             options(nostack, preserves_flags))
    };
}
