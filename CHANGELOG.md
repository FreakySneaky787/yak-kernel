# Changelog

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
