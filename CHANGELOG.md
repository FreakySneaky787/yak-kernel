# Changelog

## Unreleased
**Course**
- Chapter 14 – The other CPUs: a real-mode trampoline below 1 MiB, INIT/STARTUP IPIs, a GDT, TSS and stack per
  CPU, and `run_on` for jobs on other CPUs; QEMU runs with 4 CPUs. Bug #28 in chapter 13 fixed on the way (the
  boot hung about once in 20 in `task::init`).
- Chapter 13 – Threads and a scheduler: kernel threads with guarded stacks, a context switch in assembly,
  round-robin preemption by the local APIC timer, and blocking waits (sleep, keyboard, `join`) without lost wake-ups;
  the heap now locks with interrupts off, since the timer handler allocates.
- Chapter 12 – ACPI and the APIC, the start of part IV: the MADT from the ACPI tables, the I/O APIC routing the
  timer and the keyboard, and the local APIC timer measured against the PIT; QEMU runs with a local APIC again.
  Bug #27 found and fixed on the way (the PIT's square wave ticked twice per period through the I/O APIC).
- Chapter 11 – A first shell, part III: a line editor, a splitter for words with quotes, and a table of six
  commands; `make test` types four commands with QEMU's `sendkey` and checks the answers.
- Chapter 10 – The heap, the end of part II: a free-list allocator behind `#[global_allocator]` that grows by
  mapping pages with chapter 9's `map`; `Box`, `Vec` and `String` work in the kernel, and a request over the
  limit (`fault=heap`) ends in a panic.
- Chapter 9 – Paging: the live page tables read and changed (`translate`, `map`, `unmap`), and kernel stacks with
  a guard page – a real stack overflow ends in the double fault handler.
- Chapter 8 – Physical memory, the start of part II: Limine's memory map, a bitmap frame allocator over the
  usable RAM, and physical addresses reached through the higher-half direct map.
- Chapter 7 – The keyboard, the end of part I: IRQ 1, a ring buffer between handler and kernel, a scancode
  decoder with Shift and Caps Lock, and a kernel that echoes what you type. `make test` types on the emulated
  keyboard with QEMU's `sendkey`.
- Chapter 6 – Hardware interrupts and the timer: the 8259 PIC moved to vectors 32–47, a 100 Hz PIT timer, and
  `sleep_ms` that waits in `hlt` and never ends early. From this chapter on QEMU runs with a CPU without local APIC
  (bug #25: under QEMU 8.2 no PIC interrupt arrives otherwise).
- Chapter 5 – Interrupts and exceptions: an IDT with handlers for six CPU exceptions, a double fault that survives a
  broken stack on IST1, and faults on purpose via the kernel command line (`KERNEL_CMDLINE=fault=page make run`).
- Chapters 1–4 checked as a first reader would use them: the pages now show every line a learner has to change
  (including the calls in `kmain`), name the `diff` command against the previous chapter, and QEMU's own messages in
  `make test` go to `build/qemu.log` (printed when a check fails).
- Issue forms "I got stuck in a course chapter" and "Feedback on the course"; `CONTRIBUTING.md`.
- `scripts/check-course.py` also checks the course table, the issue form's chapter list and the `diff` line of each page.

**Tests**
- `paging::translate` no longer reads the PAT bit of a 2 MiB or 1 GiB page as an address bit (bug #26; no caller
  was affected, all of them translate 4 KiB pages).
- The userspace integration test no longer fails when a 100 ms sleep wakes up two ticks late on a busy host (bug #24).

## 1.0.0 (2026-10-27)
The first stable release: a reliable, text-based x86-64 system for QEMU (`docs/GOALS.md`). Stages 9 to 12 (`docs/ROADMAP.md`, phases A–C; `docs/PROGRESS.md`).

**Shell and console**
- `/bin/sh`: command substitution `$(...)`, `"$@"` / `"$*"`, `case … esac`, `local` variables in functions; an
  assignment the environment cannot hold (> 4096 bytes) is an error instead of being lost.
- Input lines up to 4096 characters, edited over several screen rows (was: 120 characters); the console understands
  the cursor movement sequences `ESC[nA/B/C/D`.

**Kernel**
- Thread stacks have an unmapped guard below them: a thread's stack overflow kills the process instead of overwriting
  the heap (`thread_spawn` with stack 0 = kernel-chosen slot).
- UDP checksums are verified on receipt.

**Release and tests**
- Release files: a bootable ISO, the disk image, `run-yak.sh` – YAK runs with nothing but QEMU (README, "Try it
  without building"); attached to every GitHub release by `.github/workflows/release.yml`; checklist
  `docs/RELEASING.md`.
- README "Supported environment" with measured RAM limits (BIOS from 24 MiB, UEFI from 160 MiB).
- Fault tests: a disk that fails at every single I/O of seven file operations (FAT32 stays mountable, only
  fsck-repairable damage); TCP with lost packets, a silent peer and connect timeouts (`kernel/tests/netfault.rs`);
  a misbehaving DNS server.

**Name**
- The project is now called **YAK – Yet Another Kernel** (formerly KernelTry): boot banner, `uname`, disk label,
  docs and website.

**Infrastructure**
- CI: `make check` on fresh Ubuntu and Fedora machines for every push and pull request
  (`.github/workflows/check.yml`); the website is built there too.
- Website: the documentation as a book (mdBook over `docs/`, `make book`), deployed to GitHub Pages from `main`.
- `docs/ROADMAP.md`: the plan to turn YAK into a course that rebuilds the kernel chapter by chapter.

**Fixed** (see `docs/BUGS.md`)
- #21: a program's heap could grow into its stack area.
- #20: `docs/DESIGN.md` said there was no block cache (found by an outside review).
- #19: a new process could run its first syscall before its process table entry existed (SMP race in `spawn_with`);
  the user-space panic handler needed the heap and recursed when an allocation failed.
- #18: `sleep_ms` could end up to one tick early.
- #17: the TLB shootdown regression test's time limit was too strict for CI machines.

## 0.8.0 – pre-release (2026-10-26)
Stage 8: no new subsystems, but the release criteria of `docs/GOALS.md`.

**For readers**
- `README.md` with screenshot, host packages and quick start; `docs/GOALS.md` (purpose, audience, release criteria).
- Four walkthroughs through the code (`docs/walkthrough/`), checked by `scripts/check-doc-refs.py` in `make check`.
- Guided experiments (`docs/EXPERIMENTS.md`) and recipes for extending the system (`docs/HACKING.md`).
- License: GNU General Public License v3 or later (`LICENSE`).

**Reproducibility**
- Rust toolchain pinned in `rust-toolchain.toml`; `make deps` checks the host tools (Fedora and Debian/Ubuntu package names).
- OVMF firmware found on Fedora and Debian/Ubuntu.

**Reliability**
- `/bin/syscheck`: every syscall with bad pointers, lengths, descriptors and numbers, seeded random calls, and a race
  mode; `make soak` (long mixed workload) and `make stress` (the whole suite many times with 1/2/4 CPUs, `qemu64`, UEFI).
- Fixed: a huge length froze the machine (#11); an empty buffer with a null pointer panicked the kernel (#12);
  `readdir` lost an entry on EFAULT (#13); a user copy racing with `brk` crashed the kernel (#14); TLB shootdowns waited
  for idle CPUs, up to forever (#15, now a dedicated IPI); one `write` could grow the kernel heap by its whole buffer (#16).

**New**
- Syscall 52 `uname` and `/bin/uname` (the worked example of `docs/HACKING.md`).

## Before 0.8.0 – stages 1–7 (version number 0.1.0 throughout) (2026-10-24 … 2026-10-25)
Milestones M1–M38, see `docs/PROGRESS.md`: boot via Limine (BIOS/UEFI), own GDT/IDT/paging/allocators, APIC and SMP
scheduler, AHCI + FAT32, ring 3 with ELF loader and syscalls, `/bin/sh`, pipes, signals, threads, editor, FPU/AVX state,
SMEP/SMAP, e1000 + IPv4/UDP/TCP/DHCP/DNS/HTTP.
