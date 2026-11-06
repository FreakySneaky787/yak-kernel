//! Threads and a scheduler - since chapter 15 for all CPUs at once.
//!
//! A thread is a stack of its own plus the CPU's registers. To switch, `switch_context` pushes the registers of the
//! running thread onto its stack, stores the stack pointer, loads the stack pointer of the next thread and pops its
//! registers - and the next `ret` continues wherever that thread stopped.
//!
//! The threads that may run wait in one queue, *round robin*, and every CPU takes the next one from it. Each CPU has
//! a timer (100 Hz) that moves its running thread to the end of the queue if another one is ready - *preemption* -
//! and an *idle* thread of its own that halts it when the queue is empty. A thread that waits for something is
//! *blocked* and not in the queue at all; whoever makes the thing happen wakes it, and sends an idle CPU a wake-up
//! interrupt so that it looks at the queue again.
//!
//! With several CPUs "interrupts off" no longer means "nobody else runs". Three rules follow:
//! - The scheduler's state is behind a real lock, and the lock is held *through* the switch: the next thread
//!   releases it (`finish_switch`). Released earlier, another CPU could take the old thread from the queue and run
//!   it on its stack while this CPU still pushes registers onto it.
//! - A thread on a CPU is marked (`on_cpu`); only the CPU it runs on lets go of it, after the switch.
//! - Waiting is announced before the check (`wait_for`): a wake-up that comes from another CPU in between finds the
//!   thread blocked and cancels the block, instead of being lost.

use crate::interrupts::{WAKE_VECTOR, without_interrupts};
use crate::smp::{self, MAX_CPUS};
use crate::stack::{self, Stack};
use crate::{lapic, time};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
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
    /// For an event with this number - whoever causes it calls `wake(Wait::Event(n))`. Only the tests use it so
    /// far; the disk driver will (chapter 19).
    #[allow(dead_code)]
    Event(u64),
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
    /// The CPU the thread is on right now - from the switch to it until the switch away from it is complete.
    on_cpu: Option<usize>,
    /// The stack pointer while the thread does not run - its registers lie on the stack just above.
    rsp: u64,
    /// `None` for the threads that run on a stack they were started with: main and the idle threads of the APs.
    stack: Option<Stack>,
}

struct Scheduler {
    /// In a `Box` each: `switch_context` gets a pointer to `rsp`, which must not move when the map changes.
    threads: BTreeMap<Id, Box<Thread>>,
    ready: VecDeque<Id>,
    /// The thread each CPU runs, and its idle thread (`None`: that CPU has not joined yet).
    current: [Option<Id>; MAX_CPUS],
    idle: [Option<Id>; MAX_CPUS],
    next_id: Id,
}

static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);
/// The scheduler, for `finish_switch`: it runs while the lock is held by the CPU's previous thread, whose guard
/// is gone (`schedule` forgot it) - so the lock is ours, but there is no guard to reach the data through.
static SCHEDULER_DATA: AtomicPtr<Scheduler> = AtomicPtr::new(core::ptr::null_mut());
/// The thread each CPU switched away from - `finish_switch` lets go of it.
static PREVIOUS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

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

/// The first Rust code of every new thread: finish the switch that brought us here (it holds the lock), interrupts
/// on, run, end.
extern "C" fn thread_start(entry: *mut Entry) -> ! {
    finish_switch();
    let entry = unsafe { Box::from_raw(entry) };
    unsafe { asm!("sti", options(nomem, nostack)) };
    entry();
    exit()
}

impl Scheduler {
    fn thread(&mut self, id: Id) -> &mut Thread {
        self.threads.get_mut(&id).expect("a thread with this id")
    }

    /// Blocked -> ready. A thread that is still on a CPU (it announced its wait, but has not switched away yet)
    /// just keeps running: its `wait_for` sees the state and checks again.
    fn wake_one(&mut self, id: Id) {
        let thread = self.thread(id);
        if !matches!(thread.state, State::Blocked(_)) {
            return;
        }
        if thread.on_cpu.is_some() {
            thread.state = State::Running;
            return;
        }
        thread.state = State::Ready;
        self.ready.push_back(id);
        self.kick_idle_cpu();
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

    /// A wake-up interrupt to one other CPU that runs its idle thread: there is work in the queue now.
    fn kick_idle_cpu(&self) {
        let me = smp::cpu();
        let idle = |cpu: usize| self.idle[cpu].is_some() && self.current[cpu] == self.idle[cpu];
        if let Some(cpu) = (0..MAX_CPUS).find(|&cpu| cpu != me && idle(cpu)) {
            lapic::send_interrupt(smp::apic_id(cpu), WAKE_VECTOR);
        }
    }
}

/// Gives this CPU to the next thread, if it should get another one. The caller has set the state of the running
/// thread: `Running` (it may go on - unless others are waiting), `Ready` (it wants to go to the back of the queue),
/// `Blocked` or `Dead`. Interrupts must be off. Returns when this thread runs again.
fn schedule() {
    let cpu = smp::cpu();
    let mut guard = SCHEDULER.lock();
    let s = guard.as_mut().expect("task::init ran");
    let (Some(current), Some(idle)) = (s.current[cpu], s.idle[cpu]) else {
        return; // a CPU that has not joined the scheduler
    };
    let nobody_waits = s.ready.is_empty();
    match s.thread(current).state {
        State::Running if nobody_waits => return, // nobody else wants the CPU
        State::Running | State::Ready => {
            s.thread(current).state = State::Ready;
            if current != idle {
                s.ready.push_back(current); // the idle thread is never queued: it runs when the queue is empty
            }
        }
        State::Blocked(_) | State::Dead => {}
    }
    let next = s.ready.pop_front().unwrap_or(idle);
    let thread = s.thread(next);
    thread.state = State::Running;
    if next == current {
        return;
    }
    thread.on_cpu = Some(cpu);
    let new_rsp = thread.rsp;
    s.current[cpu] = Some(next);
    let old_rsp: *mut u64 = &mut s.thread(current).rsp;
    PREVIOUS[cpu].store(current, Ordering::Relaxed);
    // The lock stays held: until `switch_context` has stored the old registers, no other CPU may take the old
    // thread. The next thread releases it in `finish_switch` - right after this call returns in the thread that
    // comes back here, or in `thread_start` in a new one.
    core::mem::forget(guard);
    unsafe { switch_context(old_rsp, new_rsp) };
    finish_switch();
}

/// The second half of every switch, run by the thread that was switched to: the previous thread is off this CPU now
/// - another CPU may take it - and the scheduler lock is released.
fn finish_switch() {
    let cpu = smp::cpu();
    // Safety: the lock is held (by `schedule` on this CPU, which forgot its guard), so nobody else touches the data.
    let s = unsafe { &mut *SCHEDULER_DATA.load(Ordering::Relaxed) };
    let previous = PREVIOUS[cpu].load(Ordering::Relaxed);
    if let Some(thread) = s.threads.get_mut(&previous) {
        thread.on_cpu = None;
    }
    unsafe { SCHEDULER.force_unlock() };
}

/// Turns the code that runs now (`kmain`, on the BSP) into thread 0, "main", and starts the BSP's idle thread.
pub fn init() {
    let main = Box::new(Thread {
        name: "main",
        state: State::Running,
        on_cpu: Some(0),
        rsp: 0, // filled in by its first switch
        stack: None,
    });
    let idle = new_thread("idle", idle_loop);
    let threads = BTreeMap::from([(0, main), (1, idle)]);
    // All at once, with interrupts off: a tick that found the scheduler half built would act on it (bug #28). The
    // idle thread is never in the queue - it runs when the queue is empty.
    without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let mut current = [None; MAX_CPUS];
        let mut idle = [None; MAX_CPUS];
        current[0] = Some(0);
        idle[0] = Some(1);
        *guard = Some(Scheduler {
            threads,
            ready: VecDeque::new(),
            current,
            idle,
            next_id: 2,
        });
        SCHEDULER_DATA.store(guard.as_mut().unwrap(), Ordering::Relaxed);
    });
}

/// Called by every other CPU once it runs (`smp::ap_main`): the code that runs now becomes this CPU's idle thread,
/// and the CPU takes part in scheduling. Never returns.
pub fn join_cpu(cpu: usize) -> ! {
    without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let s = guard.as_mut().expect("task::init ran before the other CPUs start");
        let id = s.next_id;
        s.next_id += 1;
        let thread = Box::new(Thread {
            name: "idle",
            state: State::Running,
            on_cpu: Some(cpu),
            rsp: 0,
            stack: None, // the stack `smp::start` gave this CPU
        });
        s.threads.insert(id, thread);
        s.current[cpu] = Some(id);
        s.idle[cpu] = Some(id);
    });
    idle_loop();
    unreachable!("the idle loop never ends");
}

/// What every idle thread does: give the CPU to whoever is ready, or halt until an interrupt - the timer's at the
/// latest, or a wake-up from a CPU that queued a thread.
fn idle_loop() {
    loop {
        unsafe { asm!("cli", options(nomem, nostack)) };
        yield_now();
        // `sti` takes effect after `hlt` begins: a wake-up right after the check still ends the `hlt`
        unsafe { asm!("sti", "hlt", options(nomem, nostack)) };
    }
}

/// Whether `init` ran.
pub fn running() -> bool {
    without_interrupts(|| SCHEDULER.lock().is_some())
}

/// A thread that will run `entry`, with a stack of its own - not known to the scheduler yet.
fn new_thread(name: &'static str, entry: impl FnOnce() + Send + 'static) -> Box<Thread> {
    let stack = stack::alloc(STACK_PAGES).expect("frames for a thread stack");
    // a `Box<dyn FnOnce>` is two words wide; boxed once more, one pointer fits into r12
    let entry: Box<Entry> = Box::new(Box::new(entry));
    let entry = Box::into_raw(entry);
    Box::new(Thread {
        name,
        state: State::Ready,
        on_cpu: None,
        rsp: first_stack(stack.top, entry as u64),
        stack: Some(stack),
    })
}

/// Starts `entry` as a new thread; it runs when its turn comes - on whichever CPU takes it first.
pub fn spawn(name: &'static str, entry: impl FnOnce() + Send + 'static) -> Id {
    reap();
    let thread = new_thread(name, entry);
    without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let scheduler = guard.as_mut().expect("task::init ran");
        let id = scheduler.next_id;
        scheduler.next_id += 1;
        scheduler.threads.insert(id, thread);
        scheduler.ready.push_back(id);
        scheduler.kick_idle_cpu();
        id
    })
}

/// The id of the running thread.
pub fn current() -> Id {
    without_interrupts(|| {
        let guard = SCHEDULER.lock();
        guard.as_ref().expect("task::init ran").current[smp::cpu()].expect("this CPU runs threads")
    })
}

/// Sets the state of the thread this CPU runs.
fn set_state(state: State) {
    let cpu = smp::cpu();
    let mut guard = SCHEDULER.lock();
    let s = guard.as_mut().expect("task::init ran");
    let current = s.current[cpu].expect("this CPU runs threads");
    s.thread(current).state = state;
}

/// Lets the next ready thread run; this one goes to the end of the queue.
pub fn yield_now() {
    without_interrupts(|| {
        set_state(State::Ready);
        schedule();
    });
}

/// Waits until `ready` gives a value, blocked as `wait` in between: the CPU runs other threads meanwhile.
///
/// The wait is announced *before* `ready` is asked: a wake-up that comes from another CPU between the question and
/// the switch finds the thread blocked, sets it running again, and `schedule` keeps it on the CPU - so the next round
/// asks again instead of sleeping through it. Before `init` there are no threads: then it halts instead.
pub fn wait_for<T>(wait: Wait, mut ready: impl FnMut() -> Option<T>) -> T {
    if !running() {
        loop {
            let value = without_interrupts(|| {
                let value = ready();
                if value.is_none() {
                    // `sti` takes effect after the next instruction: an interrupt cannot slip in before `hlt`
                    unsafe { asm!("sti", "hlt", "cli", options(nomem, nostack)) };
                }
                value
            });
            if let Some(value) = value {
                return value;
            }
        }
    }
    without_interrupts(|| {
        loop {
            set_state(State::Blocked(wait));
            if let Some(value) = ready() {
                set_state(State::Running);
                return value;
            }
            schedule(); // returns when a wake-up made the thread run again - or found it still here
        }
    })
}

/// Wakes the threads that wait for `wait`. For interrupt handlers and threads alike, on any CPU.
pub fn wake(wait: Wait) {
    without_interrupts(|| {
        if let Some(scheduler) = SCHEDULER.lock().as_mut() {
            scheduler.wake(|w| w == wait);
        }
    });
}

/// Ends the running thread. Its stack is still in use until the switch, so `reap` frees it later.
pub fn exit() -> ! {
    unsafe { asm!("cli", options(nomem, nostack)) };
    {
        let cpu = smp::cpu();
        let mut guard = SCHEDULER.lock();
        let s = guard.as_mut().expect("task::init ran");
        let id = s.current[cpu].expect("this CPU runs threads");
        s.thread(id).state = State::Dead;
        s.wake(|wait| wait == Wait::Join(id));
    }
    schedule();
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

/// Frees the stacks of ended threads that no CPU is on any more. Not in the timer interrupt: freeing takes the page
/// table lock, which the interrupted thread may hold - so only threads call it (`spawn`, `join`).
fn reap() {
    let dead: Vec<Box<Thread>> = without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let Some(scheduler) = guard.as_mut() else {
            return Vec::new();
        };
        let ids: Vec<Id> = scheduler
            .threads
            .iter()
            .filter(|&(_, t)| t.state == State::Dead && t.on_cpu.is_none())
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

/// Called by the timer interrupt of every CPU, after its end of interrupt: wakes the sleepers whose time has come,
/// and gives this CPU to the next ready thread.
pub fn on_tick() {
    let now = time::ticks();
    {
        let mut guard = SCHEDULER.lock();
        let Some(scheduler) = guard.as_mut() else {
            return; // before `init`
        };
        if scheduler.current[smp::cpu()].is_none() {
            return; // a CPU whose timer runs before it joined
        }
        scheduler.wake(|wait| matches!(wait, Wait::Until(tick) if tick <= now));
    }
    // in an interrupt handler: interrupts are off already, and `iretq` turns them on again later
    schedule();
}

/// Every thread: id, name, state, the CPU it is on - for the shell's `threads`.
pub fn list() -> Vec<(Id, &'static str, State, Option<usize>)> {
    without_interrupts(|| {
        let guard = SCHEDULER.lock();
        let scheduler = guard.as_ref().expect("task::init ran");
        scheduler.threads.iter().map(|(&id, t)| (id, t.name, t.state, t.on_cpu)).collect()
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
        // let it start and block - on this CPU or another one, which may take a moment
        for _ in 0..1000 {
            if matches!(state(sleeper), Some(State::Blocked(_))) {
                break;
            }
            yield_now();
        }
        match state(sleeper) {
            Some(State::Blocked(Wait::Until(tick))) => assert!(tick > time::ticks()),
            other => panic!("the sleeper is {:?}", other),
        }
        join(sleeper);
    }

    #[test_case]
    fn ended_threads_give_their_stack_back() {
        let round = || -> usize {
            // the threads wait until all 20 exist: the other CPUs would run them at once, and the next `spawn`
            // would already free their stacks
            let go = Arc::new(AtomicBool::new(false));
            let ids: Vec<Id> = (0..20)
                .map(|_| {
                    let go = go.clone();
                    spawn("short", move || wait_for(Wait::Event(7), || go.load(Ordering::SeqCst).then_some(())))
                })
                .collect();
            let during = crate::pmm::free_frames();
            go.store(true, Ordering::SeqCst);
            wake(Wait::Event(7));
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
    fn every_cpu_has_an_idle_thread_that_is_never_queued() {
        without_interrupts(|| {
            let guard = SCHEDULER.lock();
            let scheduler = guard.as_ref().unwrap();
            assert_eq!(scheduler.idle[0], Some(1), "the BSP's idle thread is thread 1");
            for cpu in 0..smp::online() {
                let idle = scheduler.idle[cpu].expect("every running CPU joined");
                assert_eq!(scheduler.threads[&idle].name, "idle");
                assert!(!scheduler.ready.contains(&idle));
            }
        });
    }

    #[test_case]
    fn every_cpu_has_its_own_timer() {
        let before: Vec<u64> = (0..smp::online())
            .map(|cpu| crate::interrupts::TIMER_INTERRUPTS[cpu].load(Ordering::Relaxed))
            .collect();
        time::sleep_ms(50);
        for (cpu, before) in before.into_iter().enumerate() {
            let after = crate::interrupts::TIMER_INTERRUPTS[cpu].load(Ordering::Relaxed);
            assert!(after > before, "no tick on CPU {} in 50 ms", cpu);
        }
    }

    #[test_case]
    fn threads_run_on_every_cpu() {
        // as many spinners as CPUs, each noting where it ran: the queue is shared, so idle CPUs take them
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let ids: Vec<Id> = (0..smp::online())
            .map(|_| {
                let (seen, stop) = (seen.clone(), stop.clone());
                spawn("spinner", move || {
                    while !stop.load(Ordering::Relaxed) {
                        let cpu = without_interrupts(smp::cpu);
                        without_interrupts(|| {
                            let mut seen = seen.lock();
                            if !seen.contains(&cpu) {
                                seen.push(cpu);
                            }
                        });
                    }
                })
            })
            .collect();
        time::sleep_ms(100);
        stop.store(true, Ordering::Relaxed);
        ids.into_iter().for_each(join);
        let mut seen = seen.lock().clone();
        seen.sort();
        assert_eq!(seen, (0..smp::online()).collect::<Vec<_>>(), "a CPU never ran a spinner");
    }

    #[test_case]
    fn no_wake_up_is_lost_between_cpus() {
        // Ping-pong: two threads take turns through a counter, each waiting for its turn and waking the other.
        // Every wake-up comes from the other thread - mostly on another CPU, often in the moment between the
        // waiter's check and its switch. One lost wake-up and both wait for ever: the test kernel's timeout.
        const ROUNDS: u64 = 2000;
        let turn = Arc::new(AtomicU64::new(0));
        let ids: Vec<Id> = (0..2)
            .map(|me| {
                let turn = turn.clone();
                spawn("ping-pong", move || {
                    for round in 0..ROUNDS {
                        let mine = 2 * round + me;
                        wait_for(Wait::Event(me), || (turn.load(Ordering::SeqCst) == mine).then_some(()));
                        turn.store(mine + 1, Ordering::SeqCst);
                        wake(Wait::Event(1 - me));
                    }
                })
            })
            .collect();
        ids.into_iter().for_each(join);
        assert_eq!(turn.load(Ordering::SeqCst), 2 * ROUNDS);
    }

    #[test_case]
    fn the_scheduler_lock_is_free_after_a_switch() {
        // held through every switch and released by the next thread: if a path forgot that, the next lock would hang
        for _ in 0..100 {
            yield_now();
        }
        join(spawn("short", || {}));
        assert!(running());
    }

    #[test_case]
    fn only_main_and_the_idle_threads_are_left() {
        let names: Vec<&str> = list().into_iter().map(|t| t.1).collect();
        assert_eq!(names[0], "main");
        assert_eq!(names.len(), 1 + smp::online(), "{:?}", names);
        assert!(names[1..].iter().all(|&n| n == "idle"));
    }
}
