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
| `0x0000_0000_0000_0000` – `0x0000_7FFF_FFFF_FFFF` | 0–255 | user space: private per process (see "User space") |
| `0xFFFF_8000_0000_0000` + phys | 256 | HHDM (Limine): all RAM + framebuffer, **no MMIO** |
| `0xFFFF_FE00_0000_0000` | 508 | kernel heap (grows upward, max 1 GiB) |
| `0xFFFF_FE80_0000_0000` | 509 | scratch area for paging unit tests |
| `0xFFFF_FF00_0000_0000` | 510 | kernel stacks: `[guard page][stack pages]` per stack, never reused |
| `0xFFFF_FFFF_8000_0000` | 511 | kernel image (linker script) |

- `paging::ensure_hhdm_mapped(phys, size, flags)` maps pages Limine left out of the HHDM (ACPI tables, LAPIC `0xFEE0_0000`,
  I/O APIC `0xFEC0_0000`) at their usual HHDM address, so every physical address is still reached as `phys + hhdm_offset`.
- Page tables: we modify Limine's tables through the HHDM; intermediate tables come from the frame allocator and are not freed.
- `paging::init` remembers Limine's PML4 as `kernel_root()` and creates **all 256 upper-half PML4 entries** up front (empty PDPTs).
  Every process PML4 copies these entries, so later kernel mappings (heap growth, new stacks, MMIO) land in shared lower-level
  tables and are visible in every address space. The PML4 entries themselves never change after boot.
- Limine sets the USER bit on its upper-half intermediate tables (leaves are supervisor-only). `paging::init` clears USER in all
  upper-half PML4 entries, so ring 3 cannot reach the kernel half, whatever the lower levels say.
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

## User space
- **Address space** (`mem::address_space::AddressSpace`): own PML4, user half empty, kernel half shared. Frames are filled via the
  HHDM, so loading never switches CR3. `Drop` frees every user frame and page table (`paging::free_user_half`); all user
  mappings own their frames. It must not be the active address space when dropped.
- **User layout**: ELF segments between `USER_IMAGE_START` (64 KiB, null page area stays unmapped) and `USER_IMAGE_END`;
  our programs link at `0x40_0000`. The heap starts at the first page after the image and is moved with `brk` (max 64 MiB). Stack: 16 pages below `USER_STACK_TOP = 0x7FFF_FFFF_0000`, nothing mapped below it
  (overflow → page fault → process killed). The argument block is copied to the stack top; `_start` gets pointer and length
  in RDI/RSI. Arguments inside the block are separated by NUL bytes (`ulib::Args`); the shell joins its words that way.
- **ELF loader** (`process::elf`): static `ET_EXEC` x86_64 only, `PT_LOAD` segments with R/W/X → USER, WRITABLE, NO_EXECUTE.
  Every header field is bounds-checked; segments outside the user image area or an entry point outside an X segment are refused.
- **Processes** (`process::PROCESSES`, interrupts off while locked): one kernel thread per process. `task::spawn_user` creates a
  thread with its own CR3 and a `UserEntry`; `thread_start` drops the closure and then `iretq`s to ring 3 (`enter_user`).
  The thread's kernel stack is abandoned at that point and reused from its top by every interrupt/syscall from ring 3.
- **Scheduler**: each thread has `cr3`; on a switch the scheduler loads it (if different) and sets TSS.RSP0 + the syscall stack
  to the next thread's kernel stack top (`syscall::set_kernel_stack`). Switching CR3 on the old thread's stack is fine: kernel
  stacks are in the shared half.
- **End of a process**: `exit` syscall, CPU exception with CS.RPL = 3 (handlers call `process::kill_current` instead of panicking),
  or `process::kill(pid)` (checked when the timer interrupts user code and on every syscall return). `exit_current` records the
  status, moves the thread to the kernel CR3, frees the address space and exits the thread. The status stays in the table until
  `process::wait` (zombie); the shell always waits.
- **System calls** (`syscall`/`sysret`): STAR = kernel CS 0x08 / sysret base 0x10, LSTAR = `syscall_entry`, FMASK clears
  IF/TF/DF/AC. The stub swaps to the kernel stack via a static (single CPU, interrupts off until it is pushed), saves a
  `SyscallFrame`, enables interrupts and calls `process::syscall::dispatch`. Convention like Linux: number in RAX, args in
  RDI/RSI/RDX/R10/R8/R9, result in RAX, negative = `-errno`; only RCX/R11 are clobbered. Syscalls may block (read, sleep).
  | # | Name | |
  |---|---|---|
  | 0 | `exit(code)` | never returns |
  | 1 | `write(fd, buf, len)` | fd 1/2 → console (or a capture buffer in tests) |
  | 2 | `read(fd, buf, len)` | fd 0 → keyboard/serial keys as terminal bytes (`\n`, `0x08`, UTF-8), blocks for ≥ 1 byte |
  | 3 | `sleep_ms(ms)` | |
  | 4 | `getpid()` | |
  | 5 | `uptime_ms()` | |
  | 6 | `yield()` | |
  | 7 | `open(path, len, flags)` | flags READ=1, WRITE=2, CREATE=4, TRUNCATE=8, APPEND=16; a directory opens for `readdir` |
  | 8 | `close(fd)` | |
  | 9 | `readdir(fd, *DirEntryInfo)` | 1 = entry written, 0 = end; 296-byte `repr(C)` struct shared with `ulib` |
  | 10 | `stat(path, len, *Stat)` | 32-byte `repr(C)`: size, kind (1 file, 2 dir), modified time |
  | 11 | `getcwd(buf, len)` | `-ERANGE` if too small |
  | 12 | `unlink(path, len)` | files and empty directories |
  | 13 | `mkdir(path, len)` | |
  | 14 | `brk(new_end)` | 0 = query; maps zeroed RW/NX pages when growing, frees them when shrinking; `-ENOMEM` outside the range |
  | 15 | `spawn(path, len, args, args_len)` | 4th argument in R10; child inherits cwd and output; `-ENOENT`, `-ENOEXEC`, `-E2BIG` |
  | 16 | `wait(pid, *WaitStatus)` | children only (`-ECHILD`); 128-byte status: kind (1 exited, 2 killed), code, kill reason |
  | 17 | `kill(pid)` | children only (`-ESRCH`) |
  | 18 | `chdir(path, len)` | `-ENOTDIR` for files |
  | 19 | `kshell(line, len)` | transitional: runs a kernel shell built-in (not cd/pwd/history/run) in the caller's cwd and output |
  | 20 | `kcommands(buf, len)` | NUL-separated names usable with `kshell` |
  | 21 | `pipe(*[u32; 2])` | read end, write end |
  | 22 | `dup(fd)` | lowest free fd for the same object |
  | 23 | `dup2(fd, new_fd)` | closes what was at `new_fd` |
  | 24 | `isatty(fd)` | 1 for the console (stdin, stdout to the console), else 0 |
  | 25 | `sysinfo(*SysInfo)` | 176 bytes: uptime, ticks, memory, kernel heap, FS size/free/label, CPUs, CPU vendor/brand, date |
  | 26 | `proclist(*[ProcInfo], max)` | one 64-byte entry per thread (tid, cpu ticks, parent, state, user?, name) |
  | 27 | `rename(from, len, to, len)` | |
  | 28 | `seek(fd, offset, whence)` | SET/CUR/END for files, `-ESPIPE` otherwise; past the end is allowed (gap = zeros on write) |
- **Pipes** (`process::pipe`): bounded queue (64 KiB) + reader/writer counts. A descriptor holds a `PipeEnd`; `Clone`
  (dup, dup2, spawn) increments, `Drop` (close, process exit) decrements and notifies. Read blocks while empty with writers,
  returns 0 without; write blocks while full with readers, `EPIPE` without. Waiters sleep on `Wait::Pipe(id)`; the pipe lock is
  only taken with interrupts disabled (descriptors are dropped under the process table lock). Kills abort the wait (`EINTR`).
- **Descriptor inheritance**: `spawn` clones the parent's fds 0–2 into the child (other fds are not inherited). A shell
  redirects by `dup2`-ing a pipe end or file over 0/1 before `spawn` and restoring its saved copies afterwards.
- **Process tree**: `parent` (None = started by the kernel; only the kernel waits for those). When a process ends, finished
  children are dropped and running ones become orphans, removed from the table at their own exit (nobody waits for them).
- **Kill / Ctrl+C**: `kill` stores a reason and `task::wake`s the target; blocking syscalls use abortable waits
  (`input::read_key_or_abort`, `task::join_or_abort`, sleep returns early) and the kill takes effect when the syscall returns or
  the timer interrupts user code. `FOREGROUND` = pid owning the console: set by the kernel shell for its programs, inherited by
  `process::wait_child` while the foreground process waits for that child (handed back when the wait ends); `spawn` alone never
  changes it (bug #4). The keyboard and COM1 interrupt handlers recognise
  Ctrl+C themselves (raw 0x03 / Ctrl tracker on set-1 scancodes, the C key is 0x2E on QWERTY and QWERTZ) and call
  `process::interrupt_foreground`: the key is swallowed and the process killed ("interrupted"), unless it has
  `ignore_interrupt` – then the key reaches it as byte 0x03.
- **init**: `kernel_main` runs `process::run_init("/bin/sh")` (foreground, `ignore_interrupt`) and waits; when it is missing or
  exits, the kernel shell runs on thread 0 as before.
- **`/bin/sh`** (`user/src/bin/sh/`): compiles the kernel's `shell/line_editor.rs` and `shell/parser.rs` with `#[path]`; both
  sides provide a `keys` module (`Key`, `KeyEvent`) next to them. Pipelines: for each stage, stdin = previous pipe (or `<`
  file), stdout = new pipe (or `>`/`>>` file), then the stage runs (built-in inline, program spawned); all children are waited
  for in order, only the last exit code is reported, kills always; an "interrupted" stage kills the rest. `read(0)` delivers keys as terminal bytes (VT100 sequences
  for arrows, Home/End, Delete, PageUp/Down, Insert); `keys::Decoder` turns them back into `KeyEvent`s. Order of lookup:
  sh built-ins, kernel built-ins (`kshell`), program path / `/bin/<name>`.
- **Files**: each process has a cwd (inherited from the spawner, paths are normalized against it) and up to 32 file descriptors
  (`process::Handle`): 0 stdin, 1/2 the process's `Output`, then files (path + one offset for reads and writes; writes use
  `FatFs::write_at`, or `append_file` with the APPEND flag)
  and directories (snapshot of the entries at `open`). Handlers copy what they need out of the table, do the disk I/O
  without holding `PROCESSES` (only `fs::ROOT`), then update the descriptor. `read` of a file uses `FatFs::read_at`, ≤ 64 KiB per call.
  FS errors map to Linux errno values (`ENOENT`, `EISDIR`, ...).
- **User pointers**: every buffer is checked page by page against the process's page tables (present + USER, + WRITABLE for
  `read`) before the kernel touches it; then it is accessed directly (the process CR3 is active). No SMAP/SMEP yet.
- **User programs** (`user/`, crate `ulib` + `src/bin/*`): built with the same target as the kernel but `code-model=small`,
  linked by GNU ld at `0x40_0000` (`user/linker.ld`). `make user` copies them to `build/user/bin`; `scripts/mkdisk.sh`
  and (for the persistent disk) `run-qemu.sh` install them into `/bin` via `scripts/install-user.sh`.
- **User heap** (`ulib::heap`): first-fit allocator, free list sorted by address with coalescing, 16-byte units, grows the
  break by ≥ 64 KiB at a time. No lock (programs are single-threaded). `sys_brk` takes the `AddressSpace` out of the process
  table while it maps/unmaps, so zeroing megabytes does not run with the table lock held (interrupts off).
- **Shell**: a word that is not a built-in is a program path or a name in `/bin`; `run <path> [args]` does the same explicitly.
  Programs run in the shell's cwd. `ls`, `cat` and `cp` are programs (no built-ins any more). Non-zero exit codes and kills
  are reported as errors. `Shell::capture_programs` makes program output go to the command's `out` (unit tests).
- Limitations: no FPU/SSE state is saved (kernel and our programs are built soft-float); no seek;
  no Ctrl+C for a running program (`kill` exists in the kernel API only); a program blocked in `read` is only killed after its next key.

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
| 0x18 | user data (DPL 3), selector `0x1B` with RPL 3 |
| 0x20 | user code (64-bit, DPL 3), selector `0x23` |
| 0x28 | TSS (16 bytes, occupies 2 slots) |

The user data/code order is dictated by `sysret` (SS = STAR base + 8, CS = base + 16). IST1 (`gdt::DOUBLE_FAULT_IST`) =
20 KiB static stack for double faults. RSP0 = kernel stack of the running thread, rewritten on every context switch.

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
