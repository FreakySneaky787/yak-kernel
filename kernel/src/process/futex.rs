//! Futexes ("fast user-space mutexes"): the kernel half of user-space locks.
//!
//! User code keeps a lock in a 32-bit word and only calls the kernel when it
//! has to wait: `wait(addr, expected)` blocks if the word still holds
//! `expected`, `wake(addr, n)` wakes up to `n` threads waiting on it. A key
//! is the pair (process, address).
//!
//! No lost wake-ups: `wait` checks the word and registers the thread under
//! the `FUTEXES` lock; a waker changes the word *before* calling `wake`,
//! which takes the same lock. So either the waiter sees the new value
//! (`EAGAIN`) or it is registered and gets woken.

use super::{signal_pending, with_current};
use crate::arch::x86_64::without_interrupts;
use crate::task::{self, Tid, Wait};
use crate::time;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use spin::Mutex;

struct Futexes {
    /// Waiting threads per key, oldest first.
    waiters: BTreeMap<u64, VecDeque<Tid>>,
    /// Threads taken off a queue by `wake` that have not noticed yet.
    woken: BTreeSet<Tid>,
}

/// Only taken with interrupts disabled.
static FUTEXES: Mutex<Futexes> = Mutex::new(Futexes {
    waiters: BTreeMap::new(),
    woken: BTreeSet::new(),
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FutexError {
    /// The word did not hold the expected value.
    WouldBlock,
    TimedOut,
    Interrupted,
    /// Not a readable, 4-byte aligned user address.
    Fault,
}

/// User addresses are below 2^47, process ids are small: both fit.
fn key(addr: u64) -> u64 {
    task::current_owner() << 47 | addr
}

/// Blocks until `wake` is called for `addr` - if the 32-bit word there
/// still holds `expected`. `timeout_ms` 0 = no timeout.
pub fn wait(addr: u64, expected: u32, timeout_ms: u64) -> Result<(), FutexError> {
    if addr % 4 != 0 {
        return Err(FutexError::Fault);
    }
    let me = task::current_id();
    let key = key(addr);
    let registered = without_interrupts(|| {
        let mut f = FUTEXES.lock();
        // read under the process table lock: no other thread can unmap the page meanwhile
        let value = with_current(|p| {
            p.populate(addr, 4);
            p.space
                .as_ref()
                .filter(|s| s.is_accessible(addr, 4, false))
                .map(|_| unsafe { core::ptr::read_volatile(addr as *const u32) })
        })
        .flatten()
        .ok_or(FutexError::Fault)?;
        if value != expected {
            return Err(FutexError::WouldBlock);
        }
        f.waiters.entry(key).or_default().push_back(me);
        Ok(())
    });
    registered?;
    let deadline = (timeout_ms > 0).then(|| time::ticks() + timeout_ms.div_ceil(time::MS_PER_TICK));
    let woken = || without_interrupts(|| FUTEXES.lock().woken.contains(&me));
    let timed_out = || deadline.is_some_and(|d| time::ticks() >= d);
    // with a timeout the timer must wake us: sleep "until", `wake` reaches us anyway
    let reason = deadline.map_or(Wait::Futex(key), Wait::Until);
    task::wait_for(reason, || woken() || signal_pending() || timed_out());
    let was_woken = without_interrupts(|| {
        let mut f = FUTEXES.lock();
        if f.woken.remove(&me) {
            return true;
        }
        if let Some(q) = f.waiters.get_mut(&key) {
            q.retain(|&t| t != me);
            if q.is_empty() {
                f.waiters.remove(&key);
            }
        }
        false
    });
    match () {
        _ if was_woken => Ok(()),
        _ if timed_out() => Err(FutexError::TimedOut),
        _ => Err(FutexError::Interrupted),
    }
}

/// Wakes up to `n` threads waiting on `addr`; returns how many.
pub fn wake(addr: u64, n: usize) -> usize {
    let key = key(addr);
    let threads: alloc::vec::Vec<Tid> = without_interrupts(|| {
        let mut f = FUTEXES.lock();
        let Some(q) = f.waiters.get_mut(&key) else {
            return alloc::vec::Vec::new();
        };
        let taken: alloc::vec::Vec<Tid> = (0..n).map_while(|_| q.pop_front()).collect();
        if q.is_empty() {
            f.waiters.remove(&key);
        }
        f.woken.extend(taken.iter().copied());
        taken
    });
    for &t in &threads {
        task::wake(t);
    }
    threads.len()
}
