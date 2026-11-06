# 20 – Ring 3 and the first system call

**Goal:** run code that may *not* do everything – in ring 3, the CPU's lowest privilege level – and give it one
controlled door back into the kernel: a system call.
**You end up with:** user segments in the GDT, a TSS whose RSP0 follows the running thread, pages with the USER bit,
a jump to ring 3 with `iretq`, `int 0x80` with two system calls (`write`, `exit`), and faults in ring 3 that end the
program instead of the kernel. The start of part VI – and, for now, the last chapter of the course.
**Code:** [`course/20-ring3/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/20-ring3)
(chapter 19 plus `user.rs`; user segments and RSP0 in `gdt.rs`, a DPL 3 gate in `idt.rs`, the USER bit in
`paging.rs`, faults from ring 3 in `interrupts.rs`, one line in `task.rs`)
**What changed:** `diff -ru course/19-disk-irq course/20-ring3` (in the repository) shows every line this chapter
adds.

## Background: four rings, two used
x86 has four privilege levels, rings 0 to 3; every operating system in use today uses two. The kernel runs in ring 0
and may do everything. Code in ring 3 may not:

- run *privileged instructions* – `cli`, `hlt`, loading CR3, the GDT or the IDT, port I/O: a general protection
  fault (#GP);
- touch a page whose page table entries lack the USER bit: a page fault (#PF);
- raise an interrupt vector with `int` unless the IDT gate says it may.

The CPU checks all of that on every instruction, so a program can only hurt itself – *if* the kernel turns its
faults into "this program ends" instead of a panic. And the program needs a way to ask the kernel for things it may
not do itself: the *system call*.

The current privilege level is the low 2 bits of CS. So "going to ring 3" means loading a CS whose descriptor has
DPL 3 – and the way to lower the privilege is a *return*: `iretq`, a far return or `sysretq`. Going *up* is
what interrupts and exceptions do: the CPU switches to ring 0 and – because the ring 3 stack cannot be trusted –
to a stack it takes from the TSS: **RSP0**.

## Step 1: segments for ring 3
Two more descriptors in the GDT: the kernel's code and data descriptors with DPL 3 (`gdt.rs`):

```rust,file=kernel/src/gdt.rs
const USER_DATA: u64 = 0x00CF_F200_0000_FFFF; // P | DPL3 | S | RW
const USER_CODE: u64 = 0x00AF_FA00_0000_FFFF; // P | DPL3 | S | E | RW, L = 1
```

They go to 0x28 and 0x30; with the requested privilege level 3 in the low bits, ring 3 uses the selectors 0x2B and
0x33. The GDT now has 7 entries; chapter 4's test of its size expects 7 now.

## Step 2: RSP0 – a kernel stack for every thread
When the timer interrupts a program, the CPU must push its frame somewhere in the kernel – onto RSP0 of the TSS of
the CPU it runs on. That stack must belong to the thread that was interrupted: if it is preempted there, its kernel
stack must survive while other threads run, and when it comes back, perhaps on another CPU, it needs the same stack.
So the scheduler sets RSP0 of its CPU at every switch, to the stack of the thread it switches to:

```rust,file=kernel/src/task.rs
    // an interrupt or a system call from ring 3 must land on this thread's own stack (chapter 20)
    if let Some(stack) = &thread.stack {
        crate::gdt::set_kernel_stack(stack.top);
    }
```

Until now the TSS was written once; now the kernel changes it while the CPU uses it. `gdt.rs` keeps each CPU's TSS in
an `UnsafeCell` (Rust's way of saying "changed through a shared reference") and a pointer to it per CPU:

```rust,file=kernel/src/gdt.rs
pub fn set_kernel_stack(top: u64) {
    let tss = TSS_OF[crate::smp::cpu()].load(Ordering::Relaxed);
    if tss.is_null() {
        return; // a CPU that has no TSS yet runs no thread from ring 3 either
    }
    unsafe {
        let mut stacks = (*tss).privilege_stacks;
        stacks[0] = top;
        (*tss).privilege_stacks = stacks;
    }
}
```

(The copy in and out is because the TSS is a packed struct: Rust allows no reference to its unaligned fields.)

## Step 3: pages a program may use
Bit 2 of a page table entry, U/S, is the USER bit. A program may use a page only if the bit is set in *every* entry on
the way – PML4, PDPT, PD and the page table. Chapter 9's `map` made new tables with PRESENT and WRITABLE only, so it
learns to pass USER on:

```rust,file=kernel/src/paging.rs
        // ring 3 may use a page only if every table on the way says USER too (chapter 20)
        let user = flags.0 & Flags::USER.0;
        if !entry.is(Flags::PRESENT) {
            // a new, empty table: zeroed, or its garbage would be read as entries
            let new = pmm::alloc_frame_zeroed().ok_or(MapError::OutOfFrames)?;
            // the tables above allow everything; the last entry decides what the page may do
            *entry = Entry::new(new, Flags(Flags::PRESENT.0 | Flags::WRITABLE.0 | user));
        } else if entry.is(Flags::HUGE) {
            return Err(MapError::HugePageInTheWay);
        } else {
            entry.0 |= user;
        }
```

`user.rs` maps three pages in the empty lower half, once: code at 0x40_0000 (USER, *not* writable), data at
0x40_1000 and a stack page below 0x80_0000 (USER, writable, not executable). The kernel fills them through the HHDM,
so the code page can stay read-only for the program. They stay mapped for good: a page unmapped on one CPU can still
be in the TLB of another, which would go on using a frame that may already be someone else's.

There is one address space – the kernel's – and these three pages are in it, so one program runs at a time. Giving
each program its own page tables, and loading programs from the disk, is what chapter 21 of the plan would do.

## Step 4: into ring 3
`iretq` takes RIP, CS, RFLAGS, RSP and SS from the stack, as if it returned from an interrupt that came from ring 3.
So we push what we want to find there:

```rust,file=kernel/src/user.rs
fn enter(rip: u64, stack_top: u64) -> ! {
    unsafe {
        asm!(
            "push {ss}",
            "push {sp}",
            "push {flags}",
            "push {cs}",
            "push {ip}",
            "xor eax, eax", "xor ebx, ebx", "xor ecx, ecx", "xor edx, edx",
            "xor esi, esi", "xor edi, edi", "xor ebp, ebp",
            "xor r8d, r8d", "xor r9d, r9d", "xor r10d, r10d", "xor r11d, r11d",
            "xor r12d, r12d", "xor r13d, r13d", "xor r14d, r14d", "xor r15d, r15d",
            "iretq",
            ss = in(reg) USER_DATA_SELECTOR as u64,
            sp = in(reg) stack_top,
            flags = in(reg) 0x202u64,
            cs = in(reg) USER_CODE_SELECTOR as u64,
            ip = in(reg) rip,
            options(noreturn)
        )
    }
}
```

RFLAGS 0x202 has the interrupt flag set: the timer can take the CPU away from the program. The registers are
cleared first – they hold kernel values, addresses among them, which are none of the program's business.

`user::run` copies a program into the code page, starts a thread whose entry is `enter`, and joins it. A program is
a thread that happens to run in ring 3.

## Step 5: the system call
The door back is `int 0x80`, the vector Linux used on 32-bit x86. Its IDT gate gets DPL 3 (`set_user_callable` in
`idt.rs`); every other gate keeps DPL 0, so a program cannot fake a timer tick or a page fault with `int`.

The handler needs the registers as the program left them – the call's number in RAX, the arguments in RDI and RSI –
and an `extern "x86-interrupt"` function does not get them. So the entry is a few lines of assembly: save the
registers a Rust function may change, pass their address, restore them (RAX now holding the result), `iretq`:

```rust,file=kernel/src/user.rs
    ".global syscall_entry",
    "syscall_entry:",
    "push rax", "push rcx", "push rdx", "push rsi", "push rdi", "push r8", "push r9", "push r10", "push r11",
    "mov rdi, rsp",
    "call {dispatch}",
    "pop r11", "pop r10", "pop r9", "pop r8", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rax",
    "iretq",
    dispatch = sym syscall,
```

The CPU pushed 5 values, we push 9: 14 × 8 bytes, so the stack is 16-byte aligned at the `call`, as the ABI wants.
Then the call itself:

```rust,file=kernel/src/user.rs
extern "C" fn syscall(registers: &mut Registers) {
    debug_assert_eq!(registers.cs & 3, 3, "only ring 3 calls int 0x80");
    registers.rax = match registers.rax {
        SYS_WRITE => write(registers.rdi, registers.rsi),
        SYS_EXIT => end(Exit::Code(registers.rdi)),
        _ => NO_SUCH_CALL,
    };
}
```

`exit` ends the thread from inside the handler – its stack is its own, nobody returns there. `write` prints bytes the
program names by address and length – and here is the rule every system call lives by: **an address from a program
is never trusted.** It could point into the kernel, past the end of the lower half, or wrap around:

```rust,file=kernel/src/user.rs
fn user_bytes(address: u64, length: u64) -> Option<&'static [u8]> {
    let end = address.checked_add(length)?;
    if end > USER_END {
        return None;
    }
    let mut page = align_down(address, PAGE_SIZE);
    while page < end {
        if !paging::flags(page)?.contains(Flags::USER) {
            return None;
        }
        page += PAGE_SIZE;
    }
    Some(unsafe { core::slice::from_raw_parts(address as *const u8, length as usize) })
}
```

A bad address gets −14 back (Linux's EFAULT), an unknown call −38 (ENOSYS).

The first program, assembled into the kernel and copied to 0x40_0000 – it only uses addresses relative to RIP, so it
runs wherever it is put:

```rust,file=kernel/src/user.rs
    "user_hello:",
    "mov eax, 1",
    "lea rdi, [rip + 2f]",
    "mov esi, 19",
    "int 0x80",
    "mov eax, 2",
    "mov edi, 42",
    "int 0x80",
    "ud2",
```

## Step 6: a fault in ring 3 is the program's
The handlers of #GP, #PF, #UD and #DE look at the CS the CPU saved. If it says ring 3, the program ends and the
kernel goes on:

```rust,file=kernel/src/interrupts.rs
fn end_user_program_on_fault(frame: &InterruptStackFrame, what: &'static str) {
    if frame.cs & 3 == 3 {
        crate::println!("Ring 3: {} at {:#x} - the program is ended", what, frame.rip);
        user::end(user::Exit::Killed(what));
    }
}
```

`main.rs` declares `mod user;` and `kmain` calls `show_ring3()` after `show_disk()`: the hello program, then one that
tries `cli`.

## The tests
`make test` runs 145 tests in the kernel; new:
- `the_user_segments_have_privilege_level_3` (`gdt.rs`);
- `a_program_writes_and_exits` (the output, exit code 42), `a_program_has_a_stack` (`push 5; pop rdi`);
- `privileged_instructions_end_the_program` (`cli`), `kernel_memory_is_out_of_reach` (reading a kernel address, and
  writing into its own read-only code page: page faults);
- `system_calls_check_what_they_get` (a kernel pointer → −14, call 99 → −38, and `user_bytes` at the edges: past the
  lower half, wrapping around, an unmapped page);
- `the_timer_takes_the_cpu_away_from_ring_3` – a program that spins until a byte in its data page changes; the
  kernel sleeps 50 ms meanwhile, then changes the byte. It passes with `KERNEL_SMP=1` too, where only the timer can
  get the one CPU back from ring 3;
- `each_cpu_switches_to_the_thread_s_own_kernel_stack` – RSP0 of the CPU is just above the running thread's stack
  pointer.

The IDT test of chapter 5 knows vector 0x80 now. Three counter-checks, one run each:
- the gate without DPL 3: `int 0x80` itself is a general protection fault – every program is ended at its first call;
- USER only on the page, not on the tables above: a page fault at 0x400000, the program's first instruction;
- no `set_kernel_stack` in the scheduler: RSP0 is 0, the first interrupt from ring 3 pushes to address −8 – a page
  fault the CPU cannot deliver either: a double fault ("last page fault at 0xfffffffffffffff8").

## Run it
```sh
cd course/20-ring3
make run
```
After the disk lines:

```text
Hello from ring 3!
Ring 3: the program ended with Code(42)
Ring 3: general protection fault at 0x400000 - the program is ended
Ring 3: the program that tried `cli` ended with Killed("general protection fault") - and the kernel goes on
```

## What can go wrong
- **RSP0 left alone:** a double fault at the first interrupt from ring 3 (the counter-check above) – or, worse, RSP0
  of another thread: two threads on one kernel stack, a crash far from its cause.
- **USER missing above the page:** the page looks right and faults anyway. The CPU combines the bits of all four
  levels.
- **A gate with DPL 3 too many:** a program could raise any vector – the timer's, a page fault's with a forged frame.
  Only 0x80 is open.
- **Trusting an address:** a `write` that printed whatever address it was given would print the kernel's memory to
  anyone who asks – the counter-example `user_bad_pointer` asks exactly that.
- **Kernel values in registers:** without the `xor`s, the program starts with kernel addresses in its registers –
  a gift for an attacker looking for where the kernel is.
- **`syscall` instead of `int 0x80`:** faster, and what the reference kernel uses – but `syscall` does *not* switch
  the stack. The kernel must do it itself, with a per-CPU area reached through `swapgs`, and `sysret` to a
  non-canonical address faults in ring 0. That is a chapter of its own; `int 0x80` keeps this one to ring 3.

## Experiments
1. Add a system call `getcpu` that returns the number of the CPU the program runs on. Run a program that spins and
   calls it now and then: does it move between CPUs?
2. Give the program a second stack page, but forget the USER bit on it. Where exactly does it fail?
3. Let `write` accept at most 256 bytes. Why might a kernel want such a limit?
4. Look at the reference kernel's `kernel/src/arch/x86_64/syscall.rs`: what does its entry do before it can push
   anything?

## The end, for now
This is the last chapter I have written for now (my decision of 2026-11-06, in `docs/ROADMAP.md`). The kernel boots,
handles interrupts, manages memory, runs threads on every CPU, reads and writes a FAT32 disk, and runs code in ring 3
that can only ask. The reference kernel goes on from here – own address spaces, programs loaded from the disk,
processes, pipes, signals, a network stack – and its [walkthroughs](../walkthrough/README.md) explain how.
