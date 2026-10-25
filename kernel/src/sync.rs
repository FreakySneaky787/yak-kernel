//! A sleeping mutex: a thread that finds it locked blocks (`Wait::Lock`)
//! instead of spinning, so the holder may itself block - e.g. wait for a
//! disk interrupt - without other threads burning CPU time meanwhile.
//!
//! Before the scheduler runs, or with interrupts disabled, `lock` spins
//! like a spin lock (blocking is impossible there). Unlocking wakes the
//! waiters only if there are any (`waiters`); they race for the lock again.
//! No lost wake-ups: a waiter registers itself, then `wait_for` marks it
//! blocked and checks "unlocked?" before it really sleeps.
//!
//! Not for data that interrupt handlers touch: a handler would spin on a
//! lock whose holder it interrupted.

use crate::arch::x86_64::interrupts_enabled;
use crate::task::{self, Wait};
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct Mutex<T: ?Sized> {
    locked: AtomicBool,
    waiters: AtomicUsize,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

pub struct MutexGuard<'a, T: ?Sized> {
    lock: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Mutex<T> {
        Mutex {
            locked: AtomicBool::new(false),
            waiters: AtomicUsize::new(0),
            data: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    fn key(&self) -> u64 {
        self as *const Self as *const () as u64
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            // `then`, not `then_some`: a guard built eagerly and dropped on
            // failure would unlock the holder's lock
            .then(|| MutexGuard { lock: self })
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            if task::is_running() && interrupts_enabled() {
                self.waiters.fetch_add(1, Ordering::SeqCst);
                task::wait_for(Wait::Lock(self.key()), || !self.locked.load(Ordering::SeqCst));
                self.waiters.fetch_sub(1, Ordering::SeqCst);
            } else {
                core::hint::spin_loop();
            }
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::SeqCst);
        if self.lock.waiters.load(Ordering::SeqCst) > 0 {
            task::notify(Wait::Lock(self.lock.key()));
        }
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time;
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    #[test_case]
    fn excludes_and_counts_correctly() {
        let counter = Arc::new(Mutex::new(0u64));
        let ids: Vec<_> = (0..4)
            .map(|_| {
                let c = counter.clone();
                task::spawn("mutex-test", move || {
                    for i in 0..500 {
                        let mut g = c.lock();
                        let v = *g;
                        if i % 50 == 0 {
                            task::yield_now(); // others must wait for us meanwhile
                        }
                        *g = v + 1;
                    }
                })
            })
            .collect();
        for id in ids {
            task::join(id);
        }
        assert_eq!(*counter.lock(), 2000);
    }

    #[test_case]
    fn a_waiter_sleeps_instead_of_spinning() {
        let m = Arc::new(Mutex::new(()));
        let guard = m.lock();
        let m2 = m.clone();
        let waiter = task::spawn("mutex-waiter", move || drop(m2.lock()));
        time::sleep_ms(30);
        let state = task::list().into_iter().find(|t| t.id == waiter).map(|t| t.state);
        assert!(
            matches!(state, Some(task::State::Blocked(Wait::Lock(_)))),
            "waiter is {:?}",
            state
        );
        drop(guard);
        task::join(waiter); // woken by the unlock
        assert!(!m.is_locked());
    }
}
