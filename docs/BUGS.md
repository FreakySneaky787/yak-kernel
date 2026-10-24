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
- **Remember:** when interrupts do not arrive, look at `info pic` / `info lapic` first:
  `python3 -I scripts/qemu-monitor.py <elf> [s] -- "info pic" "info lapic"`.

## Known pitfalls (not bugs, but worth remembering)
- **The Limine host utility (`limine bios-install`) needs a C compiler** (`gcc`). It is not required for booting the ISO
  in QEMU with `-cdrom`; it is only needed to make the ISO bootable as a hard disk / USB image.
- **`sudo` does not work through `!` in Claude Code** because there is no TTY for the password prompt. The user runs installs in a separate terminal.
- **Limine's stack has no guard page** – a stack overflow silently corrupts memory instead of faulting (until M9 gives us our own stack).
