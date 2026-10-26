# The course: build a kernel, chapter by chapter

In this course you write an x86-64 kernel in Rust from nothing, the same way YAK was built: one small, working
step at a time. Every chapter ends with a kernel that boots in QEMU and passes its own tests.

## How it works
- Each chapter has **a page here** (what we build, why, the code with explanations) and **a folder in the
  repository**, [`course/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course): `course/01-boot/`,
  `course/02-tests/`, … Each folder is a complete project – the state of the kernel at the end of that chapter.
- **Follow along:** start with an empty directory and type the code from the chapter pages. When you are stuck,
  compare with the chapter's folder.
- **Or jump in anywhere:** copy a chapter folder and continue from there. Every folder builds on its own.
- In every folder, `make run` boots the kernel in a QEMU window and `make test` boots it headless and checks its
  output – the same check the project's CI runs on every chapter.
- The finished kernel of the project (the "reference kernel") is much bigger than the course so far; the
  [walkthroughs](../walkthrough/README.md) explain it as it is today.

## What you need
- The host tools from the [quick start](../../README.md#quick-start) (QEMU, xorriso, make, gcc, git, python3) and
  Rust via rustup. Each chapter folder contains a `rust-toolchain.toml`; rustup installs the exact compiler version.
- Some Rust: ownership, `struct`/`impl`, traits, modules. No previous kernel or assembly knowledge – the chapters
  explain what the hardware expects.

## Chapters

| # | Chapter | You end up with |
|---|---|---|
| 1 | [Booting with Limine](01-boot.md) | a kernel that boots and prints to the serial port |
| 2 | [Testing without an operating system](02-tests.md) | `make test` running unit tests inside the kernel |
| 3 | [Text on the screen](03-console.md) | a framebuffer console with its own font renderer |
| 4 | [GDT and TSS](04-gdt.md) | our own segments and a safe stack for double faults |

The course is being written now; this pilot (chapters 1–4) is followed by a round of feedback before the rest. If
something is unclear or wrong, please open an issue on GitHub – that is exactly the feedback the pilot is for.
