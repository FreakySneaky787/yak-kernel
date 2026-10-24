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
| 8 | Physical frame allocator from the Limine memory map | ✅ | 9 tests: fake memory maps, real alloc/free, zeroing |
| 9 | Paging: map/unmap, kernel stack with guard page | ✅ | 8 unit tests + real recursion overflow test |
| 10 | Kernel heap → `alloc` | ✅ | 9 tests: allocator core on a fixed arena + Box/Vec/String/BTreeMap, growth, reuse |
| 11 | Shell | ✅ | 37 unit tests (console, input, parser, line editor, commands) + 19-step E2E over serial |

## Stage 2: multitasking, files, user space (agreed with the user on 2026-10-24)
| # | Milestone | Status | Tests |
|---|---|---|---|
| 12 | APIC: ACPI/MADT, local APIC + timer, I/O APIC, 8259 masked | ✅ | 10 tests + E2E on BIOS and UEFI, timer vs. wall clock checked |
| 13 | Multitasking: kernel threads, context switch, scheduler | ✅ | 7 scheduler tests + shell tests + E2E (`ps`, `demo`), 3× stable |
| 14 | Storage: PCI enumeration, AHCI SATA driver, block device layer | ✅ | 11 tests + E2E (`lspci`, `disk`, `hexdump`) |
| 15 | FAT32 file system (read/write, long names), persistent across reboots | ⬜ | |
| 16 | User space: ring 3, syscalls, ELF loader | ⬜ | |

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
- **2026-10-24** – M8: `mem/` module with `PhysAddr`/`VirtAddr`, HHDM offset, bitmap frame allocator (`mem/pmm.rs`).
  Bitmap logic is independent of Limine and unit-tested with fake memory maps. 253 MiB usable with `-m 256M`.
  Base revision check moved to the very start of `kernel::init()`.

- **2026-10-24** – M9: own 4-level paging code (`mem/paging.rs`: translate incl. huge pages, map/unmap, `map_fresh`, `map_mmio`).
  Kernel stacks with guard pages in PML4 slot 510 (`mem/stack.rs`); `kmain` switches to a 64 KiB guarded stack right after init.
  `tests/stack_overflow.rs` now overflows for real via endless recursion and checks CR2 is inside the guard page.
  Observed: Limine sets EFER.NXE and CR0.WP; its HHDM maps RAM + framebuffer only, no MMIO (LAPIC needs `map_mmio`).

- **2026-10-24** – M10: own linked-list allocator (`mem/heap.rs`: first fit, splitting, address-ordered free list with coalescing)
  as `#[global_allocator]`. Heap in PML4 slot 508, 1 MiB initially, grows on demand up to 1 GiB. `alloc` crate enabled.

- **2026-10-24** – M11: shell. Console got a character grid, inverted cursor, non-destructive `\x08` and an ANSI subset
  (clear, home, erase line, colours). Serial input via IRQ4 + `input::TerminalDecoder` (UTF-8, ANSI arrows, Ctrl keys);
  `input::read_key()` merges keyboard and serial. `shell/`: parser with quotes/escapes, line editor (cursor movement,
  history, tab completion, Ctrl+C/L/U/A/E), 16 commands incl. `mem`, `memmap`, `cpu`, `reboot`, `poweroff`.
  Panics are now also shown on screen. `scripts/e2e-shell.py` drives the real kernel over the serial port (`make e2e`, `make check`).
  **Stage 1 complete: 99 in-kernel tests + 19 E2E steps green.**

- **2026-10-24** – User confirmed stage 2 in the proposed order (M12–M15) and asked to be consulted on open questions.
- **2026-10-24** – M12: ACPI parser (`acpi.rs`: RSDP → XSDT/RSDT → MADT with CPUs, I/O APICs, overrides).
  `paging::ensure_hhdm_mapped` maps ACPI tables / MMIO missing from Limine's HHDM. Local APIC enabled (`lapic.rs`), its timer
  calibrated against PIT channel 2 (polled, no IRQ) and running periodic at 100 Hz on vector 32. I/O APIC (`ioapic.rs`) routes
  keyboard (GSI 1) and COM1 (GSI 4); 8259 remapped + fully masked; PIT channel 0 unused. New shell command `acpi`.
  `make check` now also runs the E2E test under UEFI. Timer accuracy: 6.00 s kernel vs. 6.00 s wall clock (BIOS), 5.99 s (UEFI).

- **2026-10-24** – M13: preemptive round-robin scheduler (`task/`): context switch in `global_asm!` (callee-saved regs + RSP),
  new threads start via a trampoline, each with an 8-page guarded stack that is freed after exit. Blocking waits: `Until(tick)`,
  `Input` (woken by keyboard/COM1 IRQs), `Join(tid)`. Idle thread halts. `sleep_ms`, `input::read_key`, `keyboard::read_key` block
  instead of spinning. Timer handler sends EOI *before* calling `task::on_tick` (which may switch threads).
  Shell: `ps` and `demo [n]` (parallel prime-counting workers).

- **2026-10-24** – User decided: the file system must be a **real persistent disk** (not a RAM fs). Roadmap changed:
  M14 = storage driver, M15 = FAT32, M16 = user space.
- **2026-10-24** – Code base formatted with rustfmt (`kernel/rustfmt.toml`, max_width 120) in a separate commit.
- **2026-10-24** – M14: PCI config space enumeration (`drivers/pci.rs`), AHCI driver (`drivers/ahci.rs`: port init, IDENTIFY,
  READ/WRITE DMA EXT, FLUSH, polled, 4 KiB bounce frame → max 8 sectors per command), `BlockDevice` trait + `RamDisk`
  (`drivers/block.rs`). Global `drivers::DISK` probed once at boot. QEMU gets a SATA disk on AHCI port 0: persistent
  `build/disk.img` in run mode, a fresh image per test binary, a scratch image for E2E. `scripts/mkdisk.sh` builds FAT32 images
  from `rootfs/`. Shell: `lspci`, `disk`, `hexdump`. Bug #3 (double probe) found and fixed.

## Open items / later
- AHCI uses polling; switching to completion interrupts (MSI or the PCI interrupt line) would free the CPU during I/O.
- Thread-level sleeping mutex (today all shared state uses spin locks with interrupts disabled).
- `ps` CPU percentages only cover threads that still exist (dead threads are reaped with their counters).
- Reclaim `BOOTLOADER_RECLAIMABLE` memory once we no longer use Limine's stack, page tables and responses.
- Shell lines longer than one console row are not handled by the line editor (input is capped at 120 characters).
- `gcc` is not installed yet → `make limine-tool` (bios-install for USB/HDD images) is not available.

## Next step
**M15 – FAT32:** parse the BPB (superfloppy, MBR partition later), FAT cluster chains, directories with 8.3 + long file names (LFN),
read files; then writing: allocate clusters, create/extend/truncate files, create directories, delete, update FSInfo.
Shell: `ls`, `cd`, `pwd`, `cat`, `write <file> <text>`, `mkdir`, `rm`, `df`. Tests: against `RamDisk` images and the real disk,
`fsck.fat -n` on the image after the kernel wrote to it (no errors allowed), persistence E2E (write, power off, boot again, read).
Then **M16** (user space).
