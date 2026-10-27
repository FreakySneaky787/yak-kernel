# Bug log

Format: number · date · title, then symptom → cause → fix → test.

### #1 · 2026-10-24 · Passing test reported as failure
- **Symptom:** `make test` prints "All tests passed.", but cargo reports `exit status: 33`.
- **Cause:** `run-qemu.sh` detected test binaries by the path `*/deps/*`. Current cargo nightly (1.101) places them under
  `target/<triple>/debug/build/<pkg>/<hash>/out/` instead, so the runner used run mode and passed the raw exit status 33 through.
- **Fix:** detect test mode by the hash suffix in the file name (`-[0-9a-f]{16}$`), which works with both layouts.
- **Test:** `make test` → 3× PASS, exit status 0. Counter-check with a deliberately wrong assert → FAIL, exit status 35.

### #2 · 2026-10-24 · Timer interrupt never arrives (test hangs in `sleep_ms`)
- **Symptom:** `sleep_ms` never returns, test timeout. `-d int` shows not a single vector 0x20 although RFLAGS.IF = 1.
- **Diagnosis:** QEMU monitor `info pic` → `pic0: irr=01 imr=fe irq_base=20`, so the PIC correctly has the IRQ pending.
  `info lapic` → `LVT0 0x00018700 … masked ExtINT`: Limine masks LINT0 of the local APIC, which is exactly where PIC interrupts enter.
- **Fix:** `lapic::disable()` clears bit 11 (global enable) in MSR `IA32_APIC_BASE`, so PIC interrupts go directly to the CPU's INTR pin.
- **Test:** `irq::tests::local_apic_disabled_for_pic_mode`, `timer_ticks_advance`, `sleep_waits_roughly`. Verified with BIOS and UEFI.
- **Update (M12):** superseded – the kernel now uses the local APIC + I/O APIC and masks LINT0 on purpose; the 8259 is fully masked.
- **Remember:** when interrupts do not arrive, look at `info pic` / `info lapic` first:
  `python3 -I scripts/qemu-monitor.py <elf> [s] -- "info pic" "info lapic"`.

### #3 · 2026-10-24 · `hexdump 0` shows IDENTIFY data instead of the boot sector
- **Symptom:** in the test run, reading sector 0 through the global disk returned the drive's IDENTIFY data ("QM0000 1" serial number).
- **Cause:** AHCI unit tests called `ahci::probe()` again. Re-initializing port 0 pointed its command-list/FIS registers at the
  test's new frames; the global `AhciDisk` kept filling its old (now ignored) command list, so the HBA executed stale commands and
  the data landed in another bounce buffer.
- **Fix:** `probe()` refuses to run twice (`PROBED` flag); tests use the global `drivers::DISK`.
- **Test:** `ahci::tests::probing_twice_is_refused`, `shell::tests::hexdump_shows_boot_signature`.
- **Remember:** hardware state is global – a device must have exactly one driver instance.

### #4 · 2026-10-24 · Ctrl+C stops working in /bin/sh after `spawntest`
- **Symptom:** E2E step `fault spin` + Ctrl+C hung under `/bin/sh` – the spinner was never killed.
- **Cause:** children of the foreground process inherited the foreground *at spawn*. `spawntest` starts an orphan (`hello`)
  that nobody waits for; it took the foreground, and when it ended the foreground fell back to its dead parent → 0.
  From then on nobody owned the console, so `sh`'s children were not foreground either and Ctrl+C arrived as a plain byte.
- **Fix:** the console is *lent* only while a foreground process waits for a child (`process::wait_child`) and returned when
  the wait ends – like a real shell's `tcsetpgrp`. `spawn` never changes the foreground.
- **Test:** E2E `spawntest` followed by `fault spin<pause>^C`, `fault sleep<pause>^C`, `upper<pause>^C` (all under `/bin/sh`).
- **Remember:** "who owns the console" must follow who is *waiting*, not who was started.

### #5 · 2026-10-24 · `smp::run_on` hangs once more threads exist
- **Symptom:** after the network thread was added, the unit test `jobs_run_on_their_cpu` timed out.
- **Cause:** `run_on` waited for the job with `yield_now()` in a loop. If the waiting thread happened to run on the target
  CPU itself, it was always ready there, so that CPU's idle thread – the one that executes jobs – was never scheduled.
  On an otherwise idle system the thread rarely ended up there; more scheduling activity made it likely.
- **Fix:** wait by *blocking* (`time::sleep_ms(1)`), which lets the target CPU's idle thread run.
- **Test:** `smp::tests::jobs_run_on_their_cpu` (now stable with the net thread running).
- **Remember:** a busy-yield loop is not waiting – a ready thread starves the idle thread of its CPU.

### #6 · 2026-10-25 · `run_on(0, …)` hangs sometimes (about 1 in 15 test runs)
- **Symptom:** `jobs_run_on_their_cpu` timed out now and then; all CPUs were idle in `hlt`.
- **Cause:** only the APs' idle loop ran jobs. Since M26 the calling thread may run on an AP, and a job for CPU 0 then waited
  forever in the mailbox: the boot CPU's idle thread (`task::idle_loop`) never looked into it. Found with a temporary scheduler
  dump after 25 s plus `info registers` of all CPUs through the QEMU monitor.
- **Fix:** one `smp::idle_loop(cpu)` for every CPU; running inline on the own CPU happens with interrupts off.
- **Test:** `smp::tests::jobs_for_every_cpu_from_every_cpu` (4 threads send jobs to all CPUs; hangs reliably with the old code).
- **Remember:** with SMP every per-CPU duty must exist on *every* CPU, including the boot CPU.

### #7 · 2026-10-25 · With one CPU, every disk command took 10 ms
- **Symptom:** after switching AHCI to MSI, `KERNEL_SMP=1 make test` timed out; `time hello` took 0.38 s instead of 0.1 s.
- **Cause:** the reading thread blocked, the CPU went to its idle thread and `hlt`. The completion interrupt made the thread
  ready, but the CPU returned into the idle thread and only switched at the next timer tick. With more CPUs `make_ready`
  sends a wake-up IPI to *another* idle CPU, which hid the problem; nothing kicks the CPU that handles the interrupt itself.
- **Fix:** `task::preempt_idle()` at the end of interrupt handlers that wake threads (AHCI, keyboard, COM1): an idling CPU
  switches to the woken thread at once.
- **Test:** the integration test with `KERNEL_SMP=1` (timing of `time hello` and the whole run under 30 s).
- **Remember:** run the suite with one CPU after scheduler or interrupt changes – some paths only exist there.

### #8 · 2026-10-25 · MSI test flaky: "only 12 interrupts for 16 commands"
- **Symptom:** `ahci::tests::commands_complete_by_msi_interrupt` failed in about 1 of 10 runs with `KERNEL_SMP=2`.
- **Cause:** the test's assumption was wrong, not the driver. QEMU often completes a command before the waiter blocks
  (`wait_for` checks PxCI first), so the thread goes on without needing the interrupt. The MSI for it is still sent to the
  boot CPU; when that CPU runs with interrupts off for a moment, the next command's MSI arrives while the first one is
  still pending in the local APIC's IRR – both set the same bit and become *one* interrupt.
- **Fix:** the test only requires that completion interrupts arrive at all (waiting up to 100 ms for the first).
- **Test:** the test itself (repeated runs with 2 CPUs).
- **Remember:** interrupts with the same vector coalesce while pending – never count interrupts to count events; the
  handler must look at the device's status registers (ours does).

### #9 · 2026-10-25 · `run_on` test hung once with 2 CPUs
- **Symptom:** `smp::tests::jobs_for_every_cpu_from_every_cpu` hung once in `KERNEL_SMP=2 make test` (not again in 160
  hunted runs).
- **Cause (by analysis):** a priority inversion. The idle thread ran jobs with interrupts *enabled*, so it could be
  preempted while the job held the result lock (`*slot.lock() = ...`). An idle thread only runs when nothing else is
  ready – but the thread waiting for the result spun on that very lock, stayed ready and occupied the CPU forever. The
  waiter could likewise be preempted holding the lock while the job spun on it.
- **Fix:** jobs run with interrupts disabled; the waiter takes the result lock with interrupts disabled too. A spin lock
  that an idle thread shares with normal threads must never be held across a preemption point.
- **Test:** `jobs_for_every_cpu_from_every_cpu` with `KERNEL_SMP=2` and `4` (rare – the race needs a timer tick inside
  a few instructions).
- **Remember:** idle threads have the lowest priority by construction; anything they hold can starve.

### #10 · 2026-10-25 · `stack_overflow` test: #GP with error code 0x102 after `-cpu max`
- **Symptom:** after switching QEMU to `-cpu max`, `tests/stack_overflow.rs` died with a general protection fault
  (code 0x102) instead of the expected double fault.
- **Cause:** error code 0x102 = "IDT entry 0x20": the timer interrupt. The test loads its own IDT with only exception
  handlers, but interrupts were still enabled; the recursion used to finish before the next tick, and with the slower
  emulated CPU model a tick arrived first.
- **Fix:** test kernels with their own IDT disable interrupts before loading it (`stack_overflow.rs`, `page_fault.rs`).
- **Test:** both tests, with `-cpu max`.
- **Remember:** a #GP with bit 1 of the error code set points at an IDT vector (index = code >> 3) – usually an
  interrupt without a handler.

### #11 · 2026-10-26 · A syscall with a huge length freezes the system
- **Symptom:** found by `syscheck` (M41): `write(1, 0, 1 << 47)` never returned, the whole machine stopped responding.
- **Cause:** before checking a user buffer, `Process::populate` walked *every* page of `[addr, addr + len)` to map
  demand-paged heap/stack pages – 2^35 iterations here, under the process table lock with interrupts off.
- **Fix:** the walk stops at the first page that is neither mapped for user mode nor demand-paged (the access check fails
  there anyway). A request now costs at most the size of the mapped area it touches.
- **Test:** `syscheck` calls every pointer-taking syscall with lengths up to `u64::MAX` (integration test `userspace`, E2E).
- **Remember:** every loop over a user-supplied range must be bounded by something the kernel owns, not by the user's length.

### #12 · 2026-10-26 · Kernel panic from ring 3: empty buffer with a null pointer
- **Symptom:** `KERNEL PANIC: unsafe precondition(s) violated: slice::from_raw_parts requires the pointer to be aligned
  and non-null` in `copy_from_user`, triggered by the random phase of `syscheck`.
- **Cause:** zero-length ranges are always "accessible" (see pitfalls: empty slices have dangling pointers), but
  `copy_from_user` / `copy_to_user` still built a slice / called `copy_nonoverlapping` with that pointer. With a null
  pointer that is undefined behaviour, which the debug build checks – any program could panic the kernel.
- **Fix:** both return immediately for empty buffers, without touching the pointer.
- **Test:** `syscheck`: `write`/`read` with length 0 and pointers 0, 1, `u64::MAX` must return 0.
- **Remember:** an "accessible" empty range is no licence to dereference its pointer.

### #13 · 2026-10-26 · `readdir` with a bad buffer skipped a directory entry
- **Symptom:** `syscheck` saw `readdir(fd, bad pointer)` return 0 (end of directory) after a few calls – the failed
  calls had used up the entries.
- **Cause:** `sys_readdir` advanced the directory position before copying the entry out; the copy then failed with EFAULT.
- **Fix:** the buffer is checked first; the position only moves when the entry can be delivered.
- **Test:** `syscheck`: `readdir` into null → EFAULT, then the first real `readdir` returns the same entry as on a fresh handle.
- **Remember:** check, then change state – a syscall that fails should leave nothing behind.

### #14 · 2026-10-26 · Kernel panic: user copy raced against `brk` in another thread
- **Symptom:** `syscheck race` with `KERNEL_SMP=1`: `KERNEL PANIC ... EXCEPTION: PAGE FAULT at address 0x44b000` within
  a second – a kernel-mode page fault on a user heap address.
- **Cause:** `copy_from_user` / `copy_to_user` checked the buffer (under the process table lock) and copied it *after*
  releasing the lock. A timer interrupt between the two let another thread of the process shrink the heap; the copy then
  touched an unmapped page in ring 0, which is fatal.
- **Fix:** check and copy happen together under the process table lock (`locked_user_copy`), in pieces of at most
  64 KiB to keep the time with interrupts off short. `brk` changes the page tables under the same lock.
- **Test:** `syscheck race 1000` in the integration test (fails within a second on the old code with 1 CPU).
- **Remember:** a check of user memory is only valid while nothing can change the mapping – hold the lock until the copy is done.

### #15 · 2026-10-26 · TLB shootdown waited for idle CPUs (seconds, or forever)
- **Symptom:** `syscheck race` with 4 CPUs: only 4 heap shrinks in 5 s (~1 s per `brk`). The new unit test with a
  spinning thread on every CPU never finished.
- **Cause:** `paging::shoot_down` sent the CR3 reload as an `smp::run_on` job to every CPU. Jobs run in a CPU's idle
  thread, which only gets the CPU when no other thread is ready – a busy CPU answered late or never.
- **Fix:** own IPI vector `smp::TLB_VECTOR` (0xF1): the handler reloads CR3 if that CPU runs the address space and
  counts down a pending counter the sender spins on (with interrupts on, so two shootdowns cannot deadlock).
- **Test:** `smp::tests::tlb_shootdown_is_fast_while_all_cpus_are_busy` (20 shootdowns < 50 ms with all CPUs busy;
  the old code hangs until the 30 s timeout).
- **Remember:** work that must happen *now* on another CPU belongs in an interrupt handler, not in a job queue.

### #16 · 2026-10-26 · One `write` could make the kernel heap grow by the whole buffer
- **Symptom:** found while sorting the open items (M42) and confirmed with `syscheck`: a 16 MiB `write` into a pipe
  without a reader grew the kernel heap by 16 388 KiB before failing with EPIPE. The kernel heap never shrinks, and a
  failed kernel allocation halts the system – two programs writing their 64 MiB heaps at once could take the machine down.
- **Cause:** `sys_write` (and `sys_sendto`) copied the whole user buffer into a kernel `Vec` before doing anything.
- **Fix:** `sys_write` copies and writes in pieces of at most 64 KiB (pieces end on a UTF-8 character boundary, the
  console prints each piece as text); `sendto` refuses datagrams larger than `MAX_UDP_PAYLOAD` *before* copying (EMSGSIZE).
  Programs still see one complete write – `ulib` does not expect short writes.
- **Test:** `syscheck`: 16 MiB into a pipe without a reader → EPIPE with < 1 MiB heap growth; `sendto` of 1 GiB → EMSGSIZE.
- **Remember:** the size of a kernel allocation must never be chosen by a user program.

### #17 · 2026-10-26 · Shootdown regression test too strict for CI runners
- **Symptom:** the first CI run (M47) failed on Ubuntu and Fedora alike: `tlb_shootdown_is_fast_while_all_cpus_are_busy`
  – "20 shootdowns took 72610 us" (Fedora: 74845 us) against a limit of 50 000 us. Locally the test passed.
- **Cause:** the limit was chosen from local runs. GitHub's shared runners emulate 4 CPUs with TCG on fewer real cores,
  so every IPI round trip takes longer. Not a kernel bug.
- **Fix:** limit raised to 1 s. The test still tells the two implementations apart: the old `run_on` shootdown never
  finished with all CPUs busy (30 s timeout).
- **Test:** the test itself, green in CI on Ubuntu and Fedora.
- **Remember:** a timing assertion must separate *good* from *broken* behaviour, not *fast machine* from *slow machine* –
  pick the limit from the broken case, not from the local measurement (see `docs/HACKING.md`, "Add a test").

### #18 · 2026-10-26 · `sleep_ms` could end up to one tick early
- **Symptom:** one local E2E run: `time hello` printed `real 0.095377 s` for a program that sleeps 100 ms (and itself
  reported "Slept for 100 ms", measured in ticks).
- **Cause:** the deadline was "now + ⌈ms / 10⌉ ticks". The tick that is already running counts fully, although part
  of it is over – a sleep starting just before a tick ended almost a whole tick early; `sleep_ms(10)` could return
  after a few microseconds.
- **Fix:** one tick more (`sleep_ms_or_abort`): a sleep now lasts at least `ms` and at most `ms` + 10 ms.
- **Test:** `irq::tests::sleep_never_ends_early` – 20 sleeps of 10 ms started at different points inside a tick,
  measured with the TSC clock; fails on the old code, passes on the new.
- **Remember:** a tick counter tells *which* tick it is, not *how far into it* – deadlines must round up.

### #19 · 2026-10-26 · A new process could miss its own table entry (SMP race)
- **Symptom:** CI on Fedora, E2E step `ls /bin | grep test | sort -r`: `sort` killed by a "page fault at
  0x7fffffeefff8 (page not present, write, user mode)" in `core::panicking::panic_nounwind_fmt` – 8 bytes below the
  1 MiB stack area. Not reproducible by running the pipeline 200 times locally.
- **Cause:** two bugs in a chain. (1) `process::spawn_with` made the new thread runnable (`task::spawn_user`) *before*
  inserting the process into the table. Another CPU – woken by an IPI because a thread became ready – could start the
  program at once; its first `brk` found no table entry and failed with EBADF, so the program's allocator could never
  get memory. (2) The allocation failure made Rust panic, and `ulib`'s panic handler formatted its message with
  `eprintln!`, which allocates – it panicked again, recursively, until the stack was used up.
- **Fix:** (1) `spawn_with` holds the process table lock while it creates the thread and inserts the entry – the order
  `spawn_thread` already used. (2) The panic handler formats into a 256-byte buffer on the stack and stops at a
  second panic ("panic while panicking").
- **Test:** `process::tests::first_syscall_of_a_new_process_finds_its_table_entry` (240 processes whose first syscall
  is `brk`; failed in 4 of 6 runs on the old code with "first brk returned -9", 0 of 7 on the new, also with 1 and
  2 CPUs); `fault oom` fills the heap to the last byte and must still print "panic: memory allocation of 1048576 bytes
  failed" (old handler: the same page fault below the stack as in CI).
- **Remember:** "interrupts off" protects against *this* CPU only – on SMP, anything a new thread may look at must
  exist before the thread becomes runnable.

### #20 · 2026-10-27 · DESIGN.md said there was no block cache
- **Symptom:** an outside review of the docs found two contradicting statements: "Disk caching and interrupts" says the
  root file system runs on a 1024-sector `BlockCache`, "File system" said "No block cache: data clusters are
  read/written directly".
- **Cause:** documentation only. The "File system" text was written at M15; M32 added `BlockCache` and updated its own
  section but not the older one. The code (`fs::RootDisk = BlockCache<SystemDisk>`) was right.
- **Fix:** the "File system" section now describes the caches as they are and states the limit of the power-off
  guarantee (after a command, not during one).
- **Test:** none possible for prose; `scripts/check-doc-refs.py` only checks that named symbols exist.
- **Remember:** a milestone that changes a layer must search the docs for every statement about that layer, not only
  edit its own section.

### #21 · 2026-10-27 · The heap could grow into the stack
- **Symptom:** none observed; found while placing the thread stack slots of M52 below the main stack.
- **Cause:** `USER_IMAGE_END` only kept the ELF segments 64 KiB below the stack area. The heap starts right after the
  image and may grow by `HEAP_MAX` (64 MiB) – a program whose image ended near `USER_IMAGE_END` could `brk` its heap
  into the stack range, and both would demand-page the same addresses. Only the program itself was affected (memory
  isolation between processes held), but it broke the stack guard.
- **Fix:** `USER_IMAGE_END = THREAD_STACKS_BOTTOM - STACK_GUARD - HEAP_MAX`: the largest heap behind the highest
  allowed image ends below a guard gap.
- **Test:** `process::tests::thread_stack_slots_have_guards` asserts the layout (fails with the old constant).
- **Remember:** a limit on one region (the image) is only safe together with the limits of the regions that grow out of
  it (the heap).

### #22 · 2026-10-27 · The TCP loss test needed almost the whole time limit of its test kernel
- **Symptom:** `make stress`, pass 1 with 4 CPUs: `netfault` killed by the 30 s timeout of `run-qemu.sh` while
  `every_fifth_packet_lost_the_data_still_arrives` ran. With 1 and 2 CPUs, and in 6 single runs, it passed.
- **Cause:** the test, not the kernel. 3 echoes of 20 KB with every 5th packet lost cost 36–49 retransmission timeouts of
  at least 200 ms each (go-back-N sends everything after a loss again, and every 5th of that is lost too) – 10–15 s of
  the 30 s limit. On a busy machine that was not enough. Runs with a 120 s limit never hung (6 of 6 passed in 18–22 s).
- **Fix:** 8 KB per echo and every 10th packet lost; the loss still hits the SYN, the SYN-ACK and the first ACK once
  each. The test prints its duration.
- **Test:** 6 runs with all 4 host cores busy: 2.7–6.1 s for the test, all passed; then the full `make stress`.
- **Remember:** a test that waits for timeouts must be sized against the time limit of the machine it runs on, with
  room to spare – the CI uses 120 s, a developer machine 30 s.

## Known pitfalls (not bugs, but worth remembering)
- **The Limine host utility (`limine bios-install`) needs a C compiler** (`gcc`). It is not required for booting the ISO
  in QEMU with `-cdrom`; it is only needed to make the ISO bootable as a hard disk / USB image.
- **`sudo` does not work through `!` in Claude Code** because there is no TTY for the password prompt. I run installs in a separate terminal.
- **`cargo fmt -- <file>` formats the whole crate**, not just the file. Format deliberately with `make fmt`, and commit pure
  formatting separately from functional changes.
- **Limine sets the USER bit on its upper-half page tables** (PML4/PDPT/PD entries; the leaf entries are supervisor-only).
  Harmless while the leaves are right, but `paging::init` clears USER in all upper-half PML4 entries so ring 3 can never
  reach kernel memory by accident (found by the test `kernel_half_is_fully_populated`).
- **`x86_64-unknown-none` defaults to `code-model=kernel`** (addresses in the top 2 GiB). User programs linked at 0x40_0000
  need `-C code-model=small`, otherwise GNU ld fails with "relocation truncated to fit".
- **LLVM may delete an unused allocation and assume it succeeded.** `alloc(huge).is_null()` was optimized to `false` in
  `heaptest`, so the "allocation must fail" check failed although the kernel refused the memory. Pass the pointer through
  `core::hint::black_box` when testing allocation failure.
- **Empty slices from Rust come with dangling pointers** (e.g. `0x1` for an empty `Vec<u8>`). The syscall pointer check
  rejected `spawn(path, [])` with `EFAULT` because page 0 is unmapped; zero-length ranges are now always accepted.
- **The serial console sends `\r\n` for every `\n`.** E2E expectations spanning several lines must contain `\r\n`.
- **rustc no longer accepts `+sse` / `-soft-float` on `x86_64-unknown-none`** ("must be disabled to ensure that the ABI of
  the current target can be implemented correctly", soon a hard error). Hard-float user programs need their own target JSON
  and `-Zbuild-std`; build scripts of `core`'s dependencies then need a host C toolchain (`gcc`, glibc headers/crt files).
- **LLVM's assembler rejects label differences inside memory operands** in `global_asm!` (Intel syntax):
  `mov eax, [ebx + (a - b)]` → "cannot use more than one symbol in memory operand". Encode such instructions with
  `.byte` and put the difference into a `.word`/`.long` (see the SMP trampoline).
- **The page allocator hands out the lowest frames first**, so memory below 1 MiB is gone after early boot. Hardware that
  needs low memory (the SMP trampoline) must reserve it at `pmm::init` time.
- **`spin` 0.12's `Mutex` has no `as_mut_ptr`.** To use the data of a lock you own without a guard (the scheduler lock is
  released by the *next* thread after a switch), keep the address of the data from a guard taken at init
  (`task::SCHEDULER_DATA`) and call `force_unlock` afterwards.
- **Limine's stack has no guard page** – a stack overflow silently corrupts memory instead of faulting (until M9 gives us our own stack).
