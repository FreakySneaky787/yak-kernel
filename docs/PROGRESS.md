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
| 15 | FAT32 file system (read/write, long names), persistent across reboots | ✅ | 30 tests + `fsck.fat`/mtools check of all test disks + 45-step E2E incl. reboot (BIOS + UEFI) |
| 16 | User space: ring 3, syscalls, ELF loader | ✅ | 26 new unit tests (hand-assembled programs) + integration test `userspace` (real programs from disk) + 7 E2E steps |

## Stage 3: a real user space (agreed with the user on 2026-10-24)
| # | Milestone | Status | Tests |
|---|---|---|---|
| 17 | Files for programs: fd table, `open`/`close`/`readdir`/`stat`/`getcwd`/`unlink`/`mkdir`, `cat`/`ls`/`cp` as programs | ✅ | `read_at` unit test, `fstest` (25 syscall checks incl. errors), integration test, mtools check of a `cp` copy, 6 E2E steps |
| 18 | User heap: `brk`-style syscall + allocator in `ulib` (`alloc` in programs) | ✅ | 4 unit tests (grow/shrink, limits, release, rollback), `heaptest` (11 checks), `sort` in integration test, 6 E2E steps |
| 19 | Processes from processes: `spawn`/`wait`/`kill` syscalls, Ctrl+C | ✅ | Ctrl+C unit tests (foreground, ignore, raw scancode tracker), `spawntest` (17 checks incl. orphans), 6 E2E steps (Ctrl+C over serial), PS/2 Ctrl+C checked via `sendkey` |
| 20 | Shell as a user program (`/bin/sh` started as `init`, kernel shell as fallback) | ✅ | `builtins_for_user_space`, scripted `/bin/sh` session in the integration test (injected serial input), whole E2E suite (72 steps) now against `/bin/sh` + `exit` fallback |

## Stage 4: a usable system (agreed with the user on 2026-10-24)
| # | Milestone | Status | Tests |
|---|---|---|---|
| 21 | Pipes and redirection (`a \| b`, `>`, `>>`, `<`) | ✅ | 4 pipe unit tests (EOF, EPIPE, full pipe with a writer thread, abort), `pipetest` (17 checks incl. 200 KB from a child), scripted `sh` pipeline session, 8 E2E steps |
| 22 | System information as syscalls (replace `kshell`), `ps`/`mem`/... as programs, `rename` + seek | ✅ | `write_at` unit test, `fstest` +17 checks (seek, gaps, APPEND, rename, ESPIPE), integration test for all 10 programs, 5 E2E steps |
| 23 | Full-screen text editor as a user program | ✅ | `edit --selftest` (19 buffer checks), console SGR 7 test, 3 scripted editor sessions in the integration test, 5 E2E steps, screenshot |

## Stage 5: hardware (agreed with the user on 2026-10-24)
| # | Milestone | Status | Tests |
|---|---|---|---|
| 24 | FPU/SSE state per thread; user programs built with SSE2 hardware floating point | ✅ | 3 FPU unit tests (CR0/CR4, initial state, save/restore), `fputest` (4 preempted workers checking XMM0–15 + MXCSR, pi with f64) in integration + E2E; verified that it fails without the switch |
| 25 | SMP part 1: start the other cores ourselves (trampoline, INIT/SIPI), per-CPU GDT/TSS/LAPIC, jobs on APs | ✅ | 4 SMP unit tests (all MADT CPUs online, jobs run on the right CPU with its own TSS, FPU + heap on APs, trampoline page), E2E `cpu` shows 4 CPUs, BIOS + UEFI, 3 stable runs |
| 26 | SMP part 2: scheduler on all cores (per-CPU current/idle thread, lock held across switches, no lost wake-ups, `swapgs` in the syscall stub) | ✅ | new test: spinning threads seen on ≥ 2 CPUs; whole suite green with `KERNEL_SMP=1`, `2` and `4`; `demo 4`: 760 ms on 1 core → 200 ms on 4 |
| 27 | Network: e1000 driver, Ethernet/ARP/IPv4/ICMP/UDP, `ping`, `netinfo`, `udpecho` | ✅ | 4 packet unit tests (checksum, IPv4/ARP/ICMP/UDP round trips), 4 network tests (config, ARP + ping to the gateway, UDP ports, receive path), integration test (ping/netinfo/unreachable), E2E: ping + UDP echo through a host server |

## Stage 6: a complete small system (agreed with the user on 2026-10-24: "all of it")
| # | Milestone | Status | Tests |
|---|---|---|---|
| 28 | Microsecond clock from the TSC, `time` program, ping in µs | ✅ | 2 TSC unit tests (calibration, agreement with the tick clock), `time hello` in integration test + E2E |
| 29 | TCP: connect/listen/accept, retransmission, flow control; client and server programs | ⬜ | |
| 30 | DHCP and a DNS client; HTTP client (`wget`) and server (`httpd`) | ⬜ | |
| 31 | Shell: scripts, environment variables, `2>`, background jobs (`&`, `jobs`, `wait`) | ⬜ | |
| 32 | Storage speed: sector cache for FAT32, AHCI completion interrupts (MSI) | ⬜ | |
| 33 | Memory: reclaim bootloader memory, demand paging for user heaps | ⬜ | |

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

- **2026-10-24** – M15: FAT32 (`fs/fat32.rs`): mount (superfloppy or first FAT32 MBR partition), FAT cluster chains with a
  one-sector FAT cache written through to all FAT copies, directories with 8.3 + long file names (LFN, UTF-16, checksum),
  read/write/append/truncate, mkdir, rm (empty dirs only), rename/move, FSInfo free-count hints, `format` for RAM-disk tests.
  Timestamps from the new CMOS clock driver (`drivers/rtc.rs`). `fs::ROOT` is mounted at boot. Shell: current directory in the
  prompt (`kernel:/path>`), `ls cd pwd cat write append touch mkdir rm mv stat df date`. `make test` now validates every test
  disk with `fsck.fat -n` and mtools (`scripts/check-test-disks.sh`); the E2E test boots twice on the same disk to prove persistence.
  Session was interrupted once before the commit; `make check` re-run green on resume. The `poweroff` exit-status-33 observation
  from before the cut did not reproduce (4/4 sessions exited with 0).

- **2026-10-24** – M16: user space. GDT gets user data/code (DPL 3, order fixed by `sysret`), TSS moves to 0x28, RSP0 is set
  on every context switch. `syscall`/`sysret` via STAR/LSTAR/FMASK with an assembly stub (`arch/x86_64/syscall.rs`).
  Per-process address spaces (`mem/address_space.rs`): own PML4, kernel half shared – `paging::init` pre-creates all
  upper-half PML4 entries so they never change. ELF64 loader (`process/elf.rs`), process table with spawn/wait/kill and
  exit statuses (`process/mod.rs`), 7 syscalls (`process/syscall.rs`: exit, write, read, sleep_ms, getpid, uptime_ms, yield)
  with page-table checks of every user buffer. Exceptions from ring 3 kill only the process. User programs in Rust in `user/`
  (runtime `ulib` + `hello`, `upper`, `fault`), linked at 0x40_0000 with GNU ld, installed into `/bin` on every disk.
  Shell: `run <path>`, or just the program name for `/bin` (also in tab completion).
  Found: Limine sets USER on its upper-half intermediate page tables – cleared in `paging::init` (BUGS.md pitfalls).
  **Stage 2 complete: 156 in-kernel tests + 6 test kernels + 52 E2E steps green (BIOS and UEFI).**

- **2026-10-24** – User agreed to stage 3 (M17–M19) as proposed.
- **2026-10-24** – M17: per-process file descriptor table (`process::Handle`: stdin, stdout, file with offset, directory snapshot;
  max 32) and current directory (inherited from the shell). New syscalls 7–13: `open` (READ/WRITE/CREATE/TRUNCATE, writes append),
  `close`, `readdir`, `stat`, `getcwd`, `unlink`, `mkdir`; `read`/`write` work on files. `FatFs::read_at` reads at an offset.
  The process table lock is never held during disk I/O. Argument block is now NUL-separated (`ulib::Args`), so quoted
  arguments with spaces survive. `ulib::fs` (`File`, `Dir`, `stat`, `cwd`); new programs `cat`, `ls`, `cp`, `fstest`.
  The kernel shell's built-in `ls`/`cat` are gone – `/bin/ls` and `/bin/cat` replace them. Shell tests capture program output.

- **2026-10-24** – M18: user heap. Syscall 14 `brk(new_end)`: heap right after the ELF image, max 64 MiB, pages mapped zeroed
  when growing and freed when shrinking (address space borrowed out of the process table so the lock is not held while zeroing).
  `AddressSpace::map_fresh` now rolls back on failure, new `AddressSpace::unmap`. `ulib::heap`: first-fit allocator with an
  address-ordered free list and coalescing as `#[global_allocator]` – programs can use `Vec`, `String`, `BTreeMap`, `format!`.
  New programs `sort` (files or typed lines, `-r`) and `heaptest`.

- **2026-10-24** – M19 was split: M19 = process syscalls + Ctrl+C, M20 = the shell in user space.
- **2026-10-24** – M19: syscalls 15–17 `spawn(path, args)` (child inherits cwd + output), `wait(pid, *WaitStatus)` (children only,
  128-byte status with kill reason), `kill(pid)` (children only). Parents, orphans (removed from the table when they end) and a
  foreground process (inherited by children of the foreground process, given back to the parent at exit). Ctrl+C is detected in
  the interrupt handlers – serial byte 0x03, PS/2 via a tiny Ctrl tracker on the raw scancodes – and kills the foreground process
  unless it ignores interrupts (then the key is delivered as 0x03). `task::wake` + interruptible `read`/`wait`
  (`input::read_key_or_abort`, `task::join_or_abort`) so a blocked process dies at once. `process::Options` / `spawn_with`.
  `ulib::process` (`spawn`, `wait`, `kill`, `run`), new program `spawntest`, `fault spin|sleep`.
  Found: empty user buffers come with dangling pointers (0x1) – zero-length ranges are now always accessible.

- **2026-10-24** – M20: `/bin/sh` in user space (`user/src/bin/sh/`). It compiles the kernel shell's `line_editor.rs` and
  `parser.rs` via `#[path]` (shared code, `shell/keys.rs` provides the key types on both sides) and decodes the terminal bytes
  of `read` (which now delivers arrows/Home/End/Delete as VT100 sequences) with its own `keys::Decoder`. Built-ins cd (new
  syscall 18 `chdir`), pwd, echo, run, history, help, exit; kernel built-ins through the transitional syscalls 19 `kshell` /
  20 `kcommands`; programs via `spawn`/`wait`. The kernel starts `/bin/sh` as init (`process::run_init`: foreground, ignores
  Ctrl+C) and falls back to the kernel shell when it is missing or exits. Bug #4 (foreground stolen by an orphan) found by the
  E2E test and fixed: the console is lent to a child only while its parent waits for it.
  **Stage 3 complete: 163 in-kernel tests + 6 test kernels + 72 E2E steps green (BIOS and UEFI), init is /bin/sh.**

- **2026-10-24** – User agreed to stage 4 (M21–M23) as proposed.
- **2026-10-24** – M21: pipes (`process/pipe.rs`: 64 KiB queue, reader/writer counts kept by cloning/dropping `PipeEnd`s,
  blocking with `Wait::Pipe(id)` + `task::notify`, EOF and EPIPE). Syscalls 21–24 `pipe`, `dup`, `dup2`, `isatty`; children
  inherit fds 0–2 at `spawn`; `kshell` output follows fd 1. `/bin/sh`: `a | b | c`, `>`, `>>`, `<` via `dup2` around each stage,
  errors to stderr, Ctrl+C in a pipeline kills the rest of it. `ulib::io::read_line` returns `None` at EOF / Ctrl+D and only
  echoes on the console; `read_all`, `read_stdin`, `read_input`. `cat`/`sort` read stdin, new `wc`, `grep`, `pipetest`.

- **2026-10-24** – M22: syscalls 25–28 `sysinfo` (176-byte struct: uptime, memory, kernel heap, file system, CPU count/vendor/
  brand, CMOS date), `proclist` (64-byte entries: tid, parent, state, user/kernel, CPU ticks, name), `rename`, `seek`.
  File descriptors now have one position for reads *and* writes (`FatFs::write_at`: overwrite, extend, zero-filled gaps);
  new `open` flag APPEND (used by `>>`). New programs `ps`, `mem`, `uptime`, `df`, `date`, `mkdir`, `rm`, `touch`, `stat`, `mv`;
  these names are no longer offered through `kshell` (the kernel shell keeps its built-ins as fallback).

- **2026-10-24** – M23: `/bin/edit` (`user/src/bin/edit/`): `buffer.rs` (lines of chars, cursor with remembered column,
  insert/newline/backspace/delete joining lines; checked by `edit --selftest`) and a VT100 screen with horizontal and vertical
  scrolling, inverted status bar, partial redraw of the cursor line. ^S saves, ^Q quits (twice with unsaved changes).
  Kernel: syscalls 29 `termsize` and 30 `ignore_interrupt` (the editor takes Ctrl+C itself), console SGR 7/27 (inverse video).
  **Stage 4 complete: 169 in-kernel tests + 6 test kernels + 90 E2E steps green (BIOS and UEFI), 26 programs in /bin.**

- **2026-10-24** – User agreed to stage 5 (M24–M26) and installed `gcc` (needed as host linker for `-Zbuild-std`).
- **2026-10-24** – M24: `arch/x86_64/fpu.rs` enables x87/SSE (CR0.EM/TS off, MP/NE on, CR4.OSFXSR/OSXMMEXCPT) and records a
  clean initial FXSAVE image. Every thread owns a 512-byte `FxState`; `schedule()` does `fxsave64` for the outgoing and
  `fxrstor64` for the incoming thread (eager). The kernel stays soft-float. User programs now use their own target
  `user/x86_64-kerneltry-user.json` (SSE2, hard-float ABI, static, small code model, red zone allowed) with
  `-Zbuild-std=core,alloc`. New program `fputest`. Check: with the save/restore removed, all 4 workers report corrupted XMM.

- **2026-10-24** – User chose our own INIT/SIPI start (not Limine's MP feature). M25 was split: M25 = starting the cores,
  M26 = scheduling on them, M27 = network.
- **2026-10-24** – M25: `arch/x86_64/smp.rs`. `pmm` reserves one frame below 1 MiB before anything else; the trampoline
  (`global_asm!`, 16 → 32 → 64 bit, data accesses hand-encoded because the assembler rejects label differences in memory
  operands) is copied there, patched with its physical addresses, the kernel PML4 and a per-AP stack, and temporarily
  identity-mapped. INIT, 10 ms, SIPI (twice if needed) per AP via new LAPIC IPI functions. `ap_main`: own GDT/TSS (now arrays
  per CPU) with its own double fault stack, shared IDT, FPU/SSE, syscall MSRs, local APIC, then `hlt` until a `WAKE_VECTOR`
  IPI brings a job (`smp::run_on`). QEMU runs with `-smp 4`; `cpu` lists the online CPUs.

- **2026-10-24** – M26: the scheduler runs on every core. Per CPU: current thread, idle thread (APs turn their start-up context
  into it), LAPIC timer (calibrated once on the BSP; only CPU 0 advances `time::ticks`). Shared ready queue; `schedule()` keeps
  the scheduler lock across the context switch and the next thread releases it (`finish_switch`), so a thread that is still
  being switched out (`on_cpu`) can never be picked by another CPU. `block_current` replaced by `wait_for(reason, ready)`:
  mark blocked first, then check – no lost wake-ups across CPUs; wakers on a thread still `on_cpu` just set it running.
  Idle CPUs get a wake-up IPI when a thread becomes ready. Syscall stub: per-CPU slots through `swapgs`/IA32_KERNEL_GS_BASE,
  swapped back before interrupts are enabled. `ps` shows the CPU a thread runs on. `KERNEL_SMP=<n>` selects the core count.

- **2026-10-24** – M27: `drivers/e1000.rs` (82540EM: reset, MAC from RAL/EEPROM, 32 RX / 8 TX descriptors with 2 KiB DMA
  buffers, polled) and `net/` (`packet.rs`: Ethernet II, ARP, IPv4 with checksum, ICMP echo, UDP with pseudo-header checksum;
  `mod.rs`: static config for QEMU user networking 10.0.2.15/24 via 10.0.2.2, ARP cache + resolution, net thread polling
  every 10 ms, answers ARP and pings, ping replies, UDP sockets with receive queues; waits use `Wait::Net`). Syscalls 31–35
  `net_info`, `ping`, `udp_socket`, `sendto`, `recvfrom` (5th argument in R8); `Handle::Udp`. Programs `ping`, `netinfo`,
  `udpecho`. QEMU gets `-netdev user -device e1000` (`KERNEL_NETDEV_OPTS` for extras). Bug #5 (`run_on` could starve the
  target CPU's idle thread) fixed. **Stage 5 complete: 185 in-kernel tests + 6 test kernels + 94 E2E steps (BIOS + UEFI).**

- **2026-10-24** – User wants all of the stage 6 ideas; order M28–M33 chosen by dependencies.
- **2026-10-24** – M28: `arch/x86_64/tsc.rs` calibrates the time stamp counter against PIT channel 2 (50 ms, after the LAPIC
  calibration); `time::uptime_us`. Syscall 36 `clock_us`. Ping replies are time-stamped in µs (`ping` prints ms with three
  decimals – the receive time is still when the net thread polls, i.e. up to 10 ms late). New program `time <program>`.

## Open items / later
- AHCI uses polling; switching to completion interrupts (MSI or the PCI interrupt line) would free the CPU during I/O.
- Thread-level sleeping mutex (today all shared state uses spin locks with interrupts disabled).
- `ps` CPU percentages only cover threads that still exist (dead threads are reaped with their counters).
- Reclaim `BOOTLOADER_RECLAIMABLE` memory once we no longer use Limine's stack, page tables and responses.
- Shell lines longer than one console row are not handled by the line editor (input is capped at 120 characters).
- `gcc` is installed now → `make limine-tool` (bios-install for USB/HDD images) could be used (not tried yet).
- User space: no AVX (XSAVE not enabled, AVX instructions kill the program), no SMAP/SMEP, heap is mapped eagerly (no demand paging),
  only a single CPU, no signals beyond "kill", no pipes (a child always writes to its parent's output).
- Network: static IP (no DHCP), no TCP, no fragmentation, no DNS client; polled driver (10 ms latency); ping resolution
  10 ms (no fine-grained clock).
- `kshell`/`kcommands` are a bridge: the information commands (mem, ps, cpu, acpi, lspci, ...) should become real syscalls
  and programs. `/bin/sh`: no scripts, no environment variables, no `2>`, operators must be separate words, built-in output
  in a pipeline must fit into one pipe (64 KiB) because it is written before the next stage starts.
- File syscalls: `read`/`write_at` re-walk the cluster chain per call; directory handles are snapshots; no truncate-to-length.
- Still behind `kshell`: cpu, acpi, lspci, disk, hexdump, memmap, layout, demo, colors, version, int3, panic, reboot, poweroff,
  write, append, echo, sleep, clear, help. Without `/bin` on the disk, the kernel shell has no `ls`/`cat`.

## Next step
**M29 – TCP:** connection state machine (SYN/SYN-ACK/ACK, FIN handshakes, RST), sequence numbers, a send buffer with
retransmission (timeout from the measured RTT, µs clock from M28), receive buffer and advertised window, MSS 1460.
Syscalls `tcp_connect(ip, port)`, `tcp_listen(port)`, `tcp_accept(fd)`; `read`/`write`/`close` on the connection fds.
Programs: `nc` (netcat-like client), `echod` (echo server). Tests: connect to a Python TCP echo server on the host via
10.0.2.2, an E2E transfer of 100 KB, `hostfwd=tcp::<port>-:7` so the host connects to `echod` in the guest.
