# Progress

Legend: ✅ done · 🔨 in progress · ⬜ open

## Stage 1: shell inside the kernel
| # | Milestone | Status | Tests |
|---|---|---|---|
| 0 | Setup: toolchain, Limine, git, GitHub | ✅ | – |
| 1 | Limine boot, serial output, framebuffer test pattern, panic handler | ✅ | manual: BIOS + UEFI |
| 2 | Test framework + `make test` | ✅ | 7 tests: lib, boot, should_panic + manual failing-test check |
| 3 | Framebuffer text console (bitmap font), `println!` | ✅ | 9 unit tests (font, console) + screenshot |
| 4 | GDT + TSS (double fault stack) | ✅ | 3 tests: CS/TR/GDTR, IST stack |
| 5 | IDT + exception handlers | ✅ | 3 unit tests + integration tests `page_fault`, `stack_overflow` |
| 6 | PIC remap + PIT timer | ✅ | 4 tests: ticks, sleep_ms, PIC mask, APIC off |
| 7 | PS/2 keyboard | ✅ | 13 unit tests + E2E via `sendkey` (QWERTZ, umlauts, AltGr, Caps) |
| 8 | Physical frame allocator from the Limine memory map | ⬜ | |
| 9 | Paging: map/unmap, kernel stack with guard page | ⬜ | |
| 10 | Kernel heap → `alloc` | ⬜ | |
| 11 | Shell | ⬜ | |

## Log
- **2026-10-24** – Setup: Rust nightly 1.101 (rustup), target `x86_64-unknown-none`, Limine v11.x-binary.
  M1: kernel boots via BIOS and UEFI, prints "Hello from KernelTry!" on serial, colour bars on the framebuffer (1280×800×32).
- **2026-10-24** – M2: kernel split into library + binary. Custom test framework in `kernel/src/testing.rs`.
  Integration tests `tests/boot.rs` (Limine responses) and `tests/should_panic.rs`. Bug #1 found and fixed (runner did not detect test mode).
- **2026-10-24** – The user decided that Claude implements everything itself (no user exercises).
- **2026-10-24** – M3: own PSF2 parser (`font.rs`, Terminus 8×16, OFL) and text console (`console.rs`) with wrapping, tab, backspace, scrolling.
  `print!`/`println!` write to screen and serial. `without_interrupts` prepared. Added `scripts/screenshot.py` for visual checks.
- **2026-10-24** – M4: own GDT (null, kernel code 0x08, kernel data 0x10, TSS 0x18) and TSS with IST1 = 20 KiB double fault stack. CS reloaded via `retfq`.
- **2026-10-24** – M5: own IDT (`idt.rs`), handlers for all exceptions 0–21 (`interrupts.rs`), readable page fault error codes.
  Double fault runs on IST1 (verified by a test). **Limitation:** real stack overflows do not fault yet because the Limine stack has no guard page (fixed with M9).
- **2026-10-24** – M6: 8259 PIC remapped to 32–47, PIT at 100 Hz, `time.rs` (ticks, `uptime_ms`, `sleep_ms`), spurious IRQ 7/15 handling.
  Bug #2: Limine masks LAPIC LINT0, so no PIC interrupt arrived → local APIC is now disabled via MSR. Works with BIOS and UEFI.
- **2026-10-24** – M7: PS/2 keyboard on IRQ1. Ring buffer in the handler, `scancode::Decoder` (set 1, Shift, Caps, Ctrl, Alt, AltGr, E0/E1).
  Layouts DE (default) and US. `keyboard::read_key()` sleeps race-free with `sti; hlt`. `main.rs` has an echo mode until the shell exists.
  New: `scripts/qemu-monitor.py` for QEMU monitor queries.
- **2026-10-24** – Repository translated to English (user request). Kernel now linked with **GNU ld** (`ld.bfd`) instead of rust-lld;
  added binutils make targets (`disasm`, `symbols`, `sections`, `addr2line`).

## Open items / later
- Switch to local APIC + I/O APIC (needs MMIO mapping, i.e. after M9). Then remove `lapic::disable()`.
- Own kernel stack with a guard page (M9), then a real recursion stack overflow test.
- The console has no visible cursor yet (needed for the shell in M11).
- Automated E2E keyboard test: the runner would have to send `sendkey` through the monitor. Makes sense together with the shell (M11).

## Next step
**M8:** physical frame allocator. Read the usable regions from the Limine memory map, bitmap allocator for 4 KiB frames
(the bitmap itself lives in a usable region, accessed through the HHDM). API: `alloc_frame()`, `free_frame()`, statistics (free/total).
Tests: alloc/free, no double allocation, alignment, statistics add up.
