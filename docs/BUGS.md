# Bug-Log

Format: Datum · Symptom · Ursache · Fix · Test/Commit

_Noch keine Bugs._ 🎉

## Bekannte Stolperfallen (kein Bug, aber merken)
- **Limine-Hostprogramm (`limine bios-install`) lässt sich nicht bauen**, weil kein C-Compiler installiert ist.
  Für QEMU mit `-cdrom` ist es auch nicht nötig. Erst wenn wir ein USB- oder HDD-Image wollen, brauchen wir `gcc` (`dnf install gcc`).
- **`sudo` funktioniert nicht über `!` in Claude Code**, weil dort kein TTY zur Passworteingabe da ist. Installationen mit sudo führt der User in einem eigenen Terminal aus.
