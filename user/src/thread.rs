//! Further threads in the same process: they share memory, files and the
//! heap; each has its own stack (`STACK_SIZE` bytes in a slot the kernel
//! chooses, pages appear on first use). Below every stack is an unmapped
//! guard: a thread that recurses too deeply faults and the process is
//! killed, instead of the thread overwriting other memory.
//!
//! The process ends when `main` returns or any thread calls `sys::exit`
//! (or dies of a signal or fault); the kernel then stops all threads.

use crate::fs::Error;
use crate::sys;
use alloc::boxed::Box;

/// The kernel's `THREAD_STACK_SIZE`.
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
/// thread run on (the kernel frees its stack after it has ended).
pub struct JoinHandle {
    tid: u64,
}

/// Starts `f` in a new thread; its result becomes the thread's exit code.
pub fn spawn(f: impl FnOnce() -> i32 + Send + 'static) -> Result<JoinHandle, Error> {
    let main: Main = Box::new(f);
    let arg = Box::into_raw(Box::new(main));
    let entry = ulib_thread_start as *const () as u64;
    let tid = sys::thread_spawn(entry, 0, arg as u64); // stack 0: the kernel's slot with a guard
    if tid < 0 {
        drop(unsafe { Box::from_raw(arg) });
        return Err(Error(-tid));
    }
    Ok(JoinHandle { tid: tid as u64 })
}

impl JoinHandle {
    pub fn id(&self) -> u64 {
        self.tid
    }

    /// Waits until the thread has ended; returns its exit code.
    pub fn join(self) -> Result<i32, Error> {
        let ret = sys::thread_join(self.tid);
        if ret < 0 {
            return Err(Error(-ret));
        }
        Ok(ret as u32 as i32)
    }
}
