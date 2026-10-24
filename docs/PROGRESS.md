# Fortschritt

Legende: ✅ erledigt · 🔨 in Arbeit · ⬜ offen · **[USER]** = schreibt der User selbst

## Etappe 1: Shell im Kernel
| # | Meilenstein | Status | Test |
|---|---|---|---|
| 0 | Setup: Toolchain, Limine, Git, GitHub | ✅ | – |
| 1 | Limine-Boot, Serial-Ausgabe, Framebuffer-Testbild, Panic-Handler | ✅ | manuell: BIOS + UEFI |
| 2 | Test-Framework + `make test` | ✅ | 7 Tests: lib, boot, should_panic + manuelle Fail-Gegenprobe |
| 3 | Framebuffer-Textkonsole (Bitmap-Font), `println!` | ✅ | 9 Unit-Tests (Font, Konsole) + Screenshot |
| 4 | GDT + TSS (Double-Fault-Stack) | ✅ | 3 Tests: CS/TR/GDTR, IST-Stack |
| 5 | IDT + Exception-Handler | ✅ | 3 Unit-Tests + Integrationstests `page_fault`, `stack_overflow` |
| 6 | PIC remappen + PIT-Timer | ✅ | 4 Tests: Ticks, sleep_ms, PIC-Maske, APIC aus |
| 7 | PS/2-Tastatur | ✅ | 13 Unit-Tests + E2E per `sendkey` (QWERTZ, Umlaute, AltGr, Caps) |
| 8 | Frame-Allocator aus der Limine-Memory-Map | ⬜ | |
| 9 | Paging: map/unmap | ⬜ | |
| 10 | Kernel-Heap → `alloc` | ⬜ | |
| 11 | Shell (**[USER]**: einzelne Befehle) | ⬜ | |

## Log
- **2026-10-24** – Setup: Rust nightly 1.101 (rustup), Target `x86_64-unknown-none`, Limine v11.x-binary.
  M1: Der Kernel bootet über BIOS und UEFI und gibt „Hello from KernelTry!“ über die serielle Schnittstelle aus. Auf dem Framebuffer (1280×800×32) erscheinen Farbbalken.

- **2026-10-24** – M2: Der Kernel ist jetzt Library + Binary. Custom Test Framework in `kernel/src/testing.rs`.
  Integrationstests: `tests/boot.rs` (Limine-Antworten) und `tests/should_panic.rs`. Bug #1 gefunden und behoben (Runner hat den Testmodus nicht erkannt).

- **2026-10-24** – Der User hat entschieden, dass Claude auch M5 und M7 komplett selbst schreibt (keine [USER]-Teile).
- **2026-10-24** – M3: Eigener PSF2-Parser (`font.rs`, Terminus 8×16, OFL) und Textkonsole (`console.rs`) mit Umbruch, Tab, Backspace und Scrolling.
  `print!`/`println!` schreiben auf Bildschirm und Serial. `without_interrupts` ist schon vorbereitet. Dazu `scripts/screenshot.py` für visuelle Checks.

- **2026-10-24** – M4: Eigene GDT (Null, Kernel-Code 0x08, Kernel-Data 0x10, TSS 0x18) und TSS mit IST1 = 20-KiB-Double-Fault-Stack. CS wird per `retfq` neu geladen.

- **2026-10-24** – M5: Eigene IDT (`idt.rs`), Handler für alle Exceptions 0–21 (`interrupts.rs`), lesbare Ausgabe des Page-Fault-Fehlercodes.
  Double Fault läuft auf IST1, per Test nachgewiesen. **Einschränkung:** Echte Stack Overflows faulten noch nicht, weil der Limine-Stack keine Guard-Page hat. Das kommt mit M9.

- **2026-10-24** – M6: 8259-PIC auf 32–47 umgelegt, PIT auf 100 Hz, `time.rs` (Ticks, `uptime_ms`, `sleep_ms`), Behandlung von Spurious IRQ 7/15.
  Bug #2: Limine maskiert LAPIC LINT0, deshalb kamen keine PIC-Interrupts an. Der Local APIC wird jetzt per MSR abgeschaltet. Läuft unter BIOS und UEFI.

- **2026-10-24** – M7: PS/2-Tastatur an IRQ1. Ringpuffer im Handler, `scancode::Decoder` (Set 1, Shift, Caps, Ctrl, Alt, AltGr, E0/E1).
  Layouts: DE (Default) und US. `keyboard::read_key()` schläft race-frei mit `sti; hlt`. `main.rs` hat bis zur Shell einen Echo-Modus.
  Neu: `scripts/qemu-monitor.py` zum Abfragen des QEMU-Monitors.

## Offene Punkte / später
- Konsole hat noch keinen sichtbaren Cursor (spätestens für die Shell in M11).
- Automatisierter E2E-Tastaturtest: Runner müsste über den Monitor `sendkey` schicken. Sinnvoll mit der Shell (M11).
- Umstieg auf Local APIC + I/O APIC (braucht MMIO-Mapping, also nach M9). Dann `lapic::disable()` wieder entfernen.
- Eigener Kernel-Stack mit Guard-Page (nach M9), danach einen echten Rekursions-Stack-Overflow-Test schreiben.

## Nächster Schritt
**M8:** Physischer Frame-Allocator. Usable-Bereiche aus der Limine-Memory-Map lesen, Bitmap-Allocator für 4-KiB-Frames
(die Bitmap selbst liegt in einem Usable-Bereich, Zugriff über die HHDM). API: `alloc_frame()`, `free_frame()`, Statistik (frei/gesamt).
Tests: Alloc/Free, keine doppelte Vergabe, Ausrichtung, Statistik stimmt.
