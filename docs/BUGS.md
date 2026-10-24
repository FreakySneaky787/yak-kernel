# Bug-Log

Format: Datum · Symptom · Ursache · Fix · Test/Commit

### #1 · 2026-10-24 · Erfolgreicher Test wird als Fehler gemeldet
- **Symptom:** `make test` gibt „All tests passed.“ aus, Cargo meldet aber `exit status: 33`.
- **Ursache:** `run-qemu.sh` hat Test-Binaries am Pfad `*/deps/*` erkannt. Das aktuelle Cargo-Nightly (1.101) legt sie stattdessen unter
  `target/<triple>/debug/build/<pkg>/<hash>/out/` ab. Deshalb lief der Runner im Run-Modus und hat den rohen Exitcode 33 durchgereicht.
- **Fix:** Testmodus am Hash-Suffix im Dateinamen erkennen (`-[0-9a-f]{16}$`). Das funktioniert in beiden Layouts.
- **Test:** `make test` → 3× PASS, Exitcode 0. Gegenprobe mit absichtlich falschem Assert → FAIL, Exitcode 35.

### #2 · 2026-10-24 · Timer-Interrupt kommt nie an (Test hängt in `sleep_ms`)
- **Symptom:** `sleep_ms` kehrt nie zurück, Test-Timeout. `-d int` zeigt keinen einzigen Vektor 0x20, obwohl RFLAGS.IF = 1.
- **Diagnose:** QEMU-Monitor `info pic` → `pic0: irr=01 imr=fe irq_base=20`, der PIC hat den IRQ also korrekt anstehen.
  `info lapic` → `LVT0 0x00018700 … masked ExtINT`: Limine maskiert LINT0 des Local APIC, und genau darüber laufen die PIC-Interrupts.
- **Fix:** `lapic::disable()` löscht Bit 11 (global enable) in MSR `IA32_APIC_BASE`. Dann gehen PIC-Interrupts direkt an den INTR-Pin der CPU.
- **Test:** `irq::tests::local_apic_disabled_for_pic_mode`, `timer_ticks_advance`, `sleep_waits_roughly`. Unter BIOS und UEFI geprüft.
- **Merken:** Bei „Interrupts kommen nicht an“ zuerst `info pic` / `info lapic` im QEMU-Monitor anschauen. Dafür gibt es `scripts/qemu-monitor.py <elf> [sek] -- "info pic" "info lapic"`.

## Bekannte Stolperfallen (kein Bug, aber merken)
- **Limine-Hostprogramm (`limine bios-install`) lässt sich nicht bauen**, weil kein C-Compiler installiert ist.
  Für QEMU mit `-cdrom` ist es auch nicht nötig. Erst wenn wir ein USB- oder HDD-Image wollen, brauchen wir `gcc` (`dnf install gcc`).
- **`sudo` funktioniert nicht über `!` in Claude Code**, weil dort kein TTY zur Passworteingabe da ist. Installationen mit sudo führt der User in einem eigenen Terminal aus.
