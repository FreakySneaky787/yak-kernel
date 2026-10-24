# Design decisions

## Basics
| Topic | Decision | Why |
|---|---|---|
| Language | Rust nightly, `#![no_std]`, some inline assembly | memory safety; nightly is needed for `custom_test_frameworks` and `abi_x86_interrupt` |
| Target | `x86_64-unknown-none` (built-in) | bare metal, soft-float (no SSE in the kernel) → no custom target JSON needed |
| Linker | **GNU ld** (`ld.bfd`, binutils 2.46), flavor `ld` | user preference for GNU tools; linker script `kernel/linker.ld` |
| Architecture | x86_64 | matches the host PC, best documentation (OSDev, Intel SDM) |
| Bootloader | Limine v11, boot protocol base revision 6, crate `limine` 0.6.5 | starts directly in long mode with paging, provides memory map, HHDM and framebuffer |
| Boot medium | hybrid ISO (BIOS + UEFI) built with xorriso | boots in QEMU with `-cdrom`, both firmware paths testable |
| External crates | only `limine`, `spin` | everything else is written from scratch |
| Tools | GNU make, gdb, binutils (objdump/nm/readelf/addr2line), gcc for host helpers | user preference: Rust kernel, GNU tooling around it |

## Memory layout
- The kernel lives in the higher half at `0xffffffff80000000` (top 2 GiB), `code-model=kernel`, `relocation-model=static`.
- Limine additionally maps all physical memory at the HHDM offset (`boot::HHDM`):
  physical address `p` is reachable at `p + hhdm_offset`.
- Linker sections: `.limine_requests` (between start and end marker), `.text`, `.rodata`, `.data`/`.bss`,
  each page-aligned with its own PT_LOAD segment.

## Physical memory
- `mem::pmm` is a bitmap allocator: one bit per 4 KiB frame (1 = used), covering physical memory up to the end of the highest usable region.
- The bitmap lives in the first usable region big enough for it and is reserved in itself. Frame 0 is never handed out.
- Only `USABLE` memory is managed for now. `BOOTLOADER_RECLAIMABLE` still holds Limine's stack/page tables/responses.
- All access to physical memory goes through the HHDM (`PhysAddr::to_virt`). Allocator calls run with interrupts disabled.
- `free_frame` panics on double free / invalid addresses – that is always a kernel bug.

## Virtual address space
| Range | PML4 slot | Use |
|---|---|---|
| `0x0000_0000_0000_0000` – `0x0000_7FFF_FFFF_FFFF` | 0–255 | lower half, unused (reserved for user space later) |
| `0xFFFF_8000_0000_0000` + phys | 256 | HHDM (Limine): all RAM + framebuffer, **no MMIO** |
| `0xFFFF_FE00_0000_0000` | 508 | kernel heap (grows upward, max 1 GiB) |
| `0xFFFF_FE80_0000_0000` | 509 | scratch area for paging unit tests |
| `0xFFFF_FF00_0000_0000` | 510 | kernel stacks: `[guard page][stack pages]` per stack, never reused |
| `0xFFFF_FFFF_8000_0000` | 511 | kernel image (linker script) |

- Page tables: we modify Limine's tables (CR3) through the HHDM; intermediate tables come from the frame allocator and are not freed.
- Limine enables EFER.NXE (so `NO_EXECUTE` is usable) and CR0.WP (the kernel cannot write read-only pages either).
- `kmain` switches from Limine's stack to a 64 KiB guarded stack (`mem::stack`) immediately after `kernel::init()`.

## Kernel heap
- `mem::heap::LinkedListHeap`: free blocks form a singly linked list sorted by address; each free block stores `size` + `next` in its first 16 bytes.
- First fit; a block is split into [front padding][allocation][rest], padding and rest go back to the list. Freeing inserts by address and merges neighbours.
- All addresses and sizes are multiples of 16, so leftovers are either 0 or big enough for a header (no lost bytes).
- `KernelHeap` wraps it in a spin lock (interrupts disabled while held) and grows by mapping ≥64 KiB at the heap end when an allocation fails.
- Allocation failure → null → Rust's default alloc error handler panics.

## GDT / TSS
| Selector | Entry |
|---|---|
| 0x00 | null |
| 0x08 | kernel code (64-bit, DPL 0) |
| 0x10 | kernel data (DPL 0), also used for SS/DS/ES/FS/GS |
| 0x18 | TSS (16 bytes, occupies 2 slots) |

IST1 (`gdt::DOUBLE_FAULT_IST`) = 20 KiB static stack for double faults. User segments will be added together with user space.

## Interrupt vectors
| Vector | What |
|---|---|
| 0–31 | CPU exceptions (`arch/x86_64/interrupts.rs`), double fault (8) on IST1 |
| 32 | IRQ0 – PIT timer, 100 Hz |
| 33 | IRQ1 – PS/2 keyboard |
| 39 / 47 | IRQ7 / IRQ15 – spurious IRQ handling |
| 34–47 (rest) | other PIC IRQs, masked |

- Interrupt controller: legacy 8259 PIC. The **local APIC is disabled via MSR** (see BUGS #2) until we switch to the APIC.
- `kernel::init()` enables interrupts with `sti` at the very end. Anything holding a lock that a handler also takes must use `without_interrupts`.
- Handlers use `extern "x86-interrupt"` (nightly `abi_x86_interrupt`): the compiler saves registers and emits `iretq`.
  The IDT structure and all gate descriptors are our own code.
- `interrupts::install_exception_handlers(&mut Idt)` is separate from `init()`; tests build their own IDTs (see `tests/page_fault.rs`).
- Address `0x0000_4000_0000_0000` (lower half) is unmapped under Limine and is used by tests as a "guaranteed to fault" address.

## Keyboard
- The IRQ1 handler only reads port 0x60 and pushes the byte into a `RingBuffer<128>`; translation happens outside the
  interrupt in `drivers::scancode::Decoder`.
- Scancode set 1 (the 8042 controller translates by default). Layout switchable (`keyboard::set_layout`), default **DE**.
  QEMU forwards physical keys, so the kernel layout must match the user's real keyboard.
- `keyboard::inject_scancode()` feeds bytes without hardware (tests, later e.g. serial input).

## Output
- `serial_println!`: COM1 only. `println!`: console **and** COM1. Both disable interrupts while holding their lock (`without_interrupts`).
- Panic handlers only use serial, because the console lock might still be held when the panic happens.
- Console: works on a `Surface` (32 bpp, stride in pixels). Scrolling copies pixel rows inside the framebuffer
  (slow on real hardware, fine in QEMU). Font: Terminus 8×16 (PSF2) via `include_bytes!`; Latin-1 characters through the
  Unicode table, unknown characters become `?`.

## Testing
- Crate layout: `src/lib.rs` contains the whole kernel, `src/main.rs` only `kmain` + panic handler.
  Every test binary (lib unit tests, `tests/*.rs`) defines its own `kmain`; the linker uses `ENTRY(kmain)` for all of them.
- Unit tests: `#[test_case]` inside the module. Integration tests: `kernel/tests/<name>.rs` with their own harness setup (see `tests/boot.rs`).
  Tests that must panic or fault get `harness = false` in `Cargo.toml` (see `tests/should_panic.rs`).
- `scripts/run-qemu.sh` is the cargo runner. A hash suffix in the ELF name means test binary → headless QEMU with a timeout.
- Result via the `isa-debug-exit` device on port 0xF4: value 0x10 → QEMU exit status 33 = PASS, 0x11 → 35 = FAIL.
- All test output goes through COM1 (`serial_println!`).
