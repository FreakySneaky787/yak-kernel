# 14 – The other CPUs

**Goal:** wake the other cores of the machine and bring them into our kernel – through 16-bit real mode, 32-bit
protected mode and 64-bit long mode, the whole history of the x86 in one page of start code.
**You end up with:** a trampoline below 1 MiB, the INIT/STARTUP sequence sent through the local APIC, every CPU
with its own stack, GDT and TSS and the shared IDT, and `run_on(cpu, job)` that runs a closure on another CPU.
QEMU now has 4 CPUs (`KERNEL_SMP` changes it).
**Code:** [`course/14-smp/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/14-smp) (chapter 13
plus `smp.rs`, inter-processor interrupts in `lapic.rs`, a GDT and TSS per CPU in `gdt.rs`, the MADT's APIC IDs)
**What changed:** `diff -ru course/13-threads course/14-smp` (in the repository) shows every line this chapter adds.

## Background: one CPU starts, the others wait
After power-on only one core runs, the *bootstrap processor* (BSP). Limine handed it to `kmain`, and everything so
far ran on it. The others, the *application processors* (APs), sit in a waiting state until their local APIC
receives two messages from another CPU – *inter-processor interrupts* (IPIs):

- **INIT** resets the core and lets it wait for the next one.
- **STARTUP** (SIPI) carries a page number `v`. The core starts executing at physical address `v << 12` – in
  16-bit **real mode**, exactly like a PC of 1981: no paging, 1 MiB of address space, segments times 16.

So the start code must lie in the first megabyte, and it must climb on its own: real mode → **protected mode**
(32 bit, a GDT, CR0.PE) → **long mode** (PAE paging, the EFER.LME bit, CR0.PG) → a jump into Rust. This code is
called the **trampoline**. The MADT of chapter 12 already lists the CPUs; this chapter keeps their APIC IDs, not
just their number.

What does a started AP share with the BSP, and what is its own? Shared: memory and the page tables (one CR3 for
all – that is what makes it one machine), the kernel's code, the IDT. Its own: registers, stack, local APIC – and a
TSS, as step 3 shows.

The scheduler of chapter 13 stays on the BSP in this chapter. The APs wait in `hlt` until `run_on` gives them a job;
one scheduler for all CPUs comes next.

## Step 1: a frame below 1 MiB
The frame allocator of chapter 8 hands out the lowest free frame first, and the usable frames below 1 MiB are few.
So `smp::reserve_frame` takes one right after `mem::init`, before anything else can:

```rust,file=kernel/src/smp.rs
pub fn reserve_frame() {
    let frame = pmm::alloc_frame().expect("a free frame");
    assert!(frame.0 < MIB, "no free frame below 1 MiB for the trampoline");
```

Under QEMU it is `0x62000`, right after the frame bitmap.

## Step 2: the trampoline
The trampoline is assembly in `global_asm!`, assembled into the kernel at its high address and *copied* into the
low frame before use. It cannot know where it runs until it does, so it computes its own physical address from CS
(`ebx = CS * 16`) and reads its data relative to it. The assembler cannot use the difference of two labels as a
memory operand, so the few instructions that read the trampoline's data are written as bytes – for example
`lgdt [ap_gdt_pointer]`:

```rust,file=kernel/src/smp.rs
    // lgdt [ap_gdt_pointer]: 0F 01 /2 with a 16-bit displacement
    ".byte 0x0F, 0x01, 0x16",
    ".word ap_gdt_pointer - ap_trampoline",
```

The trampoline has a small GDT of its own (32-bit and 64-bit code and data). It switches on protected mode, PAE,
long mode and no-execute in EFER, and paging with *our* PML4 – the address `smp::start` filled in. Then it loads
a stack, the CPU's number and the address of `ap_main` from its data area and calls it.

One trap remains. The instruction that turns paging on is fetched at a physical address; the next one is fetched
through the page tables – at the same address, now a virtual one. Our page tables map the higher half, not
`0x62000`. So for the start, the trampoline's frame is also mapped at its own address:

```rust,file=kernel/src/smp.rs
    paging::map(frame.0, frame, Flags::CODE).expect("the trampoline's address is free in the lower half");
```

`Flags::CODE` has no no-execute bit – the AP runs code there. After the last AP is up, the mapping goes again.

## Step 3: INIT, STARTUP, and a TSS per CPU
An IPI is sent through the local APIC's *interrupt command register*: the destination's APIC ID into its high half,
then the kind of message into its low half – writing the low half sends it. Intel's recipe for one AP:

```rust,file=kernel/src/smp.rs
        lapic::send_init(apic_id);
        time::sleep_ms(10);
        lapic::send_startup(apic_id, page);
        if !wait_online(cpu, 10) {
            lapic::send_startup(apic_id, page);
```

The APs start one after another: they all run through the same trampoline, whose stack and CPU number are filled
in anew for each.

In `ap_main` every AP loads a GDT and a TSS of its own, the shared IDT, and switches its local APIC on. Why not the
BSP's GDT? Loading a TSS with `ltr` marks it *busy* in its GDT descriptor, and the CPU refuses to load a busy TSS –
the second CPU would fail. Each also needs its own double fault stack: two CPUs on one IST stack would overwrite
each other's frames. `gdt::init_other_cpu` builds both on the heap.

## Step 4: jobs for the other CPUs
An AP then waits in `hlt` for a job. Each CPU has a mailbox – a `Mutex<Option<Job>>`. `run_on(cpu, job)` puts a
closure into it and sends a *wake-up IPI* (vector `0xF0`, whose handler only ends it), and the AP takes the job,
runs it and puts the result where `run_on` waits for it. The AP checks its mailbox with interrupts off and
sleeps with `sti; hlt`: `sti` takes effect only after the next instruction, so a wake-up that comes right after the
check still ends the `hlt`.

Two old rules now really matter. The heap and the console are used by several CPUs at once – their spin locks
work across CPUs, which is what spin locks are for. And "interrupts off" no longer means "nobody else runs":
on one CPU it did, which is all chapter 13's scheduler relies on. That is why it stays on the BSP for now, and why
jobs on the APs must not touch it.

What the reference kernel ([`kernel/src/arch/x86_64/smp.rs`](https://github.com/FreakySneaky787/yak-kernel/blob/main/kernel/src/arch/x86_64/smp.rs))
does besides: up to 16 CPUs, an idle thread per CPU that the scheduler uses, a local APIC timer on every CPU,
SSE/FPU and syscalls switched on per CPU, and TLB shootdowns – when one CPU changes a mapping, the others must drop
their cached copy of it.

## The tests
`make test` now runs 96 tests in the kernel; four are new:
- `every_cpu_of_the_madt_runs` – as many CPUs online as the MADT lists;
- `a_job_runs_on_the_cpu_it_was_sent_to` – each CPU answers with its own number and APIC ID, has the TSS loaded,
  and a GDT at an address of its own; there is no CPU 8;
- `the_heap_works_on_every_cpu` – a `Vec` built and summed on each CPU;
- `the_trampoline_is_below_1_mib_and_unmapped_again`.

`qemus_tables_describe_qemus_machine` now wants the APIC IDs 0, 1, … for as many CPUs as there are. The test kernel
passed with `KERNEL_SMP=1`, `2` and `4`, and 30 boots of the normal kernel did not hang once.

On the way, this chapter found a bug in chapter 13: about one boot in 20 hung after the `Keyboard:` line. The
scheduler was installed in steps with interrupts on, and a tick in between lost `main` ([bug #28](../BUGS.md));
chapter 13 is fixed too.

## Run it
```sh
cd course/14-smp
make run
```
After the thread lines of chapter 13:

```text
SMP: 4 CPUs run; the others started at the trampoline at physical 0x62000
SMP: a job on each CPU asked it (number, APIC ID): [(0, 0), (1, 1), (2, 2), (3, 3)]
```
`KERNEL_SMP=2 make run` starts QEMU with two CPUs.

## What can go wrong
Two of these mistakes end the whole machine, not just one CPU: a triple fault on any CPU resets them all, and
with `-no-reboot` QEMU quits – `run-qemu.sh` says `QEMU ended by itself: the machine reset`. QEMU's own log
(`-d int,cpu_reset -D log.txt` on its command line) shows what happened:
- **No identity mapping:** a page fault at `0x6205b` with error code `0x10` – an instruction fetch – right after the
  `mov cr0` that turns paging on. The next instruction's address is not mapped.
- **The BSP's GDT on the APs:** a general protection fault with error code `0x18` – the TSS selector: `ltr`
  refuses the TSS the BSP already marked busy. The AP has no IDT of ours yet, so the exception cannot be handled,
  and the next faults end in the triple fault.
- **The AP's local APIC left off:** nothing resets, but the boot stops after the first `SMP:` line. The wake-up
  IPI is never accepted, the AP sleeps on, and `run_on` waits for ever.

## Experiments
1. Start QEMU with `-smp 8`, and with `KERNEL_SMP=9` – what does `start` do with the ninth CPU?
2. Leave out the second STARTUP. Does QEMU need it? (Real hardware sometimes does.)
3. Time how long the start of the APs takes. Where do the milliseconds go?
4. Give `run_on` a job that never ends. What does the CPU that sent it do now – and the shell?

The CPUs run, but only the BSP runs threads. The next chapter gives all of them to one scheduler – and with that,
every lock and every "interrupts off" has to be thought through again.
