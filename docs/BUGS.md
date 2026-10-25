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

## Known pitfalls (not bugs, but worth remembering)
- **The Limine host utility (`limine bios-install`) needs a C compiler** (`gcc`). It is not required for booting the ISO
  in QEMU with `-cdrom`; it is only needed to make the ISO bootable as a hard disk / USB image.
- **`sudo` does not work through `!` in Claude Code** because there is no TTY for the password prompt. The user runs installs in a separate terminal.
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
