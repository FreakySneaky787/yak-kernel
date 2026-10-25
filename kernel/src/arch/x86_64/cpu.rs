//! CPU identification via the `cpuid` instruction.

use alloc::string::String;
use core::arch::x86_64::{__cpuid, __cpuid_count, CpuidResult};
use core::sync::atomic::{AtomicBool, Ordering};

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
    /// XSAVE/XRSTOR and XCR0
    pub xsave: bool,
    pub avx: bool,
    /// supervisor mode execution / access prevention (CPUID 7)
    pub smep: bool,
    pub smap: bool,
    pub apic: bool,
    pub x2apic: bool,
    pub hypervisor: bool,
    pub nx: bool,
    pub long_mode: bool,
}

pub fn features() -> Features {
    let basic = cpuid(1);
    let ext = if max_extended_leaf() >= 0x8000_0001 {
        cpuid(0x8000_0001)
    } else {
        CpuidResult {
            eax: 0,
            ebx: 0,
            ecx: 0,
            edx: 0,
        }
    };
    let leaf7 = if cpuid(0).eax >= 7 { cpuid(7).ebx } else { 0 };
    Features {
        sse: basic.edx & (1 << 25) != 0,
        sse2: basic.edx & (1 << 26) != 0,
        xsave: basic.ecx & (1 << 26) != 0,
        avx: basic.ecx & (1 << 28) != 0,
        smep: leaf7 & (1 << 7) != 0,
        smap: leaf7 & (1 << 20) != 0,
        apic: basic.edx & (1 << 9) != 0,
        x2apic: basic.ecx & (1 << 21) != 0,
        hypervisor: basic.ecx & (1 << 31) != 0,
        nx: ext.edx & (1 << 20) != 0,
        long_mode: ext.edx & (1 << 29) != 0,
    }
}

/// `cpuid` with a sub-leaf, for other modules (XSAVE sizes).
pub fn cpuid_sub(leaf: u32, sub: u32) -> CpuidResult {
    #[allow(unused_unsafe)]
    unsafe {
        __cpuid_count(leaf, sub)
    }
}

const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;

/// Whether the CPU has SMAP (then `stac`/`clac` exist and `user_access` uses them).
static SMAP: AtomicBool = AtomicBool::new(false);

/// Turns on SMEP (the kernel cannot execute user pages) and SMAP (the
/// kernel cannot read or write user pages except inside `user_access`) on
/// this CPU, if it has them. Call on every CPU, before user code runs.
pub fn enable_protection() {
    let f = features();
    if f.smap {
        SMAP.store(true, Ordering::Relaxed);
    }
    let mut cr4 = super::fpu::read_cr4();
    if f.smep {
        cr4 |= CR4_SMEP;
    }
    if f.smap {
        cr4 |= CR4_SMAP;
    }
    unsafe { core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags)) };
}

/// (SMEP, SMAP) as enabled in CR4 of this CPU.
pub fn protection() -> (bool, bool) {
    let cr4 = super::fpu::read_cr4();
    (cr4 & CR4_SMEP != 0, cr4 & CR4_SMAP != 0)
}

/// Runs `f`, which reads or writes user memory, with SMAP lifted (`stac` /
/// `clac`, RFLAGS.AC). `f` must not block or be long: the window is per CPU.
#[inline]
pub fn user_access<R>(f: impl FnOnce() -> R) -> R {
    let smap = SMAP.load(Ordering::Relaxed);
    if smap {
        // no `nomem`: the compiler must not move memory accesses across
        unsafe { core::arch::asm!("stac", options(nostack)) };
    }
    let r = f();
    if smap {
        unsafe { core::arch::asm!("clac", options(nostack)) };
    }
    r
}

#[test_case]
fn protection_is_on_when_the_cpu_has_it() {
    let f = features();
    assert_eq!(protection(), (f.smep, f.smap));
}

#[test_case]
fn cpuid_reports_sane_values() {
    assert_eq!(vendor().len(), 12);
    let f = features();
    assert!(f.long_mode && f.nx, "we are running in long mode with NX enabled");
}
