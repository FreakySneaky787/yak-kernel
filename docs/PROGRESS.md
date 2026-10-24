# Fortschritt

Legende: ✅ erledigt · 🔨 in Arbeit · ⬜ offen · **[USER]** = schreibt der User selbst

## Etappe 1: Shell im Kernel
| # | Meilenstein | Status | Test |
|---|---|---|---|
| 0 | Setup: Toolchain, Limine, Git, GitHub | ✅ | – |
| 1 | Limine-Boot, Serial-Ausgabe, Framebuffer-Testbild, Panic-Handler | ✅ | manuell: BIOS + UEFI |
| 2 | Test-Framework + `make test` | ✅ | 7 Tests: lib, boot, should_panic + manuelle Fail-Gegenprobe |
| 3 | Framebuffer-Textkonsole (Bitmap-Font), `println!` | ✅ | 9 Unit-Tests (Font, Konsole) + Screenshot |
| 4 | GDT + TSS (Double-Fault-Stack) | ⬜ | |
| 5 | IDT + Exception-Handler | ⬜ | |
| 6 | PIC remappen + PIT-Timer | ⬜ | |
| 7 | PS/2-Tastatur | ⬜ | |
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

## Nächster Schritt
**M4:** GDT mit Kernel-Code- und Kernel-Data-Segment sowie TSS. Der TSS bekommt einen IST-Stack für Double Faults. Segmente neu laden (far return für CS) und `ltr`.
