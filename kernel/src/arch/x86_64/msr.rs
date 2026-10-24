//! Model Specific Registers: CPU-Konfigurationsregister, gelesen/geschrieben
//! mit `rdmsr`/`wrmsr` (Nummer in ECX, Wert in EDX:EAX).

use core::arch::asm;

pub const IA32_APIC_BASE: u32 = 0x1B;

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
