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

## Known pitfalls (not bugs, but worth remembering)
- **The Limine host utility (`limine bios-install`) needs a C compiler** (`gcc`). It is not required for booting the ISO
  in QEMU with `-cdrom`; it is only needed to make the ISO bootable as a hard disk / USB image.
- **`sudo` does not work through `!` in Claude Code** because there is no TTY for the password prompt. The user runs installs in a separate terminal.
- **`cargo fmt -- <file>` formats the whole crate**, not just the file. Format deliberately with `make fmt`, and commit pure
  formatting separately from functional changes.
- **Limine's stack has no guard page** – a stack overflow silently corrupts memory instead of faulting (until M9 gives us our own stack).
