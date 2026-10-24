# Fortschritt

Legende: ✅ erledigt · 🔨 in Arbeit · ⬜ offen · **[USER]** = schreibt der User selbst

## Etappe 1: Shell im Kernel
| # | Meilenstein | Status | Test |
|---|---|---|---|
| 0 | Setup: Toolchain, Limine, Git, GitHub | ✅ | – |
| 1 | Limine-Boot, Serial-Ausgabe, Framebuffer-Testbild, Panic-Handler | ✅ | manuell: BIOS + UEFI |
| 2 | Test-Framework + `make test` | ⬜ | Pass- und Should-Panic-Test |
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

## Nächster Schritt
**M2:** Custom Test Framework (`#![feature(custom_test_frameworks)]`). `cargo test` startet den Kernel über `scripts/run-qemu.sh` im Testmodus,
die Tests melden ihr Ergebnis über `qemu::exit`. Dazu kommt ein separater Integrationstest für `should_panic`.
