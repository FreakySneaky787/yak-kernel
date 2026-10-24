# Fortschritt

Legende: ✅ erledigt · 🔨 in Arbeit · ⬜ offen · **[USER]** = schreibt der User selbst

## Etappe 1: Shell im Kernel
| # | Meilenstein | Status | Test |
|---|---|---|---|
| 0 | Setup: Toolchain, Limine, Git, GitHub | ✅ | – |
| 1 | Limine-Boot, Serial-Ausgabe, Framebuffer-Testbild, Panic-Handler | ✅ | manuell: BIOS + UEFI |
| 2 | Test-Framework + `make test` | ✅ | 7 Tests: lib, boot, should_panic + manuelle Fail-Gegenprobe |
| 3 | Framebuffer-Textkonsole (Bitmap-Font), `println!` | ⬜ | |
| 4 | GDT + TSS (Double-Fault-Stack) | ⬜ | |
| 5 | IDT + Exception-Handler (**[USER]**: einzelne Handler) | ⬜ | |
| 6 | PIC remappen + PIT-Timer | ⬜ | |
| 7 | PS/2-Tastatur (**[USER]**: Scancode-Tabelle) | ⬜ | |
| 8 | Frame-Allocator aus der Limine-Memory-Map | ⬜ | |
| 9 | Paging: map/unmap | ⬜ | |
| 10 | Kernel-Heap → `alloc` | ⬜ | |
| 11 | Shell (**[USER]**: einzelne Befehle) | ⬜ | |

## Log
- **2026-10-24** – Setup: Rust nightly 1.101 (rustup), Target `x86_64-unknown-none`, Limine v11.x-binary.
  M1: Der Kernel bootet über BIOS und UEFI und gibt „Hello from KernelTry!“ über die serielle Schnittstelle aus. Auf dem Framebuffer (1280×800×32) erscheinen Farbbalken.

- **2026-10-24** – M2: Der Kernel ist jetzt Library + Binary. Custom Test Framework in `kernel/src/testing.rs`.
  Integrationstests: `tests/boot.rs` (Limine-Antworten) und `tests/should_panic.rs`. Bug #1 gefunden und behoben (Runner hat den Testmodus nicht erkannt).

## Nächster Schritt
**M3:** Textkonsole auf dem Framebuffer. Eigener 8×16-Bitmap-Font (als Rust-Array), `console.rs` mit Cursor, Zeilenumbruch und Scrolling.
Makros `print!`/`println!` schreiben auf Framebuffer **und** Serial. Tests: viele Zeilen ausgeben (Scrolling) ohne Panic, Cursor-Position prüfen.
Danach fliegt das Farbbalken-Testbild aus `main.rs`.
