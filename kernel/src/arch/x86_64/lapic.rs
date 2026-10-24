//! Local APIC – der moderne Interrupt-Controller in jedem CPU-Kern.
//!
//! Solange wir den alten 8259-PIC benutzen, schalten wir den Local APIC ab:
//! Limine maskiert seinen Eingang LINT0 (über den PIC-Interrupts als „ExtINT“
//! hereinkommen), siehe docs/BUGS.md #2. Ist der APIC global abgeschaltet,
//! landen PIC-Interrupts direkt am INTR-Pin der CPU.

use super::msr::{self, IA32_APIC_BASE};

const APIC_GLOBAL_ENABLE: u64 = 1 << 11;

pub fn is_enabled() -> bool {
    msr::read(IA32_APIC_BASE) & APIC_GLOBAL_ENABLE != 0
}

pub fn disable() {
    let value = msr::read(IA32_APIC_BASE);
    unsafe { msr::write(IA32_APIC_BASE, value & !APIC_GLOBAL_ENABLE) };
}
