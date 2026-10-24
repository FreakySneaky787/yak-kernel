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

- `paging::ensure_hhdm_mapped(phys, size, flags)` maps pages Limine left out of the HHDM (ACPI tables, LAPIC `0xFEE0_0000`,
  I/O APIC `0xFEC0_0000`) at their usual HHDM address, so every physical address is still reached as `phys + hhdm_offset`.
- Page tables: we modify Limine's tables (CR3) through the HHDM; intermediate tables come from the frame allocator and are not freed.
- Limine enables EFER.NXE (so `NO_EXECUTE` is usable) and CR0.WP (the kernel cannot write read-only pages either).
- `kmain` switches from Limine's stack to a 64 KiB guarded stack (`mem::stack`) immediately after `kernel::init()`.

## Threads and scheduling
- `task::Scheduler` (behind a spin lock, only touched with interrupts disabled): `BTreeMap<Tid, Box<Thread>>` (boxed so the saved
  RSP has a stable address), a FIFO ready queue, the current TID and the idle TID. Thread 0 = the boot code ("main"), runs the shell.
- Context switch: `kernel_switch_context(old_rsp_ptr, new_rsp)` pushes rbp/rbx/r12–r15, swaps RSP, pops, `ret`. A new thread's stack
  is prepared so that `ret` lands in `kernel_thread_trampoline`, which calls `thread_start(entry)` with the boxed closure from r12.
- `schedule()` must run with interrupts disabled and drops the lock *before* switching. A resumed thread continues with interrupts off
  and restores them itself (via `without_interrupts`, `iretq` of the interrupted handler, or `sti` in `thread_start`).
- Preemption: every timer tick, if another thread is ready. The timer handler sends EOI first, then calls `task::on_tick`.
- Blocking: `block_current(Wait)` must be called with interrupts disabled after checking the wait condition (no lost wake-ups).
  Wake-ups: timer (`Until`), keyboard/COM1 IRQ (`Input`), `exit` (`Join`).
- Dead threads are reaped (stack unmapped and frames freed) by a later `schedule()` call – never while running on their stack.

## Storage
- PCI: legacy configuration mechanism (ports 0xCF8/0xCFC), brute-force scan of all bus/device/function numbers.
- AHCI (q35's ICH9 controller, BAR5): one driver instance per port, probed **once** at boot into `drivers::DISK`.
  Per port: one frame for command list + received FIS, one for the command table (FIS + 1 PRD), one 4 KiB DMA bounce frame.
  Commands are polled (no interrupts yet) and limited to 8 sectors; larger requests are split.
- `drivers::DISK` is a spin lock held with interrupts enabled during I/O – never use it from interrupt handlers.
- File systems only see the `BlockDevice` trait (512-byte sectors); tests use `RamDisk`.
- Disk images: FAT32 without partition table ("superfloppy"), 64 MiB, 512-byte clusters (FAT32 needs ≥ 65525 clusters),
  created by `scripts/mkdisk.sh` from `rootfs/`. QEMU: `-device ide-hd,bus=ide.0` = AHCI port 0 (the boot CD is ATAPI on port 2).
  Run mode uses the persistent `build/disk.img`; every test binary and the E2E test get fresh images.

## File system
- One FAT32 volume on the system disk, mounted at boot as `fs::ROOT` (a spin lock like `DISK`: never from interrupt handlers).
  `FatFs<D: BlockDevice>` is generic, so the same code runs on `SystemDisk` (adapter for `drivers::DISK`) and on `RamDisk` in tests.
- Mount: FAT32 boot sector at LBA 0 (superfloppy), otherwise the first MBR partition of type 0x0B/0x0C. Only 512-byte sectors.
- No block cache: data clusters are read/written directly. Only one FAT sector is cached; FAT updates are written through to
  **every** FAT copy immediately. Every mutating operation ends with `sync` (FSInfo hints + disk flush), so a `poweroff` or reset
  right after a command never leaves the volume inconsistent. Simple and safe, slow for big files – fine for now.
- Names: a name that fits 8.3 in one case per part (`hello.txt`, `README`) is stored as a plain short entry with the NT
  lower-case flags, like Windows/Linux do. Everything else gets LFN entries plus a unique 8.3 alias (`MYNOTE~1`).
  Lookup is case-insensitive on long and short names.
- Paths at the FS API are absolute and normalized (`/a/b`); the shell owns the current directory and calls `fs::normalize`.
- `rm` refuses non-empty directories; `mv` only rewrites directory entries (and the `..` entry of moved directories).
- Timestamps come from the CMOS RTC (`drivers/rtc.rs`, treated as UTC).
- Validation outside the kernel: after `make test`, `scripts/check-test-disks.sh` runs `fsck.fat -n` on every test disk and
  checks kernel-written files with mtools; the E2E test does the same for its disk after two boots.

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
| 32 | local APIC timer, periodic 100 Hz (drives `time::tick`) |
| 33 | ISA IRQ1 – PS/2 keyboard (I/O APIC GSI 1) |
| 36 | ISA IRQ4 – COM1 receive (I/O APIC GSI 4) |
| 39 / 47 | spurious IRQ 7 / 15 of the masked 8259 (ignored, no EOI) |
| 0xFF | local APIC spurious vector (ignored, no EOI) |

- Interrupt controllers: **local APIC + I/O APIC** (since M12). Information comes from the ACPI MADT (`acpi.rs`).
  ISA IRQ `n` is delivered on vector `32 + n`; the GSI and polarity/trigger come from the MADT interrupt source overrides
  (QEMU: IRQ0 → GSI 2). All handlers acknowledge with `lapic::eoi()`.
- The 8259 PICs are remapped to 32–47 and then fully masked (never deliver anything). PIT channel 0 is not used;
  PIT channel 2 (polled via port 0x61) only serves as the reference clock to calibrate the LAPIC timer at boot.
- LAPIC LINT0 is masked (no ExtINT from the 8259), LINT1 = NMI. Registers are reached through the HHDM after `ensure_hhdm_mapped(.., Flags::MMIO)`.
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

## Input and shell
- Two input sources, one API: `input::read_key()` returns `KeyEvent`s from the PS/2 keyboard (scancode decoder) or from COM1
  (`TerminalDecoder`: UTF-8, CR/LF, DEL, Ctrl+letter, ANSI arrows/Home/End/Delete). It sleeps race-free with `sti; hlt`.
- The line editor only emits a terminal-compatible byte stream (`\x08` = cursor left, characters overwrite), so the same output
  works on the framebuffer console and on a serial terminal. No cursor-addressing escape sequences are needed.
- Commands are a static table (`shell::COMMANDS`) of `fn(&mut Shell, &[String], &mut dyn Write) -> Result<(), String>`;
  unit tests run them against a `String`. Errors are printed in red by the shell, not by the commands.
- `poweroff`: ACPI PM1a control port 0x604 (QEMU q35), fallback isa-debug-exit. `reboot`: 8042 reset line, fallback triple fault.

## Output
- `serial_println!`: COM1 only. `println!`: console **and** COM1. Both disable interrupts while holding their lock (`without_interrupts`).
- The boot binary's panic handler (`kernel::panic_report`) prints to serial and, if `CONSOLE.try_lock()` succeeds, in red on the screen.
- Console: keeps a `Vec<Cell>` character grid (char + colours) for redrawing the cursor cell; cursor = inverted cell.
  ANSI subset: `ESC[2J`, `ESC[H`, `ESC[row;colH`, `ESC[K`, `ESC[..m` (0, 1, 22, 30–37, 39, 40–47, 49, 90–97).
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
- `scripts/e2e-shell.py` (`make e2e`) boots the real kernel with stdin/stdout on COM1, types commands (incl. escape sequences)
  and checks the output between prompts; the final `poweroff` must end QEMU. `make check` = `make test` + `make e2e`.
