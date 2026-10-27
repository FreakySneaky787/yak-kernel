# YAK – hobby kernel in Rust (x86_64, QEMU)

A kernel written from scratch together with an AI. Runs in QEMU only.

**Language rule:** everything in this repository (docs, code comments, kernel output, commit messages) is **English**.
I talk to Claude in German.
**Perspective rule:** the docs speak as the author (FreakySneaky787): "I decided …", "my choice" – never "the user
wanted / decided".

## Read first
1. `docs/GOALS.md` – purpose, audience, direction and release criteria
2. `docs/ROADMAP.md` – long-term direction (learning platform, course in chapters)
3. `docs/PROGRESS.md` – where we are and what comes next
4. `docs/DESIGN.md` – architecture and rationale, before changing anything fundamental
5. `docs/BUGS.md` – known pitfalls, especially for triple faults and boot problems

## Commands
| Command | What |
|---|---|
| `make deps` | check host tools, print the install command for missing ones (first step of `make check`) |
| `make build` | build the user programs and the kernel (`kernel/target/x86_64-unknown-none/debug/kernel`) |
| `make user` | build the user programs (`user/`) into `build/user/bin` (installed into `/bin` on every disk image) |
| `make run` | QEMU with a window (BIOS), serial output in the terminal |
| `make run-uefi` | same, with OVMF/UEFI firmware |
| `make test` | all in-kernel tests headless in QEMU, exit status 0 = green |
| `make e2e` | boot the real kernel and drive the shell over serial (`scripts/e2e-shell.py`) |
| `make e2e-uefi` | same E2E test, booted through OVMF/UEFI |
| `make check` | `make deps` + `make test` + `make e2e` + `make e2e-uefi` – run this before every commit |
| `make soak` | long mixed workload on a scratch disk (`SOAK_ROUNDS`, default 10, ~10 s per round) |
| `make stress` | reliability gate: suite `STRESS_N` (5) times with 1/2/4 CPUs, `qemu64`, UEFI, then soak – about 30 min under TCG |
| `make book` | build the website (mdBook over `docs/`, `book.toml`) into `build/book`; fails on broken links (needs `mdbook`, see `make deps`) |
| `make course` | course: static checks + `make test` in every chapter folder `course/NN-slug/` (`scripts/check-course.py`) |
| `make fmt` | rustfmt over the kernel (commit formatting separately) |
| `make disk` / `make disk-reset` | create / recreate the persistent FAT32 disk `build/disk.img` from `rootfs/` |
| `make debug` + `make gdb` | QEMU waits for gdb on :1234 |
| `make disasm` / `make symbols` / `make sections` | GNU objdump / nm / readelf on the kernel ELF |
| `make addr2line ADDR=0xffffffff8000...` | map a code address (e.g. RIP from a panic) to file:line |
| `python3 -I scripts/qemu-monitor.py <elf> [s] -- "info pic" …` | run QEMU monitor commands against the running kernel |
| `python3 -I scripts/screenshot.py <elf> <out.png> [s] [keys…]` | boot headless, optionally send keys (QEMU `sendkey` names = US key positions), save screenshot + serial log |

Tests run headless and abort after 30 s (`KERNEL_TEST_TIMEOUT`), so Claude can run them unattended.
QEMU runs with 4 cores and `-cpu max`; `KERNEL_SMP=1 make test` (or 2, …) checks other core counts – worth doing after scheduler changes;
`KERNEL_CPU=qemu64 make test` runs without SMEP/SMAP/XSAVE/AVX (fallback paths); `KERNEL_MEM=160M` sets the RAM
(default 256M; limits in README "Supported environment").
Interrupt problems: QEMU monitor `info pic`, `info lapic`, `info registers`.
Triple faults: append `-d int,cpu_reset -D <logfile>` to the `scripts/run-qemu.sh` invocation.

## Toolchain
- Rust nightly via rustup, pinned in `rust-toolchain.toml` (update deliberately + `make check`), target `x86_64-unknown-none` for the kernel; user programs use `user/x86_64-yak-user.json`
  with `-Zbuild-std` (needs the `rust-src` component and `gcc` as host linker – both installed).
- **GNU tools preferred where they fit:** GNU ld (`ld.bfd`) links the kernel, GNU make drives everything,
  gdb for debugging, binutils (objdump, nm, readelf, addr2line) for inspection, gcc builds host helpers (Limine utility).

## Workflow (mandatory)
- Small milestones from `docs/PROGRESS.md`, one after another.
- Per step: implement → `make check` green → update docs → commit `M<n>: <what>` → push.
- Every milestone gets at least one test. Every bug fix gets an entry in `docs/BUGS.md` and, if possible, a regression test.
- At the end of a session, `docs/PROGRESS.md` must say under "Next step" where to continue.
- Claude implements everything itself (my decision); new subsystems get a short explanation in `docs/LEARN.md`.
- "From scratch": GDT, IDT, paging, allocators and drivers are our own code.
  Allowed crates: `limine` (boot protocol), `spin` (locks). Ask before adding new crates.
- `sudo` cannot be run by Claude (no TTY for the password) – ask me to run installs in a separate terminal.

## Layout
- `kernel/src/lib.rs` – kernel as a library, `init()`, unit test entry point
- `kernel/src/main.rs` – `kmain` and panic handler of the boot binary
- `kernel/src/boot.rs` – Limine requests
- `kernel/src/acpi.rs` – ACPI parser (RSDP → XSDT → MADT)
- `kernel/src/serial.rs` – COM1, macros `serial_print!` / `serial_println!`
- `kernel/src/font.rs` – PSF2 parser, built-in font from `kernel/assets/`
- `kernel/src/console.rs` – framebuffer text console, macros `print!` / `println!` (screen + serial)
- `kernel/src/testing.rs` – test runner, `Testable`, test panic handler
- `kernel/src/qemu.rs` – exit QEMU (isa-debug-exit)
- `kernel/src/mem/` – `mod.rs` (PhysAddr/VirtAddr, HHDM), `pmm.rs` (bitmap frame allocator), `paging.rs` (page tables: translate/map/unmap, kernel root),
  `address_space.rs` (per-process PML4), `stack.rs` (guarded kernel stacks), `heap.rs` (linked-list `#[global_allocator]`)
- `kernel/src/process/` – `mod.rs` (process table, spawn/wait/kill, fds), `elf.rs` (ELF64 loader), `pipe.rs`, `futex.rs`, `signal.rs` (signal state and delivery), `syscall.rs` (syscall dispatcher)
- `kernel/src/input.rs` – merged keyboard + serial input, `TerminalDecoder`; `kernel/src/ringbuf.rs` – IRQ-safe byte ring buffer
- `kernel/src/shell/` – kernel shell (fallback when `/bin/sh` is missing): `mod.rs` (commands, `Shell::run`, `execute_builtin` for `kshell`),
  `line_editor.rs`, `parser.rs` (both also compiled into `/bin/sh`), `keys.rs`
- `kernel/src/task/` – threads + preemptive SMP scheduler (`mod.rs`, `wait_for` for blocking), context switch assembly (`switch.rs`)
- `kernel/src/drivers/pci.rs` – PCI enumeration; `drivers/ahci.rs` – SATA driver; `drivers/block.rs` – `BlockDevice`, `RamDisk`;
  `drivers/mod.rs` – global `DISK`; `drivers/rtc.rs` – CMOS real-time clock
- `kernel/src/fs/` – `mod.rs` (root mount `fs::ROOT`, `SystemDisk`, path `normalize`), `fat32.rs` (FAT32 read/write, LFN, `format`)
- `kernel/src/drivers/e1000.rs` – network card; `kernel/src/net/` – `packet.rs` (Ethernet/ARP/IPv4/ICMP/UDP/TCP/DHCP formats),
  `mod.rs` (stack, net thread, IRQ probing, sockets), `tcp.rs`, `dhcp.rs` (lease + renewal)
- `rootfs/` – initial contents of the FAT32 disk image (`soak.sh` = soak workload, run by `scripts/soak.py`); `scripts/mkdisk.sh` – builds the image;
  `scripts/check-test-disks.sh` – `fsck.fat` + mtools validation of the test disks (run by `make test`);
  `scripts/install-user.sh` – copies `build/user/bin/*` into `/bin` of a disk image
- `user/` – user programs: `src/lib.rs` (`ulib`: `_start`, `Args`), `sys.rs` (syscalls), `io.rs` (`print!`, `read_line`),
  `fs.rs` (`File`, `Dir`, `stat`), `heap.rs` (allocator on `brk`), `dns.rs` (DNS client), `env.rs` (environment), `signal.rs` (handlers + trampoline), `thread.rs` (spawn/join), `sync.rs` (futex `Mutex`),
  `process.rs` (`spawn`/`wait`/`kill`), `src/bin/*.rs` (one program each: tools cat, ls, cp, mv, rm, mkdir, touch, stat,
  sort, wc, grep, ps, mem, uptime, df, date, upper, hello, ping, netinfo, udpecho, time, nc, echod, host, wget, httpd, env, uname; self-tests fault, fstest, heaptest, spawntest, pipetest, sigtest, threadtest, syscheck),
  `src/bin/sh/` (the shell, started as init: `main.rs` executor + built-ins, `syntax.rs` tokenizer/parser, `arith.rs`,
  `glob.rs`, `testcmd.rs`), `src/bin/edit/` (full-screen editor), `linker.ld`
- `kernel/src/time.rs` – ticks, `uptime_ms`, `sleep_ms`; `kernel/src/sync.rs` – sleeping `Mutex` (blocks instead of spinning)
- `kernel/src/drivers/keyboard.rs` – PS/2 driver, ring buffer, `read_key()`; `drivers/scancode.rs` – decoder + layouts (DE/US)
- `kernel/src/arch/x86_64/` – CPU specifics: `port.rs` (in/out), `gdt.rs` (GDT+TSS), `idt.rs` (IDT structure),
  `interrupts.rs` (exception handlers, kernel IDT), `syscall.rs` (syscall entry stub, MSRs, `enter_user`), `irq.rs` (IRQ routing + handlers), `lapic.rs` (local APIC + timer), `ioapic.rs`, `pic.rs` (legacy, masked), `pit.rs` (timer calibration), `msr.rs`, `cpu.rs` (cpuid), `power.rs` (reboot/poweroff),
  `fpu.rs` (x87/SSE/AVX enable, XSAVE/FXSAVE areas), `cpu.rs` also SMEP/SMAP + `user_access`, `smp.rs` (AP trampoline, INIT/SIPI, `run_on`), `tsc.rs` (µs clock), interrupt helpers in `mod.rs`
- `kernel/tests/` – integration tests (each one is a separate kernel; `reclaim.rs` is the only one that reclaims bootloader memory, `smap.rs` checks SMEP/SMAP)
- `kernel/linker.ld` – higher-half layout at `0xffffffff80000000`
- `scripts/run-qemu.sh` – cargo runner: builds the ISO and starts QEMU; `scripts/check-deps.sh` – host tool check (`make deps`)
- `docs/` – also the website source: `docs/SUMMARY.md` (navigation), `docs/index.md` (home page); `scripts/build-book.py`
  builds it and points links to files outside `docs/` at GitHub; `.github/workflows/check.yml` (CI: `make check` on
  Ubuntu + Fedora, `make book`), `.github/workflows/pages.yml` (deploys the website from `main`)
- `course/NN-slug/` – the course chapters, each a complete project (`make run`, `make test`); their pages are
  `docs/course/NN-slug.md`; code blocks with `file=` must match the folder (`scripts/check-course.py`, see `docs/HACKING.md`)
- `LICENSE` – GNU GPL v3 (`GPL-3.0-or-later`, my choice); `README.md` – entry point for new readers
- `vendor/limine` – Limine binaries (not in git, `make limine`)
