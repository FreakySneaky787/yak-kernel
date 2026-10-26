# Changelog

## 1.0.0-rc.1 – pre-release (2026-10-26)
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

## 0.1.0 – stages 1–7 (2026-10-24 … 2026-10-25)
Milestones M1–M38, see `docs/PROGRESS.md`: boot via Limine (BIOS/UEFI), own GDT/IDT/paging/allocators, APIC and SMP
scheduler, AHCI + FAT32, ring 3 with ELF loader and syscalls, `/bin/sh`, pipes, signals, threads, editor, FPU/AVX state,
SMEP/SMAP, e1000 + IPv4/UDP/TCP/DHCP/DNS/HTTP.
