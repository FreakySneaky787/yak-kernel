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
| 29 | TCP: connect/listen/accept, retransmission, flow control; client and server programs | ✅ | TCP packet test, refused + 100 KB loopback echo in the kernel, `nc`/`echod` between processes, E2E: host echo, 100 KB from the host, host → guest 50 KB via hostfwd |
| 30 | DHCP and a DNS client; HTTP client (`wget`) and server (`httpd`) | ✅ | DHCP packet test, `card_is_configured` checks the DHCP lease, `host --selftest` (7 DNS checks), httpd + wget over loopback (200/404), E2E: fake DNS on the host, wget from a host HTTP server, the host fetches from httpd via hostfwd |
| 31 | Shell: scripts, environment variables, `2>`, background jobs (`&`, `jobs`, `wait`) | ✅ | scripted sh session (expansion, inheritance, `$?`, `2>`, job + `wait`, `#!` script) and a `set -e` script in the integration test, 8 E2E steps |
| 32 | Storage speed: sector cache for FAT32, AHCI completion interrupts (MSI) | ✅ | BlockCache unit test (hits, runs, write-through, eviction), lookup-cache invalidation test, MSI test (≥ 1 interrupt per command), benchmark line in the integration test; suite green with 1 and 4 CPUs |
| 33 | Memory: reclaim bootloader memory, demand paging for user heaps | ✅ | `tests/reclaim.rs` (≥ 1 MiB gained, former bootloader frames are handed out and survive being overwritten, threads + a program afterwards), demand-paging unit test (32 MiB heap, 3 touched pages, < 100 frames), existing brk/heaptest checks; suite green with 1 and 4 CPUs |

## Stage 7: a more Unix-like system (agreed with the user on 2026-10-25: "alle")
| # | Milestone | Status | Tests |
|---|---|---|---|
| 34 | Signals: handlers in user space, `kill -SIG`, Ctrl+C as SIGINT, interrupted blocking calls | ✅ | 2 signal unit tests (dispositions, pending order, blocking during a handler, reasons ↔ numbers), `sigtest` (23 checks incl. all GPRs + XMM0–15 kept when a handler interrupts a loop without syscalls, EINTR from sleep, no nesting, SIG_IGN inherited), sh `kill` session, E2E: Ctrl+C caught by a handler, `kill -USR1 %1` |
| 35 | Shell programming: `if`/`while`/`until`/`for`, functions, `&&`/`\|\|`, `;`, `test`, `$((...))`, `read`, wildcards, multi-line input, Ctrl+C stops loops | ✅ | `/m35.sh` in the integration test (39 output lines compared exactly: all constructs, break/continue n, functions + recursion, read from pipes and files, globbing, `set -e`), interactive session (continuation prompt, syntax error, exit in a function), stack growth unit test, 6 E2E steps incl. Ctrl+C ending a loop |
| 36 | Threads in user processes (spawn/join, futex-style wait/wake) and a sleeping mutex in the kernel | ✅ | 2 `sync::Mutex` unit tests (exclusion, a waiter blocks on `Wait::Lock`), `threadtest` (17 checks: shared counter, join codes, futex mutex + heap from 4 threads, futex EAGAIN/timeout/wake, ESRCH, exit / fault / main return / SIGTERM with running threads), no threads left over in the integration test, E2E; suite green with 1–4 CPUs |
| 37 | CPU protection and AVX: SMEP/SMAP, XSAVE with AVX state per thread | ✅ | `tests/smap.rs` (user_access works, kernel read and kernel jump into a user page fault), CR4/XCR0 unit tests, YMM save/restore unit test, `fputest` with YMM0–15 in 4 preempted workers, `sigtest` YMM kept across a handler, E2E `cpu` shows SMEP/SMAP/AVX on; suite also green with `KERNEL_CPU=qemu64` and 1 CPU |
| 38 | Network polish: e1000 interrupts, TCP window scaling, DHCP lease renewal | ✅ | interrupt test (GSI found, median ping < 3 ms – about 0.5 ms now), 1 MB loopback transfer with a scaled window > 64 KiB, WS option round trip, lease phase/retry unit test, renew + rebind against QEMU's DHCP server, renewal message test; suite green with 1, 2, 4 CPUs and `qemu64` |

## Stage 8: release 1.0 – reliable and understandable (proposed 2026-10-26, see `docs/GOALS.md`)
No new kernel features: this stage makes the existing system meet the release criteria in `docs/GOALS.md`.
| # | Milestone | Status | Tests |
|---|---|---|---|
| 39 | Front door: `README.md` with screenshot and host packages, `docs/GOALS.md`, OVMF path found on Fedora and Debian/Ubuntu | ✅ | `make check` green on Ubuntu 24.04 with the detected OVMF path |
| 40 | Reproducible setup: toolchain pinned (`rust-toolchain.toml`, nightly-2026-10-26), `make deps` checks host tools and prints the dnf/apt command, `make check` runs it first; LICENSE (ECPL v1.2) | ✅ | `check-deps.sh --selftest` (hidden tool reported, exit 1) in `make check`; manual run with a reduced PATH; `make check` green with the pinned toolchain on Ubuntu 24.04 |
| 40b | Optional CI (GitHub Actions running `make check` under TCG) – only if the user wants it | ⬜ | a green run on a push |
| 41 | Reliability gate: `/bin/syscheck` (bad pointers/lengths/fds/numbers for every syscall, 20 000 seeded random calls, `race` mode), `make soak` (`rootfs/soak.sh` via `scripts/soak.py`), `make stress` (suite N× with 1/2/4 CPUs, `qemu64`, UEFI, then soak); TLB shootdown by IPI | ✅ | `syscheck` + `syscheck race` in integration test, `syscheck` E2E step, shootdown unit test (old code: hangs), soak negative check (an injected failure is reported); bugs #11–#15 found and fixed |
| 42 | Known limits: "Open items" sorted into documented limitations (no open release blocker); found and fixed on the way: #16 (one `write` grew the kernel heap by its whole buffer) | ✅ | `syscheck`: 16 MiB into a pipe without reader → EPIPE with < 1 MiB heap growth (old kernel: +16 388 KiB), oversized `sendto` → EMSGSIZE |
| 43 | Walkthroughs (`docs/walkthrough/`): boot to `/bin/sh`, a key press, `cat \| wc`, a page fault – with links to the functions | ✅ | `scripts/check-doc-refs.py` (`make docs-check`, part of `make check`, with self-test): every \[`symbol`\](file) link must name a symbol in an existing file; "things to try" checked in the running system |
| 44 | Experiments (`docs/EXPERIMENTS.md`): more cores, no FPU save, the bug #14 fix undone, a 16-byte pipe – no build switches, each a small hand edit | ✅ | each experiment run once, results recorded (590 → 250 ms; 4 workers corrupted; panic 2/2; 1.40 → 5.41 s) |
| 45 | Extending guides (`docs/HACKING.md`): syscall, user program, test, shell built-in | ✅ | the syscall + program guide followed once: syscall 52 `uname` + `/bin/uname` (integration test, E2E step, `syscheck` bad pointers, `SYSCALL_COUNT`) |
| 46 | Pre-release 1.0.0-rc.1: GPL-3.0-or-later (`LICENSE` = FSF text, SHA-256 `3972dc97…`), version in both manifests and the boot banner, `CHANGELOG.md`, new screenshot | ✅ | `make check` green; `uname` shows `KernelTry 1.0.0-rc.1 x86_64` |
| 47 | Release 1.0.0: everything under "Missing for 1.0" below, then version 1.0.0, `make check` + `make stress` on that commit, tag `v1.0.0` on the default branch | ⬜ | – |

Decided 2026-10-26: name KernelTry; license first ECPL v1.2, then (same day) **GNU GPL v3 or later** (copyright
FreakySneaky787 and contributors); 1.0 goes out as a pre-release `1.0.0-rc.1` first.
Still open: N for `make stress` (proposal: 5), whether CI is wanted (M40b), whether experiments may add build switches to the kernel (M44).

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

- **2026-10-25** – M29: `net/tcp.rs` (state machine, go-back-N retransmission with RTT-based RTO, windows, loopback for our own
  address), syscalls 37–40 `tcp_connect`/`tcp_listen`/`tcp_accept`/`shutdown`, `read`/`write`/`close` on connections, programs
  `nc` and `echod`, kernel command `netstat`. Bug #6 fixed: the boot CPU's idle thread did not run `smp::run_on` jobs (hang
  when a thread on another CPU sent a job to CPU 0); one idle loop for all CPUs now, regression test added. IPIs are sent with
  interrupts off; SMP-robust leak tests (`pmm::frames_lost`).

- **2026-10-25** – M30: `net/dhcp.rs` (DISCOVER/OFFER/REQUEST/ACK with the broadcast flag; `net::init` waits up to 3 s per
  step, static fallback otherwise); the network config is now mutable (`dns`, `dhcp` flag) and accepts all packets to our MAC
  while unconfigured. DNS client in user space (`ulib::dns`: query building, compressed-name parsing, retries; server from
  `net_info`, which now reports DNS and DHCP). Programs `host`, `wget` (HTTP/1.0, `-O file`), `httpd` (files below the cwd,
  directory listings, 403/404/405).

- **2026-10-25** – M31: per-process environment in the kernel (inherited at `spawn`, init gets `PATH=/bin`, `HOME=/`),
  syscalls 41–43 `setenv`/`getenv`/`environ`, `wait` flag WNOHANG. `/bin/sh` rewritten around it: `$NAME`/`${NAME}`/`$?`/
  `$$`/`$#`/`$0..$9` expansion (not in '...', `\$` escapes), `#` comments, `NAME=value`/`export`/`unset`, `$PATH` lookup,
  `2>`/`2>>`/`2>&1`, background jobs (`&`, `jobs`, `wait`, `fg`, "Done" before the prompt), scripts (`sh file args`,
  `sh -c`, `source`/`.`, `#!` files run through `sh`, `set -e`), `true`/`false`, `exit` defaults to `$?`. New program `env`.
  Found: lines of concurrent programs were mixed mid-line because `println!` wrote every format piece separately – the
  `ulib` print macros now format first and write once.

- **2026-10-25** – M32: measured first (300 KB through `wc`: 0.12 s; `read_at` in 4 KiB steps: 94 ms – the disk itself
  needs only ~3 ms, the time went into walking the FAT chain and the directory on *every* call). Now: `BlockCache` (write-
  through sector cache, 512 KiB, LRU by eighths) under the root file system, `read_at` reads contiguous clusters with one
  request, FatFs caches the last cluster chain and the last path lookup (both dropped on any write). Result: `read_at` 94 →
  5 ms, `wc` of 300 KB 0.12 → 0.038 s. AHCI completes commands by MSI (PCI capability list, `Device::enable_msi`): the
  waiting thread blocks on `Wait::Disk` instead of spinning (single commands get slower, ~2×, but the CPU is free);
  polling remains for early boot. Bug #7 (an idle CPU only noticed threads woken by its own interrupts at the next tick)
  found with `KERNEL_SMP=1` and fixed (`task::preempt_idle` at the end of waking interrupt handlers).
- **2026-10-25** – M33: `mem::reclaim_bootloader_memory` (first thing in `kernel_main`, on our own stack): copies every page
  table that lives in bootloader memory (`paging::copy_kernel_tables`, the mappings stay identical), points every thread
  (`task::retarget_cr3`) and every CPU (`smp::run_on` + CR3 reload) at the new PML4, then frees the regions
  (`pmm::release_region`; the bitmap now covers them from the start). The memory map is copied at boot (`pmm::boot_memmap`)
  because Limine's lives in that memory. QEMU with 256 MiB: ~11.7 MiB gained. Demand paging: `brk` only moves the heap
  end; a not-present user fault inside the heap maps a zeroed page and retries (`process::handle_page_fault`), syscalls
  populate heap pages before checking user buffers. **Stage 6 is complete.**

- **2026-10-25** – M34: signals (`process/signal.rs`): dispositions, pending mask, `kill(pid, sig)`, `sigaction`,
  `sigreturn`; delivery when returning to ring 3 after a syscall or a timer/wake interrupt. Handlers run through a
  trampoline in `ulib` that saves all registers itself – the kernel only pushes `sig, handler, rflags, rip` (so the user
  target now has `disable-redzone`). Ctrl+C sends SIGINT (raw 0x03 only for `ignore_interrupt` programs without a
  handler); `sleep` is interruptible; `$?` of a signalled child is 128 + signal; sh built-in `kill [-SIG] pid|%job`, `kill -l`.
  Bug #8 (MSI test assumed one interrupt per command – MSIs coalesce in the APIC) found while running with 2 CPUs.

- **2026-10-25** – M35: `/bin/sh` became a small POSIX-like language: tokenizer + recursive-descent parser
  (`syntax.rs`) into lists / and-or / pipelines / compound commands / functions, executed with a `Flow` result;
  `$((...))` (`arith.rs`), `test`/`[` (`testcmd.rs`), `read`, `shift`, `:`, wildcards (`glob.rs`), continuation prompt,
  compound commands with redirections and in pipelines (`cat f | while read l; do ...; done`), background compound
  commands via `sh -c`. Ctrl+C stops loops (SIGINT handler + children killed by SIGINT). Kernel: file offsets are now shared
  between `dup`-ed descriptors (the shell's fd save/restore re-read the same line forever otherwise), user stacks grow on
  demand up to 1 MiB (4 pages mapped at start). Bug #9 (`run_on` priority inversion between idle thread and waiter) fixed.

- **2026-10-25** – M36: threads. `thread_spawn`/`thread_exit`/`thread_join`/`gettid`, every user thread has an
  `owner` (the pid) in the scheduler; process end with running threads via `exiting` (the main thread joins the
  others before freeing the address space); futexes (`process/futex.rs`) and `ulib::sync::Mutex`, thread-safe user
  heap, `ulib::thread::spawn`. TLB shootdown for `brk` shrinking with threads. Kernel: sleeping `sync::Mutex` for the
  disk and the root file system (found while writing its test: `bool::then_some` built the guard eagerly, so a
  failed `try_lock` unlocked somebody else's lock - `then` is lazy).

- **2026-10-25** – M37: QEMU now runs `-cpu max` (TCG; `KERNEL_CPU` overrides). SMEP + SMAP on every CPU that has them,
  user memory only inside `cpu::user_access` (`stac`/`clac`); `read_stdin` now copies with `copy_to_user` after its
  wait. XSAVE with XCR0 = x87 | SSE | AVX, 1 KiB `FpuState` per thread; the signal trampoline uses `xsave64`.
  User programs stay SSE2 and detect AVX at run time. Bug #10 (test IDTs without IRQ vectors, exposed by the slower
  CPU model) fixed.

- **2026-10-25** – M38: QEMU's e1000 has no MSI, so `net::probe_irq` finds its I/O APIC input by trying GSI 16-23 with a
  software-raised interrupt (`ICS`); the net thread now sleeps until the card interrupts (ping to the gateway 2–10 ms →
  ~0.5 ms; loopback packets wake it too). TCP window scaling (shift 3, 256 KiB buffers, 128 segments in flight); DHCP
  lease renewal thread (T1 renew, T2 rebind, static fallback at the end). **Stage 7 is complete.**

- **2026-10-26** – User wrote a project definition ("KernelTry – Understand the Kernel"): recorded as a proposal in
  `docs/GOALS.md`. Stage 8 (M39–M46) planned around its release criteria. M39: `README.md` (overview, screenshot
  `docs/images/shell.png`, host packages, quick start, documentation map), Makefile finds OVMF on Fedora and Debian/Ubuntu
  (`OVMF ?=`, override with `make OVMF=<file>`). Found while setting up a fresh Ubuntu container: no toolchain pin
  (stable rustc fails on the user target), hard-coded Fedora OVMF path – both feed M40.

- **2026-10-26** – User chose the license (ECPL v1.2, their own text) and the name KernelTry. M40: `rust-toolchain.toml`
  pins nightly-2026-10-26 (rustc 1.101.0-nightly 76456a688) with rust-src, rustfmt and the `x86_64-unknown-none` target;
  `scripts/check-deps.sh` / `make deps` (Fedora/Debian package names, OVMF, rustup) is the first step of `make check`.
  `license-file` set in both Cargo manifests.

- **2026-10-26** – M41: `syscheck` found five kernel bugs on its first runs – #11 a huge length froze the machine
  (`populate` walked every page), #12 an empty buffer with a null pointer panicked the kernel, #13 `readdir` lost an
  entry on EFAULT, #14 a user copy raced with `brk` in another thread (kernel page fault → panic), #15 TLB shootdowns
  waited for idle CPUs (seconds, or forever with busy CPUs). Fixes: bounded `populate`, empty copies never touch the
  pointer, check-before-advance in `readdir`, check + copy under the process table lock in ≤ 64 KiB pieces, a
  `TLB_VECTOR` IPI. `make soak` (10 rounds ≈ 1–2 min) and `make stress` added. `syscheck race` with 4 CPUs: 4 → ~3500
  heap shrinks in 5 s. DESIGN.md listed the wake-up IPI as 0x60; it is 0xF0 (fixed).

- **2026-10-26** – `make stress` (5 passes) of the M41 commit: green – 20 × E2E 126/126 with 1/2/4 CPUs and
  `qemu64`/UEFI, every `make test` green, soak 10 rounds, 26 min in total. M42: #16 fixed (`sys_write` in 64 KiB
  pieces, `sendto` size check before the copy); the open items became "Known limitations of release 1.0". M43–M45:
  walkthroughs, experiments (all four run and measured), HACKING.md with syscall 52 `uname` as the worked example,
  `CHANGELOG.md`, `make docs-check`.

- **2026-10-26** – User switched the license to the GNU GPL v3 (taken as `GPL-3.0-or-later`, the FSF's recommended
  wording) and wants a pre-release before 1.0. M46: `LICENSE` from Ubuntu's `base-files` (`/usr/share/common-licenses/GPL-3`,
  identical to the FSF's `gpl-3.0.txt` – gnu.org itself is blocked by the sandbox proxy), dependency licenses checked
  (limine, lock_api, scopeguard: MIT OR Apache-2.0; spin: MIT; Limine bootloader: BSD-2-Clause – all GPLv3-compatible),
  version 1.0.0-rc.1.

## Known limitations of release 1.0 (sorted in M42)
Release blockers were defined as "a program can crash, hang or exhaust the kernel" (`docs/GOALS.md`: isolated
programs, reliability). All of those found so far are fixed (bugs #11–#16). The rest are **documented limitations**:
they make KernelTry smaller than a production system, but they do not break the promises of release 1.0.

**Security model**
- Single user, no permissions: every program may signal any process (`kill`), read and write every file, and use
  the kernel-command bridge `kshell` – including `reboot`, `poweroff`, `panic` and `int3`, which exist for teaching.
  Isolation means *memory* isolation: a program cannot read or corrupt another program's or the kernel's memory.
- The kernel heap grows on demand but never shrinks.

**User space**
- No AVX-512; the ELF image is mapped eagerly (heap and stack on demand); no thread-local storage; no guard pages for
  thread stacks; no SIGCHLD/SIGSTOP/alarm, no signal masks; a CPU exception cannot be caught as a signal.
- `/bin/sh`: no `$(...)`, `case`, `local`, here-documents or `"$@"`; `&&`/`||`/`|`/`&` must be separate words;
  functions are not inherited by `sh -c` background shells; built-ins, functions and loops inside a pipeline must fit
  their output into one pipe (64 KiB) – more blocks until Ctrl+C; background jobs share the keyboard with the shell.
- The line editor handles one console row (input capped at 120 characters).
- `kshell`/`kcommands` bridge the commands that are still kernel built-ins: cpu, acpi, lspci, disk, hexdump, memmap,
  layout, demo, colors, version, int3, panic, reboot, poweroff, write, append, echo, sleep, clear, help. Without `/bin`
  on the disk, the kernel shell has no `ls`/`cat`.

**Files and network**
- FAT32 only; `read`/`write_at` re-walk the cluster chain per call; directory handles are snapshots; no truncate-to-length.
- IPv4 only: no fragmentation, no SACK / persist timer, DNS A records only; the e1000 interrupt line is found by
  probing (no AML interpreter).

**Hardware and tools**
- QEMU is the reference machine (q35, e1000, AHCI, PS/2, COM1); real hardware is not a goal of 1.0.
- `make limine-tool` (bios-install for USB/HDD images) exists but has not been tried.
- `ps` CPU percentages only cover threads that still exist (dead threads are reaped with their counters).
- Under TCG (no KVM) `make check` takes about 2 minutes on a warm build, `make stress` (5 passes) about 30 minutes.

## Missing for 1.0
- **Fresh-host test on Fedora** (release criterion "Reproducibility"): only Ubuntu 24.04 has been tested from the README.
- **A first reader**: somebody who did not build KernelTry follows README → walkthrough 01 → one experiment and
  reports where they got stuck. That is the real test of the "understandable" goal.
- **Merge** this branch into the default branch; the tags `v1.0.0-rc.1` / `v1.0.0` belong on the default branch.
- **`docs/GOALS.md`** is still marked "proposal" – the user confirms it (or changes it).
- Decisions: CI (M40b); license headers (`SPDX-License-Identifier: GPL-3.0-or-later`) in every source file – optional,
  the GPL's "How to Apply" recommends a notice per file; renaming the GitHub repository to KernelTry.

## Next step
1.0.0-rc.1 is prepared (M46). Next: the items under "Missing for 1.0", then M47.
