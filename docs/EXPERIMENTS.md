# Experiments

Each experiment changes **one thing**, asks you to predict the result, and then shows what really happens. Undo every
change afterwards (`git checkout -- <file>`) and run `make check` to see that the system is whole again.

The "Measured" lines come from one run on 2026-10-26 (QEMU TCG on a 4-core host, debug build). Your numbers will differ;
the *direction* should not.

---

## 1. More cores, same work
**Question:** `demo 4` starts four threads that each count primes. How much faster are they with four cores than with one?

**Do:**
```sh
KERNEL_SMP=1 make run     # then type: demo 4
make run                  # 4 cores (the default), type: demo 4
```
**Measured:** 590 ms with 1 CPU, 250 ms with 4 CPUs.

**Why:** with one CPU the timer interrupt switches between the four workers every 10 ms – they take turns. With four,
the scheduler ([`schedule`](../kernel/src/task/mod.rs)) runs them at the same time on different cores. The speed-up is
less than 4×: the emulated CPUs share the host, the shell and network threads run too, and starting threads costs time.
Walkthrough: [01 – boot](walkthrough/01-boot.md), section on `smp::init`.

---

## 2. Forget the floating-point registers
**Question:** the kernel saves and restores the FPU/SSE/AVX registers of a thread on every switch. What breaks without it?

**Do:** in [`schedule`](../kernel/src/task/mod.rs), replace
```rust
        if let Some(old) = old_fpu {
            fpu::save(&mut *old);
        }
        fpu::restore(&*new_fpu);
```
with `let _ = (old_fpu, new_fpu);`, then `make run` and type `fputest`.

**Measured:**
```
worker 2: MXCSR changed to 0x1f80 in round 1
worker 3: MXCSR changed to 0x1f80 in round 1
worker 1: XMM registers changed in round 1
worker 4: XMM registers changed in round 5
error: fputest exited with code 4
```
**Why:** the registers belong to the CPU, not to a thread. Without saving them, a thread continues with whatever the
previous thread left behind – here each worker sees the others' values and rounding modes. Programs computing with
`f64` would silently get wrong results. See M24 in [`LEARN.md`](LEARN.md).

---

## 3. Undo a bug fix: check, then copy – without the lock
**Question:** bug #14 was fixed by copying user memory *while* holding the lock that also protects the page tables.
Is the lock really needed, or would checking first be enough?

**Do:** in [`locked_user_copy`](../kernel/src/process/syscall.rs), keep the check inside `with_current` but move the
copy behind it:
```rust
    with_current(|p| {
        p.populate(addr, len) && p.space.as_ref().is_some_and(|s| s.is_accessible(addr, len, writable))
    })
    .filter(|&ok| ok)
    .ok_or(errno::EFAULT)?;
    crate::arch::x86_64::cpu::user_access(copy); // after the lock is released
    Ok(())
```
then `KERNEL_SMP=1 make run` and type `syscheck race 5000`.

**Measured (2 of 2 runs):**
```
KERNEL PANIC: panicked at src/arch/x86_64/interrupts.rs:111:5:
EXCEPTION: PAGE FAULT at address 0x000000000044b000
```
**Why:** between the check and the copy, the timer can switch to the program's other thread, which shrinks the heap
with `brk` and unmaps exactly that page. The kernel then touches an unmapped address in ring 0 – fatal. With one CPU
this happens within a second, because every timer tick is a chance to switch at the worst moment. This is a
**time-of-check to time-of-use** race; see [walkthrough 04](walkthrough/04-page-fault.md) and bug #14 in [`BUGS.md`](BUGS.md).

---

## 4. A pipe that holds 16 bytes
**Question:** a pipe buffers 64 KiB. What happens if it can only hold 16 bytes – does anything break?

**Do:** set [`PIPE_CAPACITY`](../kernel/src/process/pipe.rs) to `16`, then `make run` and type `time pipetest`
(it pushes 200 KB through a pipe and checks every byte).

**Measured:** `pipetest: all checks passed` in both cases – 1.40 s with 64 KiB, 5.41 s with 16 bytes.

**Why:** correctness does not depend on the size: a full pipe makes the writer block until the reader made room
([`write`](../kernel/src/process/pipe.rs) / [`read`](../kernel/src/process/pipe.rs) in `pipe.rs`). Only the cost
changes – with 16 bytes every few bytes need a wake-up and two thread switches. That is the trade-off behind every
buffer size in an operating system. Walkthrough: [03 – pipeline](walkthrough/03-pipeline.md).

---

## Ideas for your own experiments
- Set [`TIMER_HZ`](../kernel/src/time.rs) to 10 and run `demo 4` with one CPU: what changes, and what does `hello`
  report for its 100 ms sleep?
- Make [`CACHE_SECTORS`](../kernel/src/fs/mod.rs) tiny (8) and compare `time wc` of a large file.
- Run `syscheck 100000 <seed>` with different seeds – can you find a bug the fixed seed does not?
