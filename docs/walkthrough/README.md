# Walkthroughs

Each walkthrough follows **one thing that happens** through the code, step by step, with links to the functions
involved. Read them with the code open next to them.

| Walkthrough | Follows | Touches |
|---|---|---|
| [01 – From power-on to `/bin/sh`](01-boot.md) | the boot | Limine, GDT/IDT, memory, interrupts, scheduler, disk, first process, ring 3 |
| [02 – A key press reaches a program](02-keypress.md) | one key | IRQ, ring buffer, scancode decoder, blocking wait, `read` syscall |
| [03 – `cat /README.txt \| wc`](03-pipeline.md) | a pipeline | shell, `pipe`/`dup2`/`spawn`, FAT32, pipe buffer, scheduler, exit and `wait` |
| [04 – A page fault in a program](04-page-fault.md) | one bad memory access | exception, demand paging, killing a process, kernel-mode faults |

**How the links are kept honest:** a link written as \[`name`\](path) promises that `name` appears in that file.
`scripts/check-doc-refs.py` (part of `make check`) verifies every such link in `docs/` and `README.md`, so a rename in
the code breaks the build until the walkthrough is updated. Line numbers are not used – they change too often.
