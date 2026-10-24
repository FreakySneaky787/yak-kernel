//! CPU identification via the `cpuid` instruction.

use alloc::string::String;
use core::arch::x86_64::{__cpuid, __cpuid_count, CpuidResult};

#[allow(unused_unsafe)] // `__cpuid` is unsafe on older toolchains
fn cpuid(leaf: u32) -> CpuidResult {
    unsafe { __cpuid_count(leaf, 0) }
}

#[allow(unused_unsafe)]
fn max_extended_leaf() -> u32 {
    unsafe { __cpuid(0x8000_0000) }.eax
}

fn push_reg(s: &mut String, reg: u32) {
    for b in reg.to_le_bytes() {
        if b != 0 {
            s.push(b as char);
        }
    }
}

/// Vendor string, e.g. "GenuineIntel", "AuthenticAMD".
pub fn vendor() -> String {
    let r = cpuid(0);
    let mut s = String::new();
    for reg in [r.ebx, r.edx, r.ecx] {
        push_reg(&mut s, reg);
    }
    s
}

/// Processor brand string, e.g. "QEMU Virtual CPU version 2.5+".
pub fn brand() -> Option<String> {
    if max_extended_leaf() < 0x8000_0004 {
        return None;
    }
    let mut s = String::new();
    for leaf in 0x8000_0002..=0x8000_0004 {
        let r = cpuid(leaf);
        for reg in [r.eax, r.ebx, r.ecx, r.edx] {
            push_reg(&mut s, reg);
        }
    }
    Some(String::from(s.trim()))
}

pub struct Features {
    pub sse: bool,
    pub sse2: bool,
    pub apic: bool,
    pub x2apic: bool,
    pub hypervisor: bool,
    pub nx: bool,
    pub long_mode: bool,
}

pub fn features() -> Features {
    let basic = cpuid(1);
    let ext = if max_extended_leaf() >= 0x8000_0001 { cpuid(0x8000_0001) } else { CpuidResult { eax: 0, ebx: 0, ecx: 0, edx: 0 } };
    Features {
        sse: basic.edx & (1 << 25) != 0,
        sse2: basic.edx & (1 << 26) != 0,
        apic: basic.edx & (1 << 9) != 0,
        x2apic: basic.ecx & (1 << 21) != 0,
        hypervisor: basic.ecx & (1 << 31) != 0,
        nx: ext.edx & (1 << 20) != 0,
        long_mode: ext.edx & (1 << 29) != 0,
    }
}

#[test_case]
fn cpuid_reports_sane_values() {
    assert_eq!(vendor().len(), 12);
    let f = features();
    assert!(f.long_mode && f.nx, "we are running in long mode with NX enabled");
}
