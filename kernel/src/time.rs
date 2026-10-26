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

/// Microseconds since boot (from the time stamp counter, see `arch::x86_64::tsc`).
pub fn uptime_us() -> u64 {
    crate::arch::x86_64::tsc::uptime_us()
}

pub fn uptime_ms() -> u64 {
    ticks() * MS_PER_TICK
}

/// Sleeps for at least `ms` milliseconds (resolution: one tick = 10 ms).
/// With the scheduler running the thread blocks and other threads run;
/// before that (early boot) the CPU halts until the deadline.
/// Requires interrupts to be enabled.
pub fn sleep_ms(ms: u64) {
    sleep_ms_or_abort(ms, || false);
}

/// Like `sleep_ms`, but returns early (false) once `abort()` is true - it is
/// checked whenever the thread is woken (used for signals).
pub fn sleep_ms_or_abort(ms: u64, abort: impl Fn() -> bool) -> bool {
    // + 1: the tick that is already running counts only partly - without it a sleep could end
    // up to one tick early (bug #18)
    let target = ticks().saturating_add(ms.div_ceil(MS_PER_TICK) + 1);
    if crate::task::is_running() {
        crate::task::wait_for(crate::task::Wait::Until(target), || ticks() >= target || abort());
        return ticks() >= target || !abort();
    }
    while ticks() < target {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
    true
}
