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
| 5 | [Interrupts and exceptions](05-interrupts.md) | exception handlers – a page fault reports itself, a double fault survives a broken stack |
| 6 | [Hardware interrupts and the timer](06-timer.md) | the PIC moved to vector 32, a 100 Hz timer, `sleep_ms` that waits in `hlt` |
| 7 | [The keyboard](07-keyboard.md) | keys from the PS/2 keyboard on the screen – IRQ 1, a ring buffer, a scancode decoder; the end of part I |
| 8 | [Physical memory](08-frames.md) | Limine's memory map and a bitmap frame allocator, reached through the higher-half direct map |

The course is being written now; the pilot (chapters 1–4) gets a round of feedback from readers, and the chapters
after it change with what they report. If you get stuck, use the issue form
[**I got stuck in a course chapter**](https://github.com/FreakySneaky787/yak-kernel/issues/new?template=course-stuck.yml);
if something was unclear even though it worked, use
[**Feedback on the course**](https://github.com/FreakySneaky787/yak-kernel/issues/new?template=course-feedback.yml).
That is exactly the feedback the pilot is for – [CONTRIBUTING.md](../../CONTRIBUTING.md) says more.
