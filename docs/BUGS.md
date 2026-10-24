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
- **Limine's stack has no guard page** – a stack overflow silently corrupts memory instead of faulting (until M9 gives us our own stack).
