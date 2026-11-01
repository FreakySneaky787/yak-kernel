//! Time, counted in ticks of the timer interrupt: 100 per second, one every 10 ms.

use core::sync::atomic::{AtomicU64, Ordering};

pub const TICKS_PER_SECOND: u32 = 100;
pub const MS_PER_TICK: u64 = 1000 / TICKS_PER_SECOND as u64;

/// Written by the timer interrupt, read everywhere else - an atomic, so neither side ever sees half a value.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Called by the timer interrupt.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn uptime_ms() -> u64 {
    ticks() * MS_PER_TICK
}

/// How many ticks must pass so that at least `ms` milliseconds are over. One more than `ms / 10` rounded up:
/// the tick that is running now has already partly passed - counting it fully would end the sleep up to 10 ms early.
pub fn ticks_for(ms: u64) -> u64 {
    ms.div_ceil(MS_PER_TICK) + 1
}

/// Waits at least `ms` milliseconds. `hlt` stops the CPU until the next interrupt, so waiting costs no work.
/// Needs interrupts on - without them, `hlt` would never wake up.
pub fn sleep_ms(ms: u64) {
    let deadline = ticks() + ticks_for(ms);
    while ticks() < deadline {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn ticks_advance_while_the_cpu_halts() {
        assert!(crate::interrupts::enabled());
        let start = ticks();
        // every `hlt` ends with an interrupt - the timer's, 10 ms later
        for _ in 0..1000 {
            if ticks() >= start + 5 {
                break;
            }
            unsafe { core::arch::asm!("hlt") };
        }
        assert!(ticks() >= start + 5, "ticks: {} -> {}", start, ticks());
    }

    #[test_case]
    fn a_deadline_rounds_up_and_adds_one() {
        assert_eq!(ticks_for(0), 1);
        assert_eq!(ticks_for(1), 2);
        assert_eq!(ticks_for(10), 2);
        assert_eq!(ticks_for(11), 3);
        assert_eq!(ticks_for(1000), 101);
    }

    #[test_case]
    fn sleep_never_ends_early() {
        for _ in 0..5 {
            let start = ticks();
            sleep_ms(30);
            // 30 ms are 3 ticks; one more because the first tick had already begun. How late it ends depends on
            // the machine, so the upper bound is only there to catch a sleep that does not end at all
            let elapsed = ticks() - start;
            assert!((4..=40).contains(&elapsed), "sleep_ms(30) took {} ticks", elapsed);
        }
    }
}
