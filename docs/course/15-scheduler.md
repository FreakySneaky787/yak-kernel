# 15 – One scheduler for all CPUs

**Goal:** let every CPU run threads – one queue, one scheduler, as many CPUs as the machine has – and find out which
of the old certainties of chapter 13 no longer hold.
**You end up with:** a scheduler whose lock is held through every switch, a timer and an idle thread on every CPU,
waiting that cannot lose a wake-up sent from another CPU, and `run_on` jobs that run in an interrupt handler. The
end of part IV.
**Code:** [`course/15-scheduler/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/15-scheduler)
(chapter 14 with a new `task.rs`, a timer per CPU in `interrupts.rs`, the APs joining the scheduler in `smp.rs`)
**What changed:** `diff -ru course/14-smp course/15-scheduler` (in the repository) shows every line this chapter adds.

## Background: "interrupts off" was a lock – for one CPU
Chapter 13's scheduler touches its state with interrupts off, and on one CPU that was enough: nothing else runs
while the timer cannot interrupt. Chapter 14 started three more CPUs and kept them away from the scheduler for
exactly this reason. Now they take part, and three things break that worked before:

- **The scheduler's data.** `SCHEDULER` was always behind a `spin::Mutex`, but the lock was released *before* the
  switch. On one CPU nobody could take it in between. With four, another CPU can take the old thread from the queue
  while this CPU is still pushing its registers onto its stack – two CPUs, one stack.
- **Which thread is "current".** There is a running thread per CPU now, and an idle thread per CPU.
- **Waiting.** "Check, then block" (chapter 13's `wait_for`) has a gap: the event can happen on another CPU between
  the check and the block, and its wake-up finds a thread that is not blocked yet. The wake-up is lost; the thread
  sleeps until something else wakes it – maybe never.

## Step 1: per-CPU state in one scheduler
The scheduler keeps, for every CPU, the thread it runs and its idle thread; a thread remembers the CPU it is on:

```rust,file=kernel/src/task.rs
    /// The thread each CPU runs, and its idle thread (`None`: that CPU has not joined yet).
    current: [Option<Id>; MAX_CPUS],
    idle: [Option<Id>; MAX_CPUS],
```

`on_cpu` is set when a CPU switches to a thread and cleared only after it has switched away from it completely – so
"in the queue" and "still on a CPU" can be true at the same time for a moment, and the code must not mix them up.

The APs join in `ap_main`: after their GDT, IDT and local APIC (chapter 14) they start their own timer and turn the
code that runs there into their idle thread:

```rust,file=kernel/src/smp.rs
    lapic::init_local();
    interrupts::start_timer_here();
    ONLINE[cpu].store(true, Ordering::Release);
    task::join_cpu(cpu)
```

`start_timer_here` uses the count the BSP measured in chapter 12: all local APIC timers of a machine run from the
same clock. Every CPU's timer interrupt calls `task::on_tick`, but only one may count the time – otherwise four CPUs
would make the clock run four times as fast:

```rust,file=kernel/src/interrupts.rs
    // every CPU has a timer now, but the clock is counted once: by the BSP's
    if smp::cpu() == 0 {
        time::tick();
    }
```

## Step 2: the lock stays held through the switch
`schedule` decides what runs next under the lock – and then does *not* release it:

```rust,file=kernel/src/task.rs
    core::mem::forget(guard);
    unsafe { switch_context(old_rsp, new_rsp) };
    finish_switch();
}
```

`forget` drops the guard without unlocking. The thread that runs after `switch_context` – a thread coming back from
its own `schedule`, or a new one in `thread_start` – calls `finish_switch`, and only then is the old thread really
off this CPU:

```rust,file=kernel/src/task.rs
fn finish_switch() {
    let cpu = smp::cpu();
    // Safety: the lock is held (by `schedule` on this CPU, which forgot its guard), so nobody else touches the data.
    let s = unsafe { &mut *SCHEDULER_DATA.load(Ordering::Relaxed) };
    let previous = PREVIOUS[cpu].load(Ordering::Relaxed);
    if let Some(thread) = s.threads.get_mut(&previous) {
        thread.on_cpu = None;
        // A thread that ended is only now off its stack: from here on `join` may return and `reap` may free it.
        if thread.state == State::Dead {
            s.wake(|wait| wait == Wait::Join(previous));
        }
    }
    unsafe { SCHEDULER.force_unlock() };
}
```

A thread that ended is a special case: until this moment its stack was still in use, so its joiners are woken only
now – `exit` just marks it dead. The lock is ours, but the guard that would let us reach the data is gone – so `init` keeps a raw pointer to the
scheduler, `SCHEDULER_DATA`, and `force_unlock` releases the lock by hand. Both are `unsafe` for a good reason: they
are correct only because every path into a thread goes through `finish_switch` exactly once.

## Step 3: announce the wait, then check
`wait_for` now marks the thread blocked *first* and asks `ready` second:

```rust,file=kernel/src/task.rs
            set_state(State::Blocked(wait));
            if let Some(value) = ready() {
                set_state(State::Running);
                return value;
            }
            schedule(); // returns when a wake-up made the thread run again - or found it still here
```

A wake-up from another CPU can now come at any point. Before `ready` – then `ready` sees the event. After it – then
the thread is blocked and the waker finds it. The waker must only not queue a thread that is still on a CPU – it
would run twice. So `wake_one` cancels the block instead:

```rust,file=kernel/src/task.rs
        if thread.on_cpu.is_some() {
            thread.state = State::Running;
            return;
        }
```

`schedule` sees `Running`, keeps the thread on its CPU, and the loop asks `ready` again.

## Step 4: waking idle CPUs, and jobs
A CPU with nothing to do halts in its idle thread. When a thread becomes ready – `spawn`, a wake-up – the scheduler
sends one idle CPU the wake-up interrupt of chapter 14 (`kick_idle_cpu`), and its idle thread looks at the queue.

`run_on` changes too. In chapter 14 the APs ran jobs in their wait loop; now an AP may be busy with a thread when a
job arrives – even with the thread that sent it, which may have moved there. So jobs run in the handler of the
wake-up interrupt, whatever the CPU is doing. That gives them the rules of interrupt handlers: no waiting, and no
lock that is ever taken with interrupts on.

## The tests
`make test` now runs 100 tests in the kernel; these are new or changed:
- `every_cpu_has_an_idle_thread_that_is_never_queued` and `only_main_and_the_idle_threads_are_left`;
- `every_cpu_has_its_own_timer` – every CPU's timer interrupt count grows during 50 ms;
- `threads_run_on_every_cpu` – as many spinning threads as CPUs; together they ran on all CPUs;
- `no_wake_up_is_lost_between_cpus` – two threads play ping-pong 2000 times through a counter, each waiting for its
  turn with `wait_for` and waking the other. One lost wake-up, and both wait for ever.
- `the_scheduler_lock_is_free_after_a_switch` – 100 yields and a short thread, and the lock is still usable.

Three old tests had to learn about other CPUs: a new thread no longer waits while `main` has interrupts off – an
idle CPU takes it at once; the thread that should be asleep may still be on its way there on another CPU; and
threads spawned for the stack test would end – and give their stacks back – before the frames were counted. They
now wait for a signal (`Wait::Event`). The first of them was found by CI, not here: on the runner an idle CPU was
quicker than on this machine.

To find such timing assumptions, the unit tests ran 8 more times, 4 of them with all host cores kept busy. That
found two more problems: `join` returned while the ended thread was still on its CPU, so its stack could not be
freed yet ([bug #30](../BUGS.md)) – and a timer test of chapter 12 whose window was too narrow for a busy machine
([bug #29](../BUGS.md)).

Two counter-checks were run, 3 times each: with chapter 13's order in `wait_for` (check, then block), the ping-pong
test hangs every time; with the lock released before `switch_context`, the same. The chapter's test kernel passed
with `KERNEL_SMP=1`, `2` and `4`, and the whole `make test` three times in a row.

## Run it
```sh
cd course/15-scheduler
make run
```
After chapter 14's `SMP:` lines:

```text
Scheduler: 4 threads that never yield, 100 ms - they ran on the CPUs [0, 1, 2, 3]
```
and in the shell, `threads` now says where each thread is:

```text
   0  main     Running, on CPU 0
   1  idle     Ready
   2  idle     Running, on CPU 1
   3  idle     Running, on CPU 2
   4  idle     Running, on CPU 3
```

## What can go wrong
- **The lock released before the switch:** another CPU runs a thread whose registers are only half saved. Here it
  showed as a hang in the ping-pong test; in a bigger kernel it is a crash somewhere far away.
- **A path into a thread that skips `finish_switch`:** the lock stays held, and the next CPU that wants it spins for
  ever – every CPU, one after another. New threads start in `thread_start`, so it calls `finish_switch` first.
- **"Ended" is not "off the CPU":** a joiner that is woken when the thread is marked dead runs while the dead thread
  is still on its stack (bug #30) – fine for the result, too early for freeing the stack.
- **"Check, then block":** wake-ups get lost, rarely – which is why the ping-pong test plays 2000 rounds.
- **Every CPU counting the time:** each CPU adds its own 100 ticks a second, so with four CPUs the clock runs four
  times too fast and every sleep ends early.
- **A check that assumed the BSP:** writing this chapter, the double fault test failed – the handler compared its
  stack with the BSP's double fault stack, but `main` had moved to another CPU, which has its own. `gdt::
  on_double_fault_stack` now asks for the stack of the CPU it runs on.

## Experiments
1. Count how often each thread changes CPU (`on_cpu` differs from last time). Is moving between CPUs rare or common?
   What does it cost – which caches are cold on the new CPU?
2. Remove `kick_idle_cpu`. Do threads still get run? How much later?
3. Run the ping-pong test with `KERNEL_SMP=1`. Why can the old `wait_for` not lose a wake-up there?

The kernel can now do several things at the same time for real. Part V gives it something worth waiting for: a disk.
