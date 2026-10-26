# Project goals

> **Status: proposal** (2026-10-26). Written down from the user's draft; not a final decision yet.
> Changes to this file are agreed with the user.

## Name
**KernelTry – Understand the Kernel.** (Open decision: the repository is called `yak-kernel`, the system calls itself
KernelTry. One name should be chosen before the first release.)

## Purpose
KernelTry is a self-contained operating system for **studying how a modern operating system kernel is built and works**,
hands-on, and for developing it further. It runs in QEMU (BIOS and UEFI).

## Audience
Computer science learners, students, hobby developers and anyone interested in operating system architecture.

## What makes it different
1. **Understandable documentation** – every subsystem is explained (`docs/LEARN.md`), every design decision has a reason
   (`docs/DESIGN.md`), every bug has a story (`docs/BUGS.md`).
2. **Traceable kernel paths** – walkthroughs that follow one action (a key press, a syscall, a page fault, a pipe)
   through the code, with file references.
3. **Reproducible tests** – one command (`make check`) runs unit tests, test kernels and end-to-end shell sessions under
   BIOS and UEFI; the suite also runs with 1–4 CPUs and without modern CPU features.
4. **Room to experiment** – guided experiments ("change X, expect Y") and short guides for extending the system
   (a new syscall, a new program, a new test).

Honest scope: KernelTry implements the *concepts* of modern kernels (x86-64 long mode, SMP, APIC, UEFI, SMEP/SMAP,
XSAVE, TCP) in simplified form. It is not POSIX-compatible and has no `fork`/`exec`, `mmap` or virtual file system.

## First release (1.0)
A **reliable, text-based x86-64 operating system** with
- a shell (`/bin/sh`: pipes, redirection, scripts, control flow, jobs),
- a persistent file system (FAT32 on a SATA disk),
- isolated user programs (ring 3, one address space per process),
- multitasking (preemptive SMP scheduler, threads, signals),
- networking (e1000, IPv4, UDP, TCP, DHCP, DNS, HTTP client and server).

All of these exist as of stage 7 (M38). Release 1.0 is therefore about **reliability, documentation and the
experience of a first-time reader**, not about new features. Stage 8 in `docs/PROGRESS.md` turns this into milestones.

### Release criteria (definition of done)
| Area | Criterion |
|---|---|
| Entry point | `README.md` explains what KernelTry is, shows a screenshot, lists host packages and gets a new reader from clone to a running shell |
| Legal | a license file exists (user decision) |
| Reproducibility | the Rust toolchain is pinned; a dependency check names missing host tools; `make check` passes on a fresh Fedora and Ubuntu host |
| Reliability | `make check` passes N times in a row (N agreed in stage 8) with 1, 2 and 4 CPUs and with `KERNEL_CPU=qemu64`; a soak test runs for a fixed time without failure; every syscall rejects invalid arguments without crashing the kernel |
| Known limits | every open item is either fixed or listed as a documented limitation |
| Understanding | at least three end-to-end walkthroughs with file references |
| Experimenting | at least three guided experiments with an expected result, and guides for adding a syscall, a program and a test |

## Not goals of the first version
- replacing Linux or being POSIX-compatible,
- a full desktop / graphical user interface,
- supporting as much real hardware as possible (QEMU is the reference machine),
- performance competitive with production kernels.

## Working rules that follow from the goals
- New features must come with a test and a `LEARN.md` section – an unexplained feature does not serve the purpose.
- Prefer clear code over clever code; when a simplification is made, say so in `DESIGN.md`.
- Third-party crates stay limited to `limine` and `spin` (from scratch is part of the point).
