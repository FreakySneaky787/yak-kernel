//! x87 FPU / SSE state of user programs.
//!
//! The kernel itself is built soft-float and never touches these registers.
//! User programs are built with SSE2 (hardware floating point, vectorised
//! copies), so every thread owns a 512-byte FXSAVE area: on a context switch
//! the scheduler saves the outgoing thread's x87/MMX/XMM registers and MXCSR
//! (`fxsave64`) and loads the incoming thread's (`fxrstor64`). Switching is
//! eager (always), which is simple and costs two 512-byte copies per switch.
//!
//! `init` enables the hardware: CR0.EM off (no emulation), CR0.MP and CR0.NE
//! on (native x87 error reporting), CR4.OSFXSR (SSE + FXSAVE) and
//! CR4.OSXMMEXCPT (SIMD exceptions as #XM instead of #UD). AVX (XSAVE) is not
//! enabled; AVX instructions fault and kill the program.

use core::arch::asm;
use spin::Once;

const CR0_MP: u64 = 1 << 1;
const CR0_EM: u64 = 1 << 2;
const CR0_TS: u64 = 1 << 3;
const CR0_NE: u64 = 1 << 5;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;

/// MXCSR after reset: all SIMD exceptions masked, round to nearest.
pub const DEFAULT_MXCSR: u32 = 0x1F80;
/// x87 control word after `fninit`: all exceptions masked, 64-bit precision.
pub const DEFAULT_FCW: u16 = 0x037F;

/// The memory image of `fxsave64` (Intel SDM Vol. 1, 10.5.1).
#[repr(C, align(16))]
#[derive(Clone)]
pub struct FxState(pub [u8; 512]);

impl FxState {
    /// The state a new thread starts with (clean registers, default modes).
    pub fn initial() -> FxState {
        DEFAULT_STATE.get().cloned().unwrap_or(FxState([0; 512]))
    }

    pub fn fcw(&self) -> u16 {
        u16::from_le_bytes([self.0[0], self.0[1]])
    }

    pub fn mxcsr(&self) -> u32 {
        u32::from_le_bytes(self.0[24..28].try_into().unwrap())
    }
}

static DEFAULT_STATE: Once<FxState> = Once::new();

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

/// Enables x87 + SSE and records the clean initial state.
pub fn init() {
    unsafe {
        let cr0 = (read_cr0() & !(CR0_EM | CR0_TS)) | CR0_MP | CR0_NE;
        asm!("mov cr0, {}", in(reg) cr0, options(nostack, preserves_flags));
        let cr4 = read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT;
        asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags));
        let mxcsr = DEFAULT_MXCSR;
        asm!("fninit", "ldmxcsr [{}]", in(reg) &mxcsr, options(nostack));
    }
    DEFAULT_STATE.call_once(|| {
        let mut state = FxState([0; 512]);
        unsafe { save(&mut state) };
        // the XMM registers were not touched by anyone yet, but make sure
        state.0[160..416].fill(0);
        state
    });
}

/// Stores the current x87/SSE registers into `state`.
///
/// # Safety
/// `init` must have run (CR4.OSFXSR).
pub unsafe fn save(state: &mut FxState) {
    unsafe { asm!("fxsave64 [{}]", in(reg) state.0.as_mut_ptr(), options(nostack, preserves_flags)) };
}

/// Loads the x87/SSE registers from `state`.
///
/// # Safety
/// `state` must come from `save` or `FxState::initial` (reserved bits of
/// MXCSR must be zero, or `fxrstor` faults).
pub unsafe fn restore(state: &FxState) {
    unsafe { asm!("fxrstor64 [{}]", in(reg) state.0.as_ptr(), options(nostack, preserves_flags)) };
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
        let s = FxState::initial();
        assert_eq!(s.fcw(), DEFAULT_FCW);
        assert_eq!(s.mxcsr(), DEFAULT_MXCSR);
        assert!(s.0[160..416].iter().all(|&b| b == 0), "XMM registers not clean");
    }

    #[test_case]
    fn save_and_restore_round_trip() {
        let mut before = FxState([0; 512]);
        let mut after = FxState([0; 512]);
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
