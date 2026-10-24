# KernelTry – Hobby-Kernel in Rust (x86_64, QEMU)

Ein Kernel von Grund auf, gemeinsam mit AI geschrieben. Läuft nur in QEMU.
Sprache der Doku und Kommentare: **Deutsch**. Bezeichner im Code: Englisch.

## Zuerst lesen
1. `docs/PROGRESS.md` – wo wir stehen und was als Nächstes kommt
2. `docs/DESIGN.md` – Architektur und Begründungen, bevor du etwas Grundlegendes änderst
3. `docs/BUGS.md` – bekannte Fallen, vor allem bei Triple Faults und Boot-Problemen

## Befehle
| Befehl | Was |
|---|---|
| `make build` | Kernel bauen (`kernel/target/x86_64-unknown-none/debug/kernel`) |
| `make run` | QEMU mit Fenster (BIOS), Serial-Ausgabe im Terminal |
| `make run-uefi` | wie run, aber mit OVMF/UEFI |
| `make test` | alle In-Kernel-Tests headless in QEMU, Exitcode 0 = grün |
| `make debug` + `make gdb` | QEMU wartet auf gdb an :1234 |
| `python3 -I scripts/screenshot.py <elf> <out.png> [sek] [tasten…]` | headless booten, optional Tasten senden, Screenshot + Serial-Log |

Tests laufen ohne Fenster und brechen nach 30 s ab (`KERNEL_TEST_TIMEOUT`), du kannst sie also selbst ausführen.
Bei Interrupt-Problemen: QEMU-Monitor (`-monitor unix:<sock>,server,nowait`) mit `info pic`, `info lapic`, `info registers`.
Bei Triple Faults hilft es, an `scripts/run-qemu.sh` die Flags `-d int,cpu_reset` anzuhängen.

## Arbeitsweise (verbindlich)
- Kleine Meilensteine aus `docs/PROGRESS.md`, einer nach dem anderen.
- Ablauf pro Schritt: implementieren → `make test` grün → Doku aktualisieren → Commit `M<n>: <was>` → Push.
- Jeder Meilenstein bekommt mindestens einen Test. Jeder Bugfix bekommt einen Eintrag in `docs/BUGS.md` und, wenn möglich, einen Regressionstest.
- Am Ende jeder Session muss in `docs/PROGRESS.md` unter „Nächster Schritt“ stehen, wo es weitergeht.
- Der User arbeitet mit (Arbeitsweise im Mix). Aufgaben, die mit **[USER]** markiert sind, schreibt der User selbst. Du gibst Hinweise und reviewst.
  Wenn du neue Subsysteme erklärst, kommt eine kurze Erklärung in `docs/LEARN.md`.
- „From scratch“: GDT, IDT, Paging, Allokatoren und Treiber schreiben wir selbst.
  Erlaubte Crates: `limine` (Boot-Protokoll), `spin` (Locks). Neue Crates erst nach Rücksprache.

## Layout
- `kernel/src/lib.rs` – Kernel als Library, `init()`, Unit-Test-Einstieg
- `kernel/src/main.rs` – `kmain`, Panic-Handler (nur das Boot-Binary)
- `kernel/src/testing.rs` – Test-Runner, `Testable`, Test-Panic-Handler
- `kernel/tests/` – Integrationstests (je ein eigener Kernel)
- `kernel/src/boot.rs` – Limine-Requests
- `kernel/src/serial.rs` – COM1, Makros `serial_print!` / `serial_println!`
- `kernel/src/font.rs` – PSF2-Parser, eingebauter Font aus `kernel/assets/`
- `kernel/src/console.rs` – Framebuffer-Textkonsole, Makros `print!`/`println!` (Bildschirm + Serial)
- `kernel/src/qemu.rs` – QEMU beenden (isa-debug-exit)
- `kernel/src/arch/x86_64/` – CPU-spezifisches: `port.rs` (in/out), `gdt.rs` (GDT+TSS), `idt.rs` (IDT-Struktur), `interrupts.rs` (Exception-Handler, Kernel-IDT), `irq.rs` (Hardware-IRQs), `pic.rs`, `pit.rs`, `lapic.rs`, `msr.rs`, Interrupt-Helfer in `mod.rs`
- `kernel/src/time.rs` – Ticks, `uptime_ms`, `sleep_ms`
- `kernel/linker.ld` – Higher-Half-Layout ab `0xffffffff80000000`
- `scripts/run-qemu.sh` – Cargo-Runner: ISO bauen und QEMU starten
- `vendor/limine` – Limine-Binaries (nicht im Git, `make limine`)
