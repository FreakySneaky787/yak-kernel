# Roadmap – from kernel to learning platform

> Decided by me (FreakySneaky787, the author of YAK) on 2026-10-26. Phases are worked off as stages in `docs/PROGRESS.md`.

## Context
YAK 0.8.0 was published as a pre-release: a working x86-64 kernel (M1–M46) with tests, walkthroughs and
experiments. I decided the direction for the next 1–2 years:
- **What:** a learning platform (not a feature-race hobby OS, not a lab).
- **For whom:** self-learners and hobby kernel developers.
- **How:** learners **rebuild the kernel chapter by chapter** (like blog_os), each chapter a runnable, tested state.
- **Language:** English. **Where:** an own website (GitHub Pages), built by CI.
- **Roles:** I steer, Claude builds.

The existing kernel stays the **reference implementation**; the course is a second, cleanly ordered path to (almost)
the same result.

## Target picture
```
main branch
├── kernel/, user/ …        reference kernel (as today)
├── book/                   mdBook source of the course + existing docs (walkthroughs, experiments, …)
└── .github/workflows/      CI: make check, book build + deploy, chapter checks
course/NN-<slug> branches   one per chapter, each = previous chapter + the chapter's code, own `make check`
website                     https://<user>.github.io/<repo>/ – course, reference docs, links to chapter branches
```
Each chapter page = goal · background (from `docs/LEARN.md`) · step-by-step code with explanation · "run it" ·
tests that prove it · experiments · "what can go wrong" (from `docs/BUGS.md`) · link to the chapter branch and the
diff to the previous one.

## Roadmap

### Phase A – finish 1.0 of the reference kernel (short) – done, 1.0.0 on 2026-10-27
1. **CI** (`.github/workflows/check.yml`): `make check` on Ubuntu and in a Fedora container under QEMU TCG
   – this also closes the open release criterion "fresh Fedora host". Reuse `make deps` for package names.
2. I confirm `docs/GOALS.md` (rewritten for the learning-platform direction, see Phase B).
3. First external reader tries README → walkthrough 01 → one experiment (I organise this); fix what they report.
4. Release 1.0.0 (M47): version, CHANGELOG, `make stress`, I create the tag + GitHub release (tags cannot be pushed
   from the sandbox).

### Phase B – course infrastructure (no chapter content yet)
1. **Website:** mdBook (host tool, installed via `cargo install` in CI – not a kernel crate) in `book/`;
   GitHub Pages deploy workflow. Existing docs (walkthroughs, EXPERIMENTS, HACKING, LEARN, BUGS) move/link into
   the book; `scripts/check-doc-refs.py` keeps checking them.
2. **Chapter format:** template page + rules (one concept per chapter, every code block appears in the chapter
   branch, every claim backed by a test or a recorded run – same honesty rule as `docs/EXPERIMENTS.md`).
3. **Chapter branches:** naming `course/NN-slug`; a script that checks a chapter branch builds and passes its tests
   and that the book's code excerpts match the branch (extension of `check-doc-refs.py`); CI runs it for all chapters.
4. Update `docs/GOALS.md` (layout, workflow for chapters).

### Phase C – pilot: Part I (chapters 1–4), then feedback
Write chapters 1–4 fresh as a clean linear history (not cut out of today's mixed commits):
1 Boot with Limine + serial output · 2 Testing without an OS · 3 Text on the screen · 4 GDT and TSS.
→ I and at least one outside reader go through them; adjust format before writing more.

### Phase D – the remaining course (parts, following the M1–M38 order, which already teaches evolution)
| Part | Chapters (from milestones) |
|---|---|
| I Bare metal | boot+serial, tests, framebuffer console, GDT/TSS, interrupts/exceptions, PIC+timer, keyboard (M1–M7) |
| II Memory | frame allocator, paging + guard pages, heap (M8–M10) |
| III A first shell | kernel shell (M11) |
| IV Concurrency | APIC/ACPI, threads + scheduler, FPU state, SMP start, SMP scheduler (M12, M13, M24–M26) |
| V Storage | PCI + AHCI, FAT32, caching + MSI (M14, M15, M32) |
| VI User space | ring 3 + syscalls + ELF, files, user heap, processes, shell as program, pipes, signals, threads/futexes (M16–M22, M34, M36) |
| VII Networking | e1000 + IPv4/UDP, TCP, DHCP/DNS/HTTP (M27–M30, M38) |
| VIII Hardening | SMEP/SMAP + XSAVE, memory reclaim/demand paging, testing how a kernel fails (M33, M37, M41) |

Roughly 30 chapters; one chapter ≈ one milestone of work. Order of writing = order of reading.

### Phase E – polish and growth
- Exercises per chapter with tests learners can run (`make check` on their branch); solutions = next chapter branch.
- Diagrams (memory layout, interrupt path, scheduler states), glossary, search (mdBook built-in).
- Kernel features only when they carry a chapter (e.g. VFS + `/proc`, `fork`/`exec`, mmap) – feature freeze otherwise.
- Community basics once the pilot works: CONTRIBUTING.md, issue templates for "I got stuck in chapter N".

## Rules that follow from the direction
- The reference kernel and the course must not drift silently: a bug fix in `main` gets a note in the affected
  chapter ("what can go wrong") and, if the chapter code has the bug, a fix in that chapter branch and all later ones.
- Every chapter branch stays green in CI with the pinned toolchain; toolchain updates are done for all branches at once.
- Learners must be able to start any chapter from its branch without reading the reference kernel.

## Risks (honest)
- **Effort:** ~30 chapters is the largest piece of work so far; the pilot (Phase C) exists to find out before
  committing to all of it.
- **Maintenance:** later changes ripple through many branches – keep chapter branches frozen once published, fix
  only real bugs.
- **AI-written teaching text** can sound right and be wrong: every claim needs a test or a recorded run, and outside
  readers are irreplaceable – Claude cannot test "is this understandable?".
- **Nightly Rust** breaks over time: the pinned toolchain must keep working; CI shows when it stops.

## Decisions I still have to make (later, not now)
- Repository rename (`yak-kernel` → YAK) – before the website URL is published.
- Website address (GitHub Pages default vs. own domain).

## How progress is checked
- Phase A: CI green on Ubuntu and Fedora; 1.0.0 released.
- Phase B: website deployed by CI; a sample chapter branch passes the chapter check.
- Phase C: four pilot chapters online, each branch green, feedback from at least one outside reader recorded in
  `docs/PROGRESS.md`.
- Phases D/E: the same checks per chapter; the course is complete when the last chapter branch reaches the feature set
  of 1.0.
