//! Local APIC - the modern interrupt controller in every CPU core.
//!
//! As long as we use the legacy 8259 PIC, we disable the local APIC:
//! Limine masks its LINT0 input (through which PIC interrupts arrive as "ExtINT"),
//! see docs/BUGS.md #2. With the APIC globally disabled,
//! PIC interrupts go straight to the CPU's INTR pin.

use super::msr::{self, IA32_APIC_BASE};

const APIC_GLOBAL_ENABLE: u64 = 1 << 11;

pub fn is_enabled() -> bool {
    msr::read(IA32_APIC_BASE) & APIC_GLOBAL_ENABLE != 0
}

pub fn disable() {
    let value = msr::read(IA32_APIC_BASE);
    unsafe { msr::write(IA32_APIC_BASE, value & !APIC_GLOBAL_ENABLE) };
}
