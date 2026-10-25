//! Locks for programs with several threads, built on futexes.
//!
//! `RawLock` is the classic three-state futex lock: 0 = free, 1 = locked,
//! 2 = locked and somebody may be waiting. Taking a free lock and releasing
//! an uncontended one need no system call; only a waiting thread sleeps in
//! the kernel (`futex_wait`) and only then does unlocking call `futex_wake`.

use crate::sys;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

const FREE: u32 = 0;
const LOCKED: u32 = 1;
const CONTENDED: u32 = 2;

pub struct RawLock {
    state: AtomicU32,
}

pub struct RawGuard<'a> {
    lock: &'a RawLock,
}

impl RawLock {
    pub const fn new() -> RawLock {
        RawLock {
            state: AtomicU32::new(FREE),
        }
    }

    pub fn lock(&self) -> RawGuard<'_> {
        if self
            .state
            .compare_exchange(FREE, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // mark it contended; sleep while somebody else holds it
            while self.state.swap(CONTENDED, Ordering::Acquire) != FREE {
                sys::futex_wait(&self.state, CONTENDED, 0);
            }
        }
        RawGuard { lock: self }
    }
}

impl Default for RawLock {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RawGuard<'_> {
    fn drop(&mut self) {
        if self.lock.state.swap(FREE, Ordering::Release) == CONTENDED {
            sys::futex_wake(&self.lock.state, 1);
        }
    }
}

/// A mutex protecting a value, like `std::sync::Mutex` (without poisoning).
pub struct Mutex<T: ?Sized> {
    lock: RawLock,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

pub struct MutexGuard<'a, T: ?Sized> {
    _raw: RawGuard<'a>,
    data: &'a mut T,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Mutex<T> {
        Mutex {
            lock: RawLock::new(),
            data: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let raw = self.lock.lock();
        MutexGuard {
            _raw: raw,
            data: unsafe { &mut *self.data.get() },
        }
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.data
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.data
    }
}
