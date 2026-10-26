# 04 – A page fault in a program

Every memory access goes through the page tables. When a translation is missing or forbids the access, the CPU raises
exception 14, a **page fault**. The same exception means three very different things in KernelTry:

| Situation | Result |
|---|---|
| a program touches its heap or stack for the first time | the kernel maps a page and the program never notices |
| a program accesses memory it does not own (`fault null`) | the program is killed, everything else runs on |
| the **kernel** accesses unmapped memory | a kernel bug: panic, the system halts |

**Try it:** `fault null` and `fault kernel` in the shell, `heaptest` (demand paging), and `make test` runs
`kernel/tests/page_fault.rs` (a kernel-mode fault on purpose).

## 1. The CPU's part
On the fault the CPU stores the faulting address in **CR2**, pushes an error code (bit 0: page was present →
protection violation; bit 1: write; bit 2: user mode) and an interrupt frame, and – coming from ring 3 – switches to the
kernel stack from the TSS (`RSP0`, set for every thread in the scheduler). Then it runs the IDT entry for vector 14:
[`page_fault`](../../kernel/src/arch/x86_64/interrupts.rs). [`PageFaultError`](../../kernel/src/arch/x86_64/interrupts.rs)
turns the error code into the readable text you see in messages.

## 2. User mode: maybe it is demand paging
[`from_user`](../../kernel/src/arch/x86_64/interrupts.rs) looks at the saved CS: ring 3? Then
[`handle_page_fault`](../../kernel/src/process/mod.rs) decides:

- the page was **present** (bit 0 set): a protection violation – writing to code, executing data. Not repairable.
- the address is inside the heap (below the `brk` end) or the 1 MiB stack area
  ([`on_demand`](../../kernel/src/process/mod.rs)): [`populate`](../../kernel/src/process/mod.rs) maps a zeroed frame
  ([`page_frame`](../../kernel/src/mem/address_space.rs)) and the handler returns – `iretq` **repeats the faulting
  instruction**, which now succeeds. This is **demand paging** (M33): `brk` only moves a number, memory is spent when
  it is used. A program that asks for 32 MiB and touches 3 pages gets 3 frames.

## 3. User mode: the program dies
Anything else is the program's fault. [`kill_current`](../../kernel/src/process/mod.rs) logs
`process N (name) killed: page fault at 0x0 (page not present, write, user mode), rip=…` on the serial port and calls
[`exit_current`](../../kernel/src/process/mod.rs) with `ExitStatus::Killed(reason)`: other threads of the process are
stopped, descriptors closed, the address space freed. Other processes are untouched – their page tables never mapped
this one's memory.

The parent's `wait` returns the reason; [`number_for_reason`](../../kernel/src/process/signal.rs) reports a CPU
exception as signal 11 (SIGSEGV), so the shell sets `$?` to 139 and prints `error: fault was killed: …`.

`fault kernel` reads `0xffffffff80000000`: the page *is* mapped – the kernel itself lives there – but without the USER
bit. The CPU refuses the access from ring 3 (the message says "protection violation, read, user mode"), which is exactly the protection
that keeps programs out of the kernel.

## 4. Kernel mode: a bug
If the fault happened in ring 0, there is nobody to blame but the kernel: `page_fault` panics with the address, the
cause and the interrupt frame (RIP!). `make addr2line ADDR=<rip>` turns the RIP into a file and line.

Two protections make sure the kernel faults loudly instead of silently misbehaving:
- **SMAP** – the kernel may only touch user memory inside [`user_access`](../../kernel/src/arch/x86_64/cpu.rs); an
  accidental dereference of a user pointer faults (`kernel/tests/smap.rs`).
- **Guard pages** below every kernel stack ([`stack.rs`](../../kernel/src/mem/stack.rs)): a stack overflow hits an
  unmapped page. The handler cannot run on the broken stack, so the CPU escalates to a **double fault**, which has its
  own stack in the TSS (IST1, `kernel/tests/stack_overflow.rs`).

Bug #14 (`docs/BUGS.md`) was a kernel-mode page fault caused from user space: the kernel checked a user buffer, another
thread unmapped it, and the kernel's copy faulted. That is why user memory is only copied in
[`copy_from_user`](../../kernel/src/process/syscall.rs) / [`copy_to_user`](../../kernel/src/process/syscall.rs), with
the check and the copy under one lock.

## Things to try
- `fault null`, `fault kernel`, `fault stack` (stack overflow in user mode: the stack area ends after 1 MiB).
- `heaptest`, then `mem`: memory is only used for touched pages.
- `sigtest` installs a handler – but a fault is not a signal you can catch yet in KernelTry; compare with what Linux does.
