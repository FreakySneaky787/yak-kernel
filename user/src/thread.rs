//! Further threads in the same process: they share memory, files and the
//! heap; each has its own stack (`STACK_SIZE` bytes from the heap, pages
//! appear on first use - no guard page, so deep recursion in a thread is
//! not caught).
//!
//! The process ends when `main` returns or any thread calls `sys::exit`
//! (or dies of a signal or fault); the kernel then stops all threads.

use crate::fs::Error;
use crate::sys;
use alloc::boxed::Box;
use alloc::vec::Vec;

pub const STACK_SIZE: usize = 256 * 1024;

type Main = Box<dyn FnOnce() -> i32 + Send + 'static>;

core::arch::global_asm!(
    ".global ulib_thread_start",
    "ulib_thread_start:",
    "xor ebp, ebp",
    "and rsp, -16",
    "call {main}",
    "ud2",
    main = sym thread_main,
);

unsafe extern "C" {
    fn ulib_thread_start();
}

extern "C" fn thread_main(arg: *mut Main) -> ! {
    let f = unsafe { Box::from_raw(arg) };
    let code = f();
    sys::thread_exit(code)
}

/// A running thread; `join` waits for it. Dropping the handle lets the
/// thread run on (its stack is then never freed).
pub struct JoinHandle {
    tid: u64,
    stack: Option<Vec<u8>>,
}

/// Starts `f` in a new thread; its result becomes the thread's exit code.
pub fn spawn(f: impl FnOnce() -> i32 + Send + 'static) -> Result<JoinHandle, Error> {
    let stack: Vec<u8> = Vec::with_capacity(STACK_SIZE);
    let top = stack.as_ptr() as u64 + STACK_SIZE as u64;
    let main: Main = Box::new(f);
    let arg = Box::into_raw(Box::new(main));
    let entry = ulib_thread_start as *const () as u64;
    let tid = sys::thread_spawn(entry, top, arg as u64);
    if tid < 0 {
        drop(unsafe { Box::from_raw(arg) });
        return Err(Error(-tid));
    }
    Ok(JoinHandle {
        tid: tid as u64,
        stack: Some(stack),
    })
}

impl JoinHandle {
    pub fn id(&self) -> u64 {
        self.tid
    }

    /// Waits until the thread has ended; returns its exit code.
    pub fn join(mut self) -> Result<i32, Error> {
        let ret = sys::thread_join(self.tid);
        if ret < 0 {
            core::mem::forget(self.stack.take()); // it may still run on it
            return Err(Error(-ret));
        }
        drop(self.stack.take()); // the thread is gone: free its stack
        Ok(ret as u32 as i32)
    }
}

impl Drop for JoinHandle {
    fn drop(&mut self) {
        // not joined: the thread may still use its stack
        if let Some(stack) = self.stack.take() {
            core::mem::forget(stack);
        }
    }
}
