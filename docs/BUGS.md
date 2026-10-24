# Bug-Log

Format: Datum · Symptom · Ursache · Fix · Test/Commit

### #1 · 2026-10-24 · Erfolgreicher Test wird als Fehler gemeldet
- **Symptom:** `make test` gibt „All tests passed.“ aus, Cargo meldet aber `exit status: 33`.
- **Ursache:** `run-qemu.sh` hat Test-Binaries am Pfad `*/deps/*` erkannt. Das aktuelle Cargo-Nightly (1.101) legt sie stattdessen unter
  `target/<triple>/debug/build/<pkg>/<hash>/out/` ab. Deshalb lief der Runner im Run-Modus und hat den rohen Exitcode 33 durchgereicht.
- **Fix:** Testmodus am Hash-Suffix im Dateinamen erkennen (`-[0-9a-f]{16}$`). Das funktioniert in beiden Layouts.
- **Test:** `make test` → 3× PASS, Exitcode 0. Gegenprobe mit absichtlich falschem Assert → FAIL, Exitcode 35.

## Bekannte Stolperfallen (kein Bug, aber merken)
- **Limine-Hostprogramm (`limine bios-install`) lässt sich nicht bauen**, weil kein C-Compiler installiert ist.
  Für QEMU mit `-cdrom` ist es auch nicht nötig. Erst wenn wir ein USB- oder HDD-Image wollen, brauchen wir `gcc` (`dnf install gcc`).
- **`sudo` funktioniert nicht über `!` in Claude Code**, weil dort kein TTY zur Passworteingabe da ist. Installationen mit sudo führt der User in einem eigenen Terminal aus.
