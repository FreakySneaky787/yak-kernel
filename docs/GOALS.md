# Project goals

> **Status:** direction decided by the user on 2026-10-26 (learning platform, see "Direction" and `docs/ROADMAP.md`);
> the release criteria below are still a proposal. Changes to this file are agreed with the user.

## Name
**YAK – Yet Another Kernel.** (User decision 2026-10-26, replacing the earlier working name KernelTry; it matches
the repository name `yak-kernel`.)

## Purpose
YAK is a self-contained operating system for **studying how a modern operating system kernel is built and works**,
hands-on, and for developing it further. It runs in QEMU (BIOS and UEFI).

## Audience
Primarily **self-learners** and **hobby kernel developers** (user decision 2026-10-26); computer science students and
anyone interested in operating system architecture are welcome too.

## Direction (1–2 years)
YAK becomes a **learning platform**: an English course on its own website in which learners **rebuild the
kernel chapter by chapter**, each chapter a runnable, tested state on its own branch. The kernel in `main` stays the
reference implementation. New kernel features are added only when they carry a chapter. Details: `docs/ROADMAP.md`.

## What makes it different
1. **Understandable documentation** – every subsystem is explained (`docs/LEARN.md`), every design decision has a reason
   (`docs/DESIGN.md`), every bug has a story (`docs/BUGS.md`).
2. **Traceable kernel paths** – walkthroughs that follow one action (a key press, a syscall, a page fault, a pipe)
   through the code, with file references.
3. **Reproducible tests** – one command (`make check`) runs unit tests, test kernels and end-to-end shell sessions under
   BIOS and UEFI; the suite also runs with 1–4 CPUs and without modern CPU features.
4. **Room to experiment** – guided experiments ("change X, expect Y") and short guides for extending the system
   (a new syscall, a new program, a new test).

Honest scope: YAK implements the *concepts* of modern kernels (x86-64 long mode, SMP, APIC, UEFI, SMEP/SMAP,
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
| Entry point | `README.md` explains what YAK is, shows a screenshot, lists host packages and gets a new reader from clone to a running shell |
| Legal | `LICENSE` (GNU GPL v3 or later, chosen by the user on 2026-10-26, replacing ECPL v1.2) applies to the whole repository except third-party assets |
| Reproducibility | the Rust toolchain is pinned; a dependency check names missing host tools; `make check` passes on a fresh Fedora and Ubuntu host (checked by CI on every push, M47) |
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
