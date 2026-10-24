//! Kernel time base: ticks from the PIT timer.

use core::sync::atomic::{AtomicU64, Ordering};

pub const TIMER_HZ: u32 = 100;
pub const MS_PER_TICK: u64 = 1000 / TIMER_HZ as u64;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Called from the timer interrupt.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn uptime_ms() -> u64 {
    ticks() * MS_PER_TICK
}

/// Sleeps for at least `ms` milliseconds (resolution: one tick = 10 ms).
/// Requires interrupts to be enabled.
pub fn sleep_ms(ms: u64) {
    let target = ticks() + ms.div_ceil(MS_PER_TICK);
    while ticks() < target {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}
