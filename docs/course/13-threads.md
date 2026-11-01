# 13 – Threads and a scheduler

**Goal:** several things at once on one CPU – threads that the timer switches between, and that wait without
burning the CPU.
**You end up with:** kernel threads with guarded stacks, a context switch of 15 instructions, a
round-robin scheduler that the local APIC timer of chapter 12 drives, blocking waits (sleep, keyboard, `join`)
without lost wake-ups, an idle thread, and a shell command `threads`.
**Code:** [`course/13-threads/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/13-threads)
(chapter 12 plus `task.rs`, `stack::free`, a heap that locks with interrupts off, and `sleep_ms`/`read_key` that
block instead of halting)
**What changed:** `diff -ru course/12-apic course/13-threads` (in the repository) shows every line this chapter adds.

## Background: a thread is a stack
What does the CPU need to continue a piece of code later? Its registers – the instruction pointer, the stack
pointer, the rest – and its stack, where the calls that led here keep their data. A **thread** is exactly that: a
stack of its own and the registers that go with it.

Switching from thread A to thread B is therefore: push A's registers onto A's stack, remember A's stack pointer,
load B's stack pointer, pop B's registers. B continues wherever it stopped – because B stopped in the very same
function, which now returns. This is the **context switch**.

Who decides when to switch? Threads may give up the CPU themselves (`yield_now`). But a thread that never does –
a loop, a bug – would keep it for ever. So the timer interrupt of chapter 12, 100 times a second, may move the
running thread to the end of the queue of ready threads and switch to the first one: **preemptive round-robin**
scheduling.

And a thread that waits – for a time, a key, another thread – should not take turns at all. It is **blocked**:
out of the queue until whoever makes the thing happen wakes it. When no thread is ready, the **idle thread** runs
and halts the CPU until the next interrupt.

## Step 1: the context switch
`switch_context` is assembly – Rust has no way to say "now continue on another stack". It saves only the six
registers a function must keep for its caller (rbx, rbp, r12–r15 in the System V ABI); for the compiler this is
a normal function call, so it already assumes all other registers are changed:

```rust,file=kernel/src/task.rs
    "switch_context:",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
```

…and then the same six `pop`s in reverse order, and `ret`. The kernel is built without SSE, so there are no
vector registers to save either.

A new thread has never been switched away from, so its stack is filled by hand as if it had: six registers –
r12 holding a pointer to the closure to run – and as return address a small trampoline that calls
`thread_start(r12)`:

```rust,file=kernel/src/task.rs
    // lowest address first: r15, r14, r13, r12, rbx, rbp, return address
    let trampoline = thread_trampoline as unsafe extern "C" fn() as usize as u64;
    let frame = [0, 0, 0, entry, 0, 0, trampoline];
```

`thread_start` turns interrupts on (the switch to it happened with them off), runs the closure, and ends the
thread. Its stack comes from chapter 9's `stack::alloc` – 32 KiB with a guard page below.

## Step 2: the scheduler
The scheduler is a map of threads (each in a `Box`, so that the stack pointer `switch_context` writes to does
not move), a queue of ready ids, the current one and the idle one. Every change happens with interrupts off: the
timer handler uses the same data, and on one CPU "interrupts off" means "nobody else runs". Picking the next
thread:

```rust,file=kernel/src/task.rs
    fn switch_to_next(&mut self) -> Option<(*mut u64, u64)> {
        let next = self.ready.pop_front().unwrap_or(self.idle);
```

`yield_now`, blocking and `exit` differ only in where they put the current thread first – back into the queue,
blocked, dead – then `schedule` switches. `kmain` becomes thread 0, "main", by simply being the one that runs
when `task::init` creates the scheduler; its registers are saved by its first switch.

A thread cannot free the stack it runs on. `exit` only marks it dead; `reap` frees the stacks of dead threads
later, in `spawn` and `join` – never in the timer interrupt, because freeing takes the page table lock, which the
interrupted thread may hold.

## Step 3: preemption
The timer handler now does three things:

```rust,file=kernel/src/interrupts.rs
    time::tick();
    // without it, the local APIC would never deliver the next tick - and it must come first: `on_tick` may switch to
    // another thread, and this handler finishes only when this thread runs again
    lapic::end_of_interrupt();
    task::on_tick();
```

`on_tick` wakes the sleepers whose tick has come, and if any thread is ready, calls `yield_now` – from inside the
interrupt handler. That is fine: the handler runs on the interrupted thread's stack, and it finishes (with
`iretq`) when that thread is switched back to.

## Step 4: waiting without losing the wake-up
Before this chapter, `sleep_ms` and `read_key` waited in `hlt` loops. Now both call one function:

```rust,file=kernel/src/task.rs
pub fn wait_for<T>(wait: Wait, mut ready: impl FnMut() -> Option<T>) -> T {
```

`ready` checks the condition – a byte in the keyboard buffer, a tick reached, a thread dead. If it is not there,
the thread blocks as `wait`. The keyboard handler calls `task::wake(Wait::Input)`, the timer wakes `Until(tick)`,
`exit` wakes `Join(id)`.

The trap is the **lost wake-up**: check "no key yet" – the key arrives, its handler wakes nobody, since nobody is
blocked yet – block, and wait for a key that has already come. `wait_for` checks and blocks with interrupts off,
so no handler can run in between. Before `task::init` there are no threads, and `wait_for` halts like the old
loops did.

## Step 5: a heap for interrupt handlers
Chapter 10 kept the heap's spin lock with interrupts on and said why that was safe: no interrupt handler
allocated. That is over – the timer handler puts threads into the queue, and `wake` collects them in a `Vec`. If
a tick came while a thread held the heap lock, the handler would wait for the lock, and the thread that holds it
never runs again. So the heap now takes its lock only with interrupts off:

```rust,file=kernel/src/heap.rs
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| self.alloc_locked(layout))
    }
```

`without_interrupts` turns them back on only if they were on before, so it works inside interrupt handlers and
nested.

The console's lock is a different case: it is never taken in an interrupt handler, so a thread that waits for it
cannot deadlock – but a thread preempted while it prints holds it until its next turn, and another thread that
prints spins until its own time slice is over. The reference kernel has a *sleeping* lock for this.

What the reference kernel's scheduler ([`kernel/src/task/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/kernel/src/task))
has besides: several CPUs sharing one queue (each with its own idle thread, and wake-up interrupts between them),
FPU/SSE state per thread, user threads with their own address spaces, CPU time per thread for `ps`, and the
sleeping lock.

## The tests
`make test` now runs 92 tests in the kernel; ten are new:
- `a_new_stack_returns_into_the_trampoline` – the hand-made first stack, checked word by word;
- `a_thread_runs_and_is_joined` – spawned, still ready (main keeps the CPU), run, joined, and gone from the table;
- `yield_takes_turns` – two threads that yield three times each;
- `a_thread_that_never_yields_is_preempted` – a loop without yield or sleep; main sleeps 50 ms and comes back;
- `a_sleeping_thread_is_blocked_until_its_tick`;
- `ended_threads_give_their_stack_back` – 20 threads of 8 pages, and the 160 frames are free again after `join`;
- `the_timer_can_allocate_while_a_thread_holds_the_heap` – a thread that allocates in a loop while main wakes up
  20 times; with the heap's lock taken with interrupts on, it hangs every time (three of three runs);
- `the_idle_thread_is_thread_1_and_never_queued` (bug #28), `only_main_and_idle_are_left`, and in the shell
  `threads_lists_the_shell_itself`.

The boot check types `threads` as a fifth command. Concurrency bugs are often rare, so I ran the test kernel seven
times under QEMU 10.2 and three times under 8.2.2: all green.

## Run it
```sh
cd course/13-threads
make run
```
After the APIC lines of chapter 12:

```text
Threads: kmain is now thread 0, "main"; the idle thread halts the CPU when nobody else is ready
Threads: while main slept 200 ms, threads 2 and 3 counted to 965585 and 996017 - both ran: yes
Shell: type help and Enter
yak> threads
   0  main     Running
   1  idle     Ready
```
Two threads that never give up the CPU both counted – the timer took it from them in turns – and main, which was
blocked, came back after its 200 ms. The counts differ from run to run. `sleep 5000` in the shell still takes 5.2
seconds on a watch.

## What can go wrong
- **The end of interrupt after `on_tick`:** the handler switches to another thread before telling the local APIC
  it is done – and the APIC delivers no further tick until the interrupted thread runs again, which needs a tick.
  The boot stops after `Threads: kmain is now thread 0`; in the tests, `a_thread_that_never_yields_is_preempted`
  hangs until the 30 s limit.
- **No `sti` in `thread_start`:** new threads run with interrupts off. In the boot, the first counter thread then
  keeps the CPU – no tick can take it away – and the boot stops at the same place. In the tests it is the idle
  thread, the first time a test sleeps: its `hlt` with interrupts off never ends.
- **The scheduler visible half built:** the first version of `task::init` installed the scheduler with the idle
  id still 0 – `main`'s – and spawned the idle thread afterwards, with interrupts on. One boot in 20 a tick came in
  between, `yield_now` took `main` for the idle thread and never queued it again, and the kernel halted for good
  after the `Keyboard:` line ([bug #28](../BUGS.md)). Now `init` builds both threads first and installs the
  scheduler in one step with interrupts off.
- **The heap's lock with interrupts on:** six test runs pass without the new test. With it, the timer finds the
  lock held and the kernel hangs in `the_timer_can_allocate_while_a_thread_holds_the_heap` – in three of three
  runs.

## Experiments
1. Give each thread a count of how many ticks it ran, and show it in `threads`.
2. Add a shell command `spin` that starts a thread counting for ever. Does the shell stay usable? Start three.
3. Change the time slice: switch only every second tick. What changes in the counts of the boot demonstration?
4. Write a sleeping mutex: a thread that finds it locked blocks (a new `Wait::Lock`) instead of spinning, and
   `unlock` wakes one waiter. Which lost wake-up do you have to avoid here?

The kernel now does several things at once – on one CPU. The reference kernel goes on to start the other CPUs,
which needs the local APIC of chapter 12 once more.
