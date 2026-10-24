# KernelTry – hobby kernel in Rust (x86_64, QEMU)

A kernel written from scratch together with an AI. Runs in QEMU only.

**Language rule:** everything in this repository (docs, code comments, kernel output, commit messages) is **English**.
The conversation with the user happens in German.

## Read first
1. `docs/PROGRESS.md` – where we are and what comes next
2. `docs/DESIGN.md` – architecture and rationale, before changing anything fundamental
3. `docs/BUGS.md` – known pitfalls, especially for triple faults and boot problems

## Commands
| Command | What |
|---|---|
| `make build` | build the user programs and the kernel (`kernel/target/x86_64-unknown-none/debug/kernel`) |
| `make user` | build the user programs (`user/`) into `build/user/bin` (installed into `/bin` on every disk image) |
| `make run` | QEMU with a window (BIOS), serial output in the terminal |
| `make run-uefi` | same, with OVMF/UEFI firmware |
| `make test` | all in-kernel tests headless in QEMU, exit status 0 = green |
| `make e2e` | boot the real kernel and drive the shell over serial (`scripts/e2e-shell.py`) |
| `make e2e-uefi` | same E2E test, booted through OVMF/UEFI |
| `make check` | `make test` + `make e2e` + `make e2e-uefi` – run this before every commit |
| `make fmt` | rustfmt over the kernel (commit formatting separately) |
| `make disk` / `make disk-reset` | create / recreate the persistent FAT32 disk `build/disk.img` from `rootfs/` |
| `make debug` + `make gdb` | QEMU waits for gdb on :1234 |
| `make disasm` / `make symbols` / `make sections` | GNU objdump / nm / readelf on the kernel ELF |
| `make addr2line ADDR=0xffffffff8000...` | map a code address (e.g. RIP from a panic) to file:line |
| `python3 -I scripts/qemu-monitor.py <elf> [s] -- "info pic" …` | run QEMU monitor commands against the running kernel |
| `python3 -I scripts/screenshot.py <elf> <out.png> [s] [keys…]` | boot headless, optionally send keys (QEMU `sendkey` names = US key positions), save screenshot + serial log |

Tests run headless and abort after 30 s (`KERNEL_TEST_TIMEOUT`), so Claude can run them unattended.
Interrupt problems: QEMU monitor `info pic`, `info lapic`, `info registers`.
Triple faults: append `-d int,cpu_reset -D <logfile>` to the `scripts/run-qemu.sh` invocation.

## Toolchain
- Rust nightly via rustup, target `x86_64-unknown-none`.
- **GNU tools preferred where they fit:** GNU ld (`ld.bfd`) links the kernel, GNU make drives everything,
  gdb for debugging, binutils (objdump, nm, readelf, addr2line) for inspection, gcc builds host helpers (Limine utility).

## Workflow (mandatory)
- Small milestones from `docs/PROGRESS.md`, one after another.
- Per step: implement → `make check` green → update docs → commit `M<n>: <what>` → push.
- Every milestone gets at least one test. Every bug fix gets an entry in `docs/BUGS.md` and, if possible, a regression test.
- At the end of a session, `docs/PROGRESS.md` must say under "Next step" where to continue.
- Claude implements everything itself (the user decided this); new subsystems get a short explanation in `docs/LEARN.md`.
- "From scratch": GDT, IDT, paging, allocators and drivers are our own code.
  Allowed crates: `limine` (boot protocol), `spin` (locks). Ask before adding new crates.
- `sudo` cannot be run by Claude (no TTY for the password) – ask the user to run installs in a separate terminal.

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
- `kernel/src/process/` – `mod.rs` (process table, spawn/wait/kill), `elf.rs` (ELF64 loader), `syscall.rs` (syscall dispatcher)
- `kernel/src/input.rs` – merged keyboard + serial input, `TerminalDecoder`; `kernel/src/ringbuf.rs` – IRQ-safe byte ring buffer
- `kernel/src/shell/` – `mod.rs` (commands, `Shell::run`), `line_editor.rs`, `parser.rs`
- `kernel/src/task/` – threads + preemptive scheduler (`mod.rs`), context switch assembly (`switch.rs`)
- `kernel/src/drivers/pci.rs` – PCI enumeration; `drivers/ahci.rs` – SATA driver; `drivers/block.rs` – `BlockDevice`, `RamDisk`;
  `drivers/mod.rs` – global `DISK`; `drivers/rtc.rs` – CMOS real-time clock
- `kernel/src/fs/` – `mod.rs` (root mount `fs::ROOT`, `SystemDisk`, path `normalize`), `fat32.rs` (FAT32 read/write, LFN, `format`)
- `rootfs/` – initial contents of the FAT32 disk image; `scripts/mkdisk.sh` – builds the image;
  `scripts/check-test-disks.sh` – `fsck.fat` + mtools validation of the test disks (run by `make test`);
  `scripts/install-user.sh` – copies `build/user/bin/*` into `/bin` of a disk image
- `user/` – user programs: `src/lib.rs` (`ulib`: `_start`, `Args`), `sys.rs` (syscalls), `io.rs` (`print!`, `read_line`),
  `fs.rs` (`File`, `Dir`, `stat`), `heap.rs` (allocator on `brk`),
  `process.rs` (`spawn`/`wait`/`kill`), `src/bin/*.rs` (one program each: hello, upper, fault, cat, ls, cp, sort,
  fstest, heaptest, spawntest), `linker.ld`
- `kernel/src/time.rs` – ticks, `uptime_ms`, `sleep_ms`
- `kernel/src/drivers/keyboard.rs` – PS/2 driver, ring buffer, `read_key()`; `drivers/scancode.rs` – decoder + layouts (DE/US)
- `kernel/src/arch/x86_64/` – CPU specifics: `port.rs` (in/out), `gdt.rs` (GDT+TSS), `idt.rs` (IDT structure),
  `interrupts.rs` (exception handlers, kernel IDT), `syscall.rs` (syscall entry stub, MSRs, `enter_user`), `irq.rs` (IRQ routing + handlers), `lapic.rs` (local APIC + timer), `ioapic.rs`, `pic.rs` (legacy, masked), `pit.rs` (timer calibration), `msr.rs`, `cpu.rs` (cpuid), `power.rs` (reboot/poweroff),
  interrupt helpers in `mod.rs`
- `kernel/tests/` – integration tests (each one is a separate kernel)
- `kernel/linker.ld` – higher-half layout at `0xffffffff80000000`
- `scripts/run-qemu.sh` – cargo runner: builds the ISO and starts QEMU
- `vendor/limine` – Limine binaries (not in git, `make limine`)
