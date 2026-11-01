//! Threads and a scheduler: several things at once on one CPU.
//!
//! A thread is a stack of its own plus the CPU's registers. To switch, `switch_context` pushes the registers of the
//! running thread onto its stack, stores the stack pointer, loads the stack pointer of the next thread and pops its
//! registers - and the next `ret` continues wherever that thread stopped.
//!
//! The threads that may run wait in a queue, *round robin*. The timer interrupt (100 Hz) moves the running thread to
//! the end of the queue if another one is ready: *preemption*, so a thread that never gives up the CPU cannot keep
//! it. A thread that waits for something (a time, a key, another thread) is *blocked* and not in the queue at all;
//! whoever makes the thing happen wakes it. If no thread is ready, the *idle* thread halts the CPU.
//!
//! All scheduler state is touched with interrupts off: the timer handler uses it too, and on one CPU "interrupts
//! off" means "nobody else runs".

use crate::interrupts::without_interrupts;
use crate::stack::{self, Stack};
use crate::time;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use spin::Mutex;

pub type Id = u64;

/// Pages of stack per thread: 32 KiB, with a guard page below (chapter 9).
const STACK_PAGES: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Until `time::ticks()` reaches this tick.
    Until(u64),
    /// For a key (the keyboard handler wakes it).
    Input,
    /// For thread `Id` to end.
    Join(Id),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    Running,
    Blocked(Wait),
    Dead,
}

/// What a new thread runs.
type Entry = Box<dyn FnOnce() + Send + 'static>;

struct Thread {
    name: &'static str,
    state: State,
    /// The stack pointer while the thread does not run - its registers lie on the stack just above.
    rsp: u64,
    /// `None` for thread 0, which runs on the stack Limine gave `kmain`.
    stack: Option<Stack>,
}

struct Scheduler {
    /// In a `Box` each: `switch_context` gets a pointer to `rsp`, which must not move when the map changes.
    threads: BTreeMap<Id, Box<Thread>>,
    ready: VecDeque<Id>,
    current: Id,
    idle: Id,
    next_id: Id,
}

static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);

// The context switch. rdi: where to store the old stack pointer; rsi: the new one. Only the registers a function
// must keep for its caller (System V ABI: rbx, rbp, r12-r15) are saved - for the compiler this is a normal function
// call, so it already assumes all others are changed. The kernel uses no SSE, so there is no vector state either.
global_asm!(
    ".global switch_context",
    "switch_context:",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
    // where a new thread's first `ret` lands: its entry is in r12 (see `first_stack`)
    ".global thread_trampoline",
    "thread_trampoline:",
    "mov rdi, r12",
    "call {start}",
    "ud2", // `thread_start` never returns
    start = sym thread_start,
);

unsafe extern "C" {
    fn switch_context(old_rsp: *mut u64, new_rsp: u64);
    fn thread_trampoline();
}

/// A new stack, filled as if `switch_context` had pushed onto it: six registers (r12 = `entry`), then the return
/// address - the trampoline. Returns the stack pointer to give `switch_context`.
fn first_stack(top: u64, entry: u64) -> u64 {
    // lowest address first: r15, r14, r13, r12, rbx, rbp, return address
    let trampoline = thread_trampoline as unsafe extern "C" fn() as usize as u64;
    let frame = [0, 0, 0, entry, 0, 0, trampoline];
    let rsp = top - 8 * frame.len() as u64;
    for (i, value) in frame.into_iter().enumerate() {
        unsafe { ((rsp + 8 * i as u64) as *mut u64).write(value) };
    }
    rsp
}

/// The first Rust code of every new thread: interrupts on (the switch here happened with them off), run, end.
extern "C" fn thread_start(entry: *mut Entry) -> ! {
    let entry = unsafe { Box::from_raw(entry) };
    unsafe { asm!("sti", options(nomem, nostack)) };
    entry();
    exit()
}

impl Scheduler {
    fn thread(&mut self, id: Id) -> &mut Thread {
        self.threads.get_mut(&id).expect("a thread with this id")
    }

    /// Blocked -> ready, at the end of the queue.
    fn wake_one(&mut self, id: Id) {
        let thread = self.thread(id);
        if let State::Blocked(_) = thread.state {
            thread.state = State::Ready;
            self.ready.push_back(id);
        }
    }

    /// Wakes every thread that waits for something `matches` accepts.
    fn wake(&mut self, matches: impl Fn(Wait) -> bool) {
        let woken: Vec<Id> = self
            .threads
            .iter()
            .filter(|(_, t)| matches!(t.state, State::Blocked(wait) if matches(wait)))
            .map(|(&id, _)| id)
            .collect();
        for id in woken {
            self.wake_one(id);
        }
    }

    /// Picks the next thread - the first in the queue, or the idle thread - and makes it current. The caller has
    /// already put the current one where it belongs (queue, blocked, dead). `None` if it stays the same.
    fn switch_to_next(&mut self) -> Option<(*mut u64, u64)> {
        let next = self.ready.pop_front().unwrap_or(self.idle);
        if next == self.current {
            self.thread(next).state = State::Running;
            return None;
        }
        let old = core::mem::replace(&mut self.current, next);
        let new_rsp = {
            let thread = self.thread(next);
            thread.state = State::Running;
            thread.rsp
        };
        Some((&mut self.thread(old).rsp as *mut u64, new_rsp))
    }
}

/// Runs `change` on the scheduler with interrupts off, and switches to the next thread if it says so. Returns when
/// this thread runs again.
fn schedule(change: impl FnOnce(&mut Scheduler)) {
    without_interrupts(|| {
        let switch = {
            let mut guard = SCHEDULER.lock();
            let scheduler = guard.as_mut().expect("task::init ran");
            change(scheduler);
            scheduler.switch_to_next()
        }; // the lock is free again before we leave: the next thread needs it
        if let Some((old_rsp, new_rsp)) = switch {
            unsafe { switch_context(old_rsp, new_rsp) };
        }
    });
}

/// Turns the code that runs now (`kmain`) into thread 0, "main", and starts the idle thread.
pub fn init() {
    let mut threads = BTreeMap::new();
    let main = Thread {
        name: "main",
        state: State::Running,
        rsp: 0, // filled in by its first switch
        stack: None,
    };
    threads.insert(0, Box::new(main));
    *SCHEDULER.lock() = Some(Scheduler {
        threads,
        ready: VecDeque::new(),
        current: 0,
        idle: 0,
        next_id: 1,
    });
    let idle = spawn("idle", || {
        loop {
            // nothing else ready: sleep until an interrupt - the timer's at the latest - and look again
            yield_now();
            unsafe { asm!("hlt", options(nomem, nostack)) };
        }
    });
    without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let scheduler = guard.as_mut().unwrap();
        scheduler.ready.retain(|&id| id != idle); // never in the queue: it runs when the queue is empty
        scheduler.idle = idle;
    });
}

/// Whether `init` ran.
pub fn running() -> bool {
    without_interrupts(|| SCHEDULER.lock().is_some())
}

/// Starts `entry` as a new thread; it runs when its turn comes.
pub fn spawn(name: &'static str, entry: impl FnOnce() + Send + 'static) -> Id {
    reap();
    let stack = stack::alloc(STACK_PAGES).expect("frames for a thread stack");
    // a `Box<dyn FnOnce>` is two words wide; boxed once more, one pointer fits into r12
    let entry: Box<Entry> = Box::new(Box::new(entry));
    let entry = Box::into_raw(entry);
    let thread = Box::new(Thread {
        name,
        state: State::Ready,
        rsp: first_stack(stack.top, entry as u64),
        stack: Some(stack),
    });
    without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let scheduler = guard.as_mut().expect("task::init ran");
        let id = scheduler.next_id;
        scheduler.next_id += 1;
        scheduler.threads.insert(id, thread);
        scheduler.ready.push_back(id);
        id
    })
}

/// The id of the running thread.
pub fn current() -> Id {
    without_interrupts(|| SCHEDULER.lock().as_ref().expect("task::init ran").current)
}

/// Lets the next ready thread run; this one goes to the end of the queue.
pub fn yield_now() {
    schedule(|s| {
        s.thread(s.current).state = State::Ready;
        if s.current != s.idle {
            s.ready.push_back(s.current); // the idle thread is never queued: it runs when the queue is empty
        }
    });
}

/// Waits until `ready` gives a value, blocked as `wait` in between: the CPU runs other threads meanwhile.
///
/// `ready` is checked with interrupts off, and the thread blocks before they come on again - so the wake-up cannot
/// come between "not yet" and "block", and be lost. Before `init` there are no threads: then it halts instead.
pub fn wait_for<T>(wait: Wait, mut ready: impl FnMut() -> Option<T>) -> T {
    loop {
        let value = without_interrupts(|| {
            let value = ready();
            if value.is_none() {
                if running() {
                    schedule(|s| s.thread(s.current).state = State::Blocked(wait));
                } else {
                    // `sti` takes effect after the next instruction: an interrupt cannot slip in before `hlt`
                    unsafe { asm!("sti", "hlt", "cli", options(nomem, nostack)) };
                }
            }
            value
        });
        if let Some(value) = value {
            return value;
        }
    }
}

/// Wakes the threads that wait for `wait`. For interrupt handlers and threads alike.
pub fn wake(wait: Wait) {
    without_interrupts(|| {
        if let Some(scheduler) = SCHEDULER.lock().as_mut() {
            scheduler.wake(|w| w == wait);
        }
    });
}

/// Ends the running thread. Its stack is still in use until the switch, so `reap` frees it later.
pub fn exit() -> ! {
    schedule(|s| {
        let id = s.current;
        s.thread(id).state = State::Dead;
        s.wake(|wait| wait == Wait::Join(id));
    });
    unreachable!("a dead thread was switched back to");
}

/// Waits until thread `id` has ended.
pub fn join(id: Id) {
    wait_for(Wait::Join(id), || {
        let guard = SCHEDULER.lock();
        let state = guard.as_ref()?.threads.get(&id).map(|t| t.state);
        matches!(state, None | Some(State::Dead)).then_some(())
    });
    reap();
}

/// Frees the stacks of ended threads. Not in the timer interrupt: freeing takes the page table lock, which the
/// interrupted thread may hold - so only threads call it (`spawn`, `join`).
fn reap() {
    let dead: Vec<Box<Thread>> = without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let Some(scheduler) = guard.as_mut() else {
            return Vec::new();
        };
        let current = scheduler.current;
        let ids: Vec<Id> = scheduler
            .threads
            .iter()
            .filter(|&(&id, t)| t.state == State::Dead && id != current)
            .map(|(&id, _)| id)
            .collect();
        ids.iter().filter_map(|id| scheduler.threads.remove(id)).collect()
    });
    for thread in dead {
        if let Some(stack) = thread.stack {
            stack::free(stack);
        }
    }
}

/// Called by the timer interrupt, after its end of interrupt: wakes the sleepers whose time has come, and gives the
/// CPU to the next ready thread.
pub fn on_tick() {
    let now = time::ticks();
    let mut guard = SCHEDULER.lock();
    let Some(scheduler) = guard.as_mut() else {
        return; // before `init`
    };
    scheduler.wake(|wait| matches!(wait, Wait::Until(tick) if tick <= now));
    if scheduler.ready.is_empty() {
        return;
    }
    drop(guard);
    yield_now(); // in an interrupt handler: interrupts are off already, and `iretq` turns them on again later
}

/// Every thread: id, name, state - for the shell's `threads`.
pub fn list() -> Vec<(Id, &'static str, State)> {
    without_interrupts(|| {
        let guard = SCHEDULER.lock();
        let scheduler = guard.as_ref().expect("task::init ran");
        scheduler.threads.iter().map(|(&id, t)| (id, t.name, t.state)).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    fn state(id: Id) -> Option<State> {
        list().into_iter().find(|t| t.0 == id).map(|t| t.2)
    }

    #[test_case]
    fn a_new_stack_returns_into_the_trampoline() {
        let mut stack = [0u64; 16];
        let top = stack.as_mut_ptr() as u64 + 8 * 16;
        let rsp = first_stack(top, 0x1234);
        assert_eq!(rsp, top - 56);
        assert_eq!(stack[16 - 4], 0x1234); // r12
        let trampoline = thread_trampoline as unsafe extern "C" fn() as usize as u64;
        assert_eq!(stack[16 - 1], trampoline); // what `ret` jumps to
    }

    #[test_case]
    fn a_thread_runs_and_is_joined() {
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        // interrupts off, so that no tick switches to it before we look
        let (id, first) = without_interrupts(|| {
            let id = spawn("test", move || flag.store(true, Ordering::Relaxed));
            (id, state(id))
        });
        assert_eq!(first, Some(State::Ready)); // spawned, but not run yet: main still has the CPU
        join(id);
        assert!(done.load(Ordering::Relaxed));
        assert_eq!(state(id), None, "join reaps the thread");
    }

    #[test_case]
    fn yield_takes_turns() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let ids: Vec<Id> = ['a', 'b']
            .into_iter()
            .map(|name| {
                let log = log.clone();
                spawn("turns", move || {
                    for _ in 0..3 {
                        without_interrupts(|| log.lock().push(name));
                        yield_now();
                    }
                })
            })
            .collect();
        ids.into_iter().for_each(join);
        let log = log.lock();
        assert_eq!(log.iter().filter(|&&c| c == 'a').count(), 3);
        assert_eq!(log.iter().filter(|&&c| c == 'b').count(), 3);
    }

    #[test_case]
    fn a_thread_that_never_yields_is_preempted() {
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicU64::new(0));
        let (s, c) = (stop.clone(), count.clone());
        let spinner = spawn("spinner", move || {
            while !s.load(Ordering::Relaxed) {
                c.fetch_add(1, Ordering::Relaxed); // no yield, no sleep: only the timer can take the CPU away
            }
        });
        time::sleep_ms(50);
        // main is back although the spinner never gave up the CPU - and the spinner ran while main slept
        assert!(count.load(Ordering::Relaxed) > 0);
        stop.store(true, Ordering::Relaxed);
        join(spinner);
    }

    #[test_case]
    fn a_sleeping_thread_is_blocked_until_its_tick() {
        let sleeper = spawn("sleeper", || time::sleep_ms(100));
        yield_now(); // let it start and block
        match state(sleeper) {
            Some(State::Blocked(Wait::Until(tick))) => assert!(tick > time::ticks()),
            other => panic!("the sleeper is {:?}", other),
        }
        join(sleeper);
    }

    #[test_case]
    fn ended_threads_give_their_stack_back() {
        let round = || -> usize {
            // interrupts off while spawning: otherwise a tick could run the first threads, and the next `spawn`
            // would already free their stacks
            let (ids, during) = without_interrupts(|| {
                let ids: Vec<Id> = (0..20).map(|_| spawn("short", || {})).collect();
                (ids, crate::pmm::free_frames())
            });
            ids.into_iter().for_each(join);
            during
        };
        round(); // the first round may grow the heap; the second needs nothing new
        let free = crate::pmm::free_frames();
        let during = round();
        assert!(during <= free - 20 * STACK_PAGES as usize, "20 stacks of 8 pages");
        // all 160 stack frames are back; at most a new page table for the stack area stays
        assert!(
            crate::pmm::free_frames() + 1 >= free,
            "{} -> {}",
            free,
            crate::pmm::free_frames()
        );
    }

    #[test_case]
    fn the_timer_can_allocate_while_a_thread_holds_the_heap() {
        // A thread that is inside the heap almost all the time - and main, whose every wake-up makes the timer
        // handler allocate (`wake` collects the woken threads in a `Vec`). If the heap lock were taken with
        // interrupts on, a tick would sooner or later find it held, and wait for it for ever.
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let allocator = spawn("allocator", move || {
            while !s.load(Ordering::Relaxed) {
                core::hint::black_box(alloc::vec![0u8; 64]);
            }
        });
        for _ in 0..20 {
            time::sleep_ms(10);
        }
        stop.store(true, Ordering::Relaxed);
        join(allocator);
    }

    #[test_case]
    fn only_main_and_idle_are_left() {
        let names: Vec<&str> = list().into_iter().map(|t| t.1).collect();
        assert_eq!(names, ["main", "idle"]);
    }
}
