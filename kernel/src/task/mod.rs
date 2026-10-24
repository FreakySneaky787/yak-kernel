//! Kernel threads and a preemptive round-robin scheduler (single CPU).
//!
//! - Every thread has its own guarded kernel stack (`mem::stack`).
//! - The LAPIC timer calls `on_tick` 100 times per second; if another thread
//!   is ready, the running one is preempted.
//! - Threads can block: sleeping until a tick, waiting for keyboard/serial
//!   input, or waiting for another thread to exit (join). Interrupt handlers
//!   and `exit` wake them up again.
//! - When nothing is ready, the idle thread halts the CPU.
//! - A thread can carry a user address space (`cr3`) and leave for ring 3
//!   (`spawn_user`); on every switch the scheduler loads the next thread's
//!   CR3 and registers its kernel stack for interrupts and syscalls from ring 3.
//!
//! Locking rule: the scheduler state is only touched with interrupts
//! disabled. `schedule()` drops the lock *before* switching stacks; with
//! interrupts off and a single CPU nothing can run in between.

mod switch;

use crate::arch::x86_64::fpu::{self, FxState};
use crate::arch::x86_64::{disable_interrupts, enable_interrupts, interrupts_enabled, syscall, without_interrupts};
use crate::mem::PhysAddr;
use crate::mem::paging;
use crate::mem::stack::{self, KernelStack};
use crate::time;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

pub type Tid = u64;

/// Stack size of spawned threads (pages).
pub const THREAD_STACK_PAGES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Sleeping until `time::ticks()` reaches this value.
    Until(u64),
    /// Waiting for keyboard or serial input.
    Input,
    /// Waiting for thread `Tid` to exit.
    Join(Tid),
    /// Waiting for data in / room in the pipe with this id.
    Pipe(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    Running,
    Blocked(Wait),
    Dead,
}

type Entry = Box<dyn FnOnce() + Send + 'static>;

/// Where a user thread starts in ring 3; `arg0`/`arg1` arrive in RDI/RSI.
#[derive(Debug, Clone, Copy)]
pub struct UserEntry {
    pub rip: u64,
    pub rsp: u64,
    pub arg0: u64,
    pub arg1: u64,
}

struct Thread {
    id: Tid,
    name: String,
    state: State,
    /// Saved stack pointer while the thread is not running.
    rsp: u64,
    /// `None` for the boot thread (it runs on a stack we did not allocate).
    stack: Option<KernelStack>,
    cpu_ticks: u64,
    /// Address space (PML4) the thread runs in; the kernel's for kernel threads.
    cr3: PhysAddr,
    /// Set for user threads until they enter ring 3.
    user_entry: Option<UserEntry>,
    /// x87/SSE registers while the thread is not running (see `arch::x86_64::fpu`).
    fpu: Box<FxState>,
}

struct Scheduler {
    threads: BTreeMap<Tid, Box<Thread>>,
    ready: VecDeque<Tid>,
    current: Tid,
    idle: Tid,
    next_id: Tid,
}

static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Snapshot of a thread for `ps`.
#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub id: Tid,
    pub name: String,
    pub state: State,
    pub cpu_ticks: u64,
}

impl Scheduler {
    fn thread(&mut self, id: Tid) -> &mut Thread {
        self.threads.get_mut(&id).expect("unknown thread id")
    }

    fn make_ready(&mut self, id: Tid) {
        let t = self.thread(id);
        if t.state != State::Ready && t.state != State::Running {
            t.state = State::Ready;
        }
        if id != self.idle && !self.ready.contains(&id) {
            self.ready.push_back(id);
        }
    }

    /// Wakes every blocked thread whose wait condition `matches`.
    fn wake(&mut self, matches: impl Fn(Wait) -> bool) {
        let woken: Vec<Tid> = self
            .threads
            .values()
            .filter(|t| matches!(t.state, State::Blocked(w) if matches(w)))
            .map(|t| t.id)
            .collect();
        for id in woken {
            self.make_ready(id);
        }
    }

    /// Frees dead threads (never the current one: we may still be on its stack).
    fn reap(&mut self) {
        let dead: Vec<Tid> = self
            .threads
            .values()
            .filter(|t| t.state == State::Dead && t.id != self.current)
            .map(|t| t.id)
            .collect();
        for id in dead {
            if let Some(thread) = self.threads.remove(&id) {
                if let Some(stack) = thread.stack {
                    stack::free_stack(stack);
                }
            }
        }
    }
}

/// Turns the currently running code into thread 0 ("main") and creates the
/// idle thread. Called once from `kernel::init` (needs heap and paging).
pub fn init() {
    without_interrupts(|| {
        let mut threads = BTreeMap::new();
        threads.insert(
            0,
            Box::new(Thread {
                id: 0,
                name: String::from("main"),
                state: State::Running,
                rsp: 0,
                stack: None,
                cpu_ticks: 0,
                cr3: paging::kernel_root(),
                user_entry: None,
                fpu: Box::new(FxState::initial()),
            }),
        );
        *SCHEDULER.lock() = Some(Scheduler {
            threads,
            ready: VecDeque::new(),
            current: 0,
            idle: 0,
            next_id: 1,
        });
        let idle = spawn_internal("idle", Box::new(idle_loop));
        let mut guard = SCHEDULER.lock();
        let s = guard.as_mut().unwrap();
        s.ready.retain(|&id| id != idle);
        s.idle = idle;
        RUNNING.store(true, Ordering::Release);
    });
}

/// Whether the scheduler is active (false very early during boot).
pub fn is_running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

fn idle_loop() {
    loop {
        unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
    }
}

fn spawn_internal(name: &str, entry: Entry) -> Tid {
    spawn_with(name, entry, paging::kernel_root(), None)
}

fn spawn_with(name: &str, entry: Entry, cr3: PhysAddr, user_entry: Option<UserEntry>) -> Tid {
    let stack = stack::alloc_stack(THREAD_STACK_PAGES).expect("out of memory for a thread stack");
    // Double box: a thin pointer fits into one register for the trampoline
    let arg = Box::into_raw(Box::new(entry)) as u64;
    let rsp = switch::initial_stack(stack.top.as_u64(), arg);
    without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let s = guard.as_mut().expect("scheduler not initialized");
        let id = s.next_id;
        s.next_id += 1;
        s.threads.insert(
            id,
            Box::new(Thread {
                id,
                name: String::from(name),
                state: State::Ready,
                rsp,
                stack: Some(stack),
                cpu_ticks: 0,
                cr3,
                user_entry,
                fpu: Box::new(FxState::initial()),
            }),
        );
        s.ready.push_back(id);
        id
    })
}

/// Starts a new kernel thread running `f`. It is scheduled like all others.
pub fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Tid {
    spawn_internal(name, Box::new(f))
}

/// Starts a thread that runs in the address space `cr3` and immediately
/// drops to ring 3 at `entry`. The caller owns the address space and must
/// keep it alive until the thread has switched away from it for good.
pub fn spawn_user(name: &str, cr3: PhysAddr, entry: UserEntry) -> Tid {
    spawn_with(name, Box::new(|| {}), cr3, Some(entry))
}

/// Entry point of every new thread (called by the assembly trampoline).
extern "C" fn thread_start(arg: *mut Entry) -> ! {
    // We arrive here through `schedule()`, which runs with interrupts disabled
    enable_interrupts();
    let entry = unsafe { Box::from_raw(arg) };
    entry();
    // Only now, with the closure dropped, a user thread leaves for ring 3:
    // nothing on this kernel stack is ever returned to.
    let user_entry = without_interrupts(|| {
        SCHEDULER.lock().as_mut().and_then(|s| {
            let current = s.current;
            s.thread(current).user_entry.take()
        })
    });
    if let Some(e) = user_entry {
        unsafe { syscall::enter_user(e.rip, e.rsp, e.arg0, e.arg1) };
    }
    exit()
}

/// Picks the next thread and switches to it. Interrupts must be disabled.
fn schedule() {
    debug_assert!(!interrupts_enabled());
    let switch = {
        let mut guard = SCHEDULER.lock();
        let s = guard.as_mut().expect("scheduler not initialized");
        s.reap();
        let current = s.current;
        if s.thread(current).state == State::Running {
            if s.ready.is_empty() {
                return; // nobody else wants the CPU
            }
            s.thread(current).state = State::Ready;
            if current != s.idle {
                s.ready.push_back(current);
            }
        }
        let next = s.ready.pop_front().unwrap_or(s.idle);
        s.thread(next).state = State::Running;
        if next == current {
            None
        } else {
            s.current = next;
            let old = s.thread(current);
            let old_rsp: *mut u64 = &mut old.rsp;
            // A dead thread's registers are never needed again
            let old_fpu: Option<*mut FxState> = (old.state != State::Dead).then(|| &mut *old.fpu as *mut FxState);
            let t = s.thread(next);
            let new_fpu: *const FxState = &*t.fpu;
            Some((
                old_rsp,
                t.rsp,
                t.cr3,
                t.stack.map(|st| st.top.as_u64()),
                old_fpu,
                new_fpu,
            ))
        }
    }; // lock released here, interrupts are still off
    if let Some((old_rsp, new_rsp, cr3, kernel_stack, old_fpu, new_fpu)) = switch {
        // The boxed FPU areas stay put: threads are only freed by `reap`,
        // which never frees the current thread, and both are still in the map.
        unsafe {
            if let Some(old) = old_fpu {
                fpu::save(&mut *old);
            }
            fpu::restore(&*new_fpu);
        }
        if let Some(top) = kernel_stack {
            syscall::set_kernel_stack(top);
        }
        // Kernel stacks live in the shared upper half: switching CR3 while
        // still on the old thread's stack is fine.
        if cr3 != paging::read_cr3() {
            unsafe { paging::write_cr3(cr3) };
        }
        unsafe { switch::kernel_switch_context(old_rsp, new_rsp) };
    }
}

/// Called from the timer interrupt (after EOI): wakes sleepers, accounts
/// CPU time and preempts the running thread if others are ready.
pub fn on_tick() {
    if !is_running() {
        return;
    }
    let now = time::ticks();
    {
        let mut guard = SCHEDULER.lock();
        let s = guard.as_mut().unwrap();
        let current = s.current;
        s.thread(current).cpu_ticks += 1;
        s.wake(|w| matches!(w, Wait::Until(t) if t <= now));
    }
    schedule();
}

/// Wakes every thread blocked with exactly `reason`.
pub fn notify(reason: Wait) {
    without_interrupts(|| {
        if let Some(s) = SCHEDULER.lock().as_mut() {
            s.wake(|w| w == reason);
        }
    });
}

/// Called from keyboard/serial interrupt handlers.
pub fn wake_input_waiters() {
    if !is_running() {
        return;
    }
    if let Some(s) = SCHEDULER.lock().as_mut() {
        s.wake(|w| w == Wait::Input);
    }
}

/// Blocks the current thread until it is woken for `reason`.
/// Must be called with interrupts disabled (so the condition the caller
/// checked cannot change before we are on the wait list); returns with
/// interrupts still disabled.
pub fn block_current(reason: Wait) {
    debug_assert!(!interrupts_enabled());
    {
        let mut guard = SCHEDULER.lock();
        let s = guard.as_mut().unwrap();
        let current = s.current;
        s.thread(current).state = State::Blocked(reason);
    }
    schedule();
}

/// Gives the CPU to the next ready thread (if any).
pub fn yield_now() {
    if is_running() {
        without_interrupts(schedule);
    }
}

/// Sleeps until `time::ticks()` reaches `tick`.
pub fn sleep_until(tick: u64) {
    without_interrupts(|| {
        if time::ticks() < tick {
            block_current(Wait::Until(tick));
        }
    });
}

/// Ends the current thread. Its stack is freed later by the scheduler.
pub fn exit() -> ! {
    disable_interrupts();
    {
        let mut guard = SCHEDULER.lock();
        let s = guard.as_mut().unwrap();
        let current = s.current;
        assert!(
            current != 0 && current != s.idle,
            "the main and idle threads cannot exit"
        );
        s.thread(current).state = State::Dead;
        s.wake(|w| w == Wait::Join(current));
    }
    schedule();
    unreachable!("a dead thread was scheduled again");
}

/// Makes thread `id` ready if it is blocked (whatever it waits for). Its
/// blocking loop re-checks its condition; used to interrupt waits on `kill`.
pub fn wake(id: Tid) {
    without_interrupts(|| {
        if let Some(s) = SCHEDULER.lock().as_mut() {
            if s.threads.get(&id).is_some_and(|t| matches!(t.state, State::Blocked(_))) {
                s.make_ready(id);
            }
        }
    });
}

/// Like `join`, but gives up when `abort()` becomes true (checked after
/// every wake-up). Returns true if the thread has exited.
pub fn join_or_abort(id: Tid, abort: impl Fn() -> bool) -> bool {
    without_interrupts(|| {
        loop {
            let alive = SCHEDULER
                .lock()
                .as_mut()
                .is_some_and(|s| s.threads.get(&id).is_some_and(|t| t.state != State::Dead));
            if !alive {
                return true;
            }
            if abort() {
                return false;
            }
            block_current(Wait::Join(id));
        }
    })
}

/// Waits until thread `id` has exited.
pub fn join(id: Tid) {
    without_interrupts(|| {
        loop {
            let alive = SCHEDULER
                .lock()
                .as_mut()
                .is_some_and(|s| s.threads.get(&id).is_some_and(|t| t.state != State::Dead));
            if !alive {
                return;
            }
            block_current(Wait::Join(id));
        }
    });
}

/// Moves the current thread into the address space `cr3` (and activates it).
/// Used by an exiting process to leave its address space before freeing it.
pub fn set_current_cr3(cr3: PhysAddr) {
    without_interrupts(|| {
        if let Some(s) = SCHEDULER.lock().as_mut() {
            let current = s.current;
            s.thread(current).cr3 = cr3;
        }
        if cr3 != paging::read_cr3() {
            unsafe { paging::write_cr3(cr3) };
        }
    });
}

pub fn current_id() -> Tid {
    without_interrupts(|| SCHEDULER.lock().as_ref().map_or(0, |s| s.current))
}

/// All threads that currently exist.
pub fn list() -> Vec<ThreadInfo> {
    without_interrupts(|| {
        SCHEDULER.lock().as_ref().map_or_else(Vec::new, |s| {
            s.threads
                .values()
                .map(|t| ThreadInfo {
                    id: t.id,
                    name: t.name.clone(),
                    state: t.state,
                    cpu_ticks: t.cpu_ticks,
                })
                .collect()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicU64, AtomicUsize};

    #[test_case]
    fn spawn_and_join() {
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let id = spawn("test-flag", move || f.store(true, Ordering::SeqCst));
        join(id);
        assert!(flag.load(Ordering::SeqCst));
        assert!(list().iter().all(|t| t.id != id || t.state == State::Dead));
    }

    #[test_case]
    fn yield_interleaves_threads() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut ids = Vec::new();
        for name in ['a', 'b'] {
            let log = log.clone();
            ids.push(spawn("test-yield", move || {
                for _ in 0..3 {
                    without_interrupts(|| log.lock().push(name));
                    yield_now();
                }
            }));
        }
        for id in ids {
            join(id);
        }
        let log = log.lock();
        assert_eq!(log.len(), 6);
        // round robin: neither thread runs all its iterations in one go
        assert_ne!(&log[..3], &['a', 'a', 'a']);
        assert_ne!(&log[..3], &['b', 'b', 'b']);
    }

    #[test_case]
    fn preemption_without_yield() {
        let counter = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (c, s) = (counter.clone(), stop.clone());
        let id = spawn("test-spin", move || {
            while !s.load(Ordering::Relaxed) {
                c.fetch_add(1, Ordering::Relaxed); // never yields
            }
        });
        // busy-wait (no yield, no sleep) for ~100 ms: only preemption can run the spinner
        let end = time::ticks() + 10;
        while time::ticks() < end {
            core::hint::spin_loop();
        }
        assert!(counter.load(Ordering::Relaxed) > 0, "spinner never ran: no preemption");
        stop.store(true, Ordering::Relaxed);
        join(id);
    }

    #[test_case]
    fn sleeping_thread_wakes_up_on_time() {
        let woke_at = Arc::new(AtomicU64::new(0));
        let w = woke_at.clone();
        let start = time::ticks();
        let id = spawn("test-sleep", move || {
            time::sleep_ms(50);
            w.store(time::ticks(), Ordering::SeqCst);
        });
        join(id);
        let elapsed = woke_at.load(Ordering::SeqCst) - start;
        assert!((5..=8).contains(&elapsed), "slept {} ticks instead of ~5", elapsed);
    }

    #[test_case]
    fn exited_threads_release_memory() {
        // warm up: page tables for the stack area, heap growth
        for _ in 0..3 {
            let id = spawn("warmup", || {});
            join(id);
        }
        yield_now(); // let the scheduler reap
        let before = crate::mem::pmm::stats().free_frames;
        for _ in 0..20 {
            let id = spawn("test-reap", || {});
            join(id);
        }
        yield_now();
        let after = crate::mem::pmm::stats().free_frames;
        // allow a few frames for new page tables in the never-reused stack area
        assert!(before.saturating_sub(after) <= 4, "leaked {} frames", before - after);
    }

    #[test_case]
    fn many_threads() {
        let sum = Arc::new(AtomicUsize::new(0));
        let ids: Vec<Tid> = (1..=50)
            .map(|i| {
                let s = sum.clone();
                spawn("test-many", move || {
                    time::sleep_ms((i % 5) as u64 * 10);
                    s.fetch_add(i, Ordering::SeqCst);
                })
            })
            .collect();
        for id in ids {
            join(id);
        }
        assert_eq!(sum.load(Ordering::SeqCst), 50 * 51 / 2);
    }

    #[test_case]
    fn main_and_idle_exist() {
        let threads = list();
        let names: Vec<&str> = threads.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"main") && names.contains(&"idle"));
        assert_eq!(current_id(), 0);
    }
}
