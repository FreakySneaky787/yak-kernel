//! Time stamp counter: a 64-bit counter in every CPU that increases at a
//! fixed rate (on modern CPUs and in QEMU). Reading it (`rdtsc`) costs a few
//! cycles, so it gives microsecond time stamps between the 10 ms timer ticks.
//!
//! The rate is unknown, so `init` measures it once against PIT channel 2
//! (whose input clock is exactly 1.193182 MHz), like the LAPIC timer.
//! The counters of all CPUs start together at reset and are assumed to stay
//! in step (QEMU keeps them synchronised).

use super::pit;
use core::sync::atomic::{AtomicU64, Ordering};

static HZ: AtomicU64 = AtomicU64::new(0);
static BOOT: AtomicU64 = AtomicU64::new(0);

pub fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Measures the TSC frequency (takes 50 ms). Interrupts may be off.
pub fn init() {
    const CALIBRATION_MS: u32 = 50;
    pit::channel2_start_oneshot(CALIBRATION_MS);
    let start = rdtsc();
    pit::channel2_wait();
    let elapsed = rdtsc() - start;
    HZ.store(elapsed * 1000 / CALIBRATION_MS as u64, Ordering::Relaxed);
    BOOT.store(start, Ordering::Relaxed);
}

/// Measured counter frequency (0 before `init`).
pub fn hz() -> u64 {
    HZ.load(Ordering::Relaxed)
}

/// Microseconds since the calibration (0 before `init`).
pub fn uptime_us() -> u64 {
    let hz = hz();
    if hz == 0 {
        return 0;
    }
    let delta = rdtsc().saturating_sub(BOOT.load(Ordering::Relaxed));
    (delta as u128 * 1_000_000 / hz as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn tsc_is_calibrated_and_monotonic() {
        assert!(hz() > 1_000_000, "TSC frequency {} Hz is implausible", hz());
        let a = uptime_us();
        let b = uptime_us();
        assert!(b >= a);
    }

    #[test_case]
    fn tsc_agrees_with_the_timer() {
        let (t0, us0) = (crate::time::ticks(), uptime_us());
        crate::time::sleep_ms(100);
        let (t1, us1) = (crate::time::ticks(), uptime_us());
        let tick_us = (t1 - t0) * crate::time::MS_PER_TICK * 1000;
        let tsc_us = us1 - us0;
        // the tick clock has 10 ms resolution; QEMU's timing jitters a little
        assert!(
            tsc_us.abs_diff(tick_us) <= 25_000,
            "TSC {} µs vs ticks {} µs",
            tsc_us,
            tick_us
        );
    }
}
