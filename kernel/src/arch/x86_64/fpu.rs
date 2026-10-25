//! x87 FPU / SSE / AVX state of user programs.
//!
//! The kernel itself is built soft-float and never touches these registers.
//! User programs use SSE2 (and AVX where the CPU has it), so every thread
//! owns an `FpuState`: on a context switch the scheduler saves the outgoing
//! thread's registers and loads the incoming thread's. Switching is eager
//! (always), which is simple.
//!
//! - With XSAVE (CPUID.1:ECX.XSAVE): CR4.OSXSAVE, XCR0 = x87 | SSE (| AVX),
//!   `xsave64` / `xrstor64` of the enabled components. The area size comes
//!   from CPUID 0xD (832 bytes with AVX) and must fit `STATE_SIZE`.
//! - Without: `fxsave64` / `fxrstor64` (512 bytes, x87 + SSE only); AVX
//!   instructions then fault (#UD) and kill the program.
//!
//! `init` (on every CPU) enables the hardware: CR0.EM off (no emulation),
//! CR0.MP and CR0.NE on (native x87 error reporting), CR4.OSFXSR (SSE +
//! FXSAVE) and CR4.OSXMMEXCPT (SIMD exceptions as #XM instead of #UD).

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Once;

const CR0_MP: u64 = 1 << 1;
const CR0_EM: u64 = 1 << 2;
const CR0_TS: u64 = 1 << 3;
const CR0_NE: u64 = 1 << 5;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;
const CR4_OSXSAVE: u64 = 1 << 18;

pub const XCR0_X87: u64 = 1 << 0;
pub const XCR0_SSE: u64 = 1 << 1;
pub const XCR0_AVX: u64 = 1 << 2;

/// MXCSR after reset: all SIMD exceptions masked, round to nearest.
pub const DEFAULT_MXCSR: u32 = 0x1F80;
/// x87 control word after `fninit`: all exceptions masked, 64-bit precision.
pub const DEFAULT_FCW: u16 = 0x037F;

/// Bytes reserved per thread (XSAVE needs 832 for x87 + SSE + AVX).
pub const STATE_SIZE: usize = 1024;
/// Offset of the upper halves of YMM0-15 in the (standard format) XSAVE area.
pub const AVX_OFFSET: usize = 576;

/// Whether `save`/`restore` use XSAVE, and the components they cover.
static XSAVE: AtomicBool = AtomicBool::new(false);
static XCR0: AtomicU64 = AtomicU64::new(XCR0_X87 | XCR0_SSE);

/// The memory image of `xsave64` (or `fxsave64`: the first 512 bytes),
/// Intel SDM Vol. 1, 10.5.1 and 13.4.
#[repr(C, align(64))]
#[derive(Clone)]
pub struct FpuState(pub [u8; STATE_SIZE]);

impl FpuState {
    /// The state a new thread starts with (clean registers, default modes).
    pub fn initial() -> FpuState {
        DEFAULT_STATE.get().cloned().unwrap_or(FpuState([0; STATE_SIZE]))
    }

    pub fn fcw(&self) -> u16 {
        u16::from_le_bytes([self.0[0], self.0[1]])
    }

    pub fn mxcsr(&self) -> u32 {
        u32::from_le_bytes(self.0[24..28].try_into().unwrap())
    }
}

static DEFAULT_STATE: Once<FpuState> = Once::new();

pub fn read_cr0() -> u64 {
    let v: u64;
    unsafe { asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub fn read_cr4() -> u64 {
    let v: u64;
    unsafe { asm!("mov {}, cr4", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// XCR0: the state components enabled for XSAVE (0 without XSAVE).
pub fn read_xcr0() -> u64 {
    if read_cr4() & CR4_OSXSAVE == 0 {
        return 0;
    }
    let (lo, hi): (u32, u32);
    unsafe { asm!("xgetbv", in("ecx") 0, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags)) };
    (hi as u64) << 32 | lo as u64
}

/// Whether threads have AVX state (YMM registers usable in user mode).
pub fn avx_enabled() -> bool {
    XSAVE.load(Ordering::Relaxed) && XCR0.load(Ordering::Relaxed) & XCR0_AVX != 0
}

/// Enables x87 + SSE (+ XSAVE and AVX when present) on this CPU and, the
/// first time, records the clean initial state.
pub fn init() {
    let features = super::cpu::features();
    unsafe {
        let cr0 = (read_cr0() & !(CR0_EM | CR0_TS)) | CR0_MP | CR0_NE;
        asm!("mov cr0, {}", in(reg) cr0, options(nostack, preserves_flags));
        let mut cr4 = read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT;
        if features.xsave {
            cr4 |= CR4_OSXSAVE;
        }
        asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags));
        if features.xsave {
            let xcr0 = XCR0_X87 | XCR0_SSE | if features.avx { XCR0_AVX } else { 0 };
            asm!("xsetbv", in("ecx") 0, in("eax") xcr0 as u32, in("edx") (xcr0 >> 32) as u32, options(nostack));
            // size of the XSAVE area for what XCR0 enables now
            let size = super::cpu::cpuid_sub(0xD, 0).ebx as usize;
            assert!(size <= STATE_SIZE, "XSAVE area of {} bytes does not fit", size);
            XCR0.store(xcr0, Ordering::Relaxed);
            XSAVE.store(true, Ordering::Relaxed);
        }
        let mxcsr = DEFAULT_MXCSR;
        asm!("fninit", "ldmxcsr [{}]", in(reg) &mxcsr, options(nostack));
    }
    DEFAULT_STATE.call_once(|| {
        let mut state = FpuState([0; STATE_SIZE]);
        unsafe { save(&mut state) };
        // the vector registers were not touched by anyone yet, but make sure
        state.0[160..416].fill(0);
        state.0[AVX_OFFSET..AVX_OFFSET + 256].fill(0);
        state
    });
}

/// Stores the current x87/SSE/AVX registers into `state`.
///
/// # Safety
/// `init` must have run on this CPU.
pub unsafe fn save(state: &mut FpuState) {
    let p = state.0.as_mut_ptr();
    if XSAVE.load(Ordering::Relaxed) {
        let mask = XCR0.load(Ordering::Relaxed);
        unsafe {
            asm!("xsave64 [{}]", in(reg) p, in("eax") mask as u32, in("edx") (mask >> 32) as u32,
                 options(nostack, preserves_flags))
        };
    } else {
        unsafe { asm!("fxsave64 [{}]", in(reg) p, options(nostack, preserves_flags)) };
    }
}

/// Loads the x87/SSE/AVX registers from `state`.
///
/// # Safety
/// `state` must come from `save` or `FpuState::initial` (reserved bits of
/// MXCSR and of the XSAVE header must be zero, or the restore faults).
pub unsafe fn restore(state: &FpuState) {
    let p = state.0.as_ptr();
    if XSAVE.load(Ordering::Relaxed) {
        let mask = XCR0.load(Ordering::Relaxed);
        unsafe {
            asm!("xrstor64 [{}]", in(reg) p, in("eax") mask as u32, in("edx") (mask >> 32) as u32,
                 options(nostack, preserves_flags))
        };
    } else {
        unsafe { asm!("fxrstor64 [{}]", in(reg) p, options(nostack, preserves_flags)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn fpu_and_sse_are_enabled() {
        assert_eq!(
            read_cr0() & (CR0_EM | CR0_TS),
            0,
            "x87 emulation or task-switched flag set"
        );
        assert_ne!(read_cr0() & CR0_NE, 0);
        assert_eq!(read_cr4() & (CR4_OSFXSR | CR4_OSXMMEXCPT), CR4_OSFXSR | CR4_OSXMMEXCPT);
    }

    #[test_case]
    fn initial_state_has_default_modes() {
        let s = FpuState::initial();
        assert_eq!(s.fcw(), DEFAULT_FCW);
        assert_eq!(s.mxcsr(), DEFAULT_MXCSR);
        assert!(s.0[160..416].iter().all(|&b| b == 0), "XMM registers not clean");
    }

    #[test_case]
    fn xsave_and_avx_are_on_when_the_cpu_has_them() {
        let f = crate::arch::x86_64::cpu::features();
        assert_eq!(read_cr4() & CR4_OSXSAVE != 0, f.xsave);
        if f.xsave {
            assert_eq!(read_xcr0() & XCR0_AVX != 0, f.avx);
            assert_eq!(read_xcr0() & (XCR0_X87 | XCR0_SSE), XCR0_X87 | XCR0_SSE);
        }
        assert_eq!(avx_enabled(), f.xsave && f.avx);
    }

    #[test_case]
    fn ymm_upper_halves_survive_save_and_restore() {
        if !avx_enabled() {
            return; // without AVX there is nothing to keep
        }
        let pattern: [u8; 32] = core::array::from_fn(|i| i as u8 + 1);
        let mut out = [0u8; 32];
        let mut saved = FpuState([0; STATE_SIZE]);
        let mut before = FpuState([0; STATE_SIZE]);
        unsafe {
            save(&mut before);
            asm!("vmovdqu ymm7, [{}]", in(reg) pattern.as_ptr(), options(nostack));
            save(&mut saved);
            asm!("vpxor ymm7, ymm7, ymm7", options(nomem, nostack));
            restore(&saved);
            asm!("vmovdqu [{}], ymm7", in(reg) out.as_mut_ptr(), options(nostack));
            restore(&before);
        }
        assert_eq!(out, pattern, "YMM7 not restored");
        // the upper half of YMM7 sits at AVX_OFFSET + 7 * 16 in the area
        assert_eq!(&saved.0[AVX_OFFSET + 112..AVX_OFFSET + 128], &pattern[16..]);
    }

    #[test_case]
    fn save_and_restore_round_trip() {
        let mut before = FpuState([0; STATE_SIZE]);
        let mut after = FpuState([0; STATE_SIZE]);
        unsafe {
            save(&mut before);
            let mut changed = before.clone();
            changed.0[24..28].copy_from_slice(&0x3F80u32.to_le_bytes()); // MXCSR: round toward zero
            restore(&changed);
            save(&mut after);
            restore(&before);
        }
        assert_eq!(after.mxcsr(), 0x3F80);
    }
}
