# 4 – GDT and TSS

**Goal:** our own **Global Descriptor Table** and **Task State Segment** – the CPU structures every x86-64 kernel
needs before it can handle exceptions safely.
**You end up with:** the kernel runs on segments we defined, and a separate, known-good stack is ready for the day
the normal stack breaks (used in the next chapter).
**Code:** [`course/04-gdt/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/04-gdt) (chapter 3 plus
`gdt.rs`)
**What changed:** `diff -ru course/03-console course/04-gdt` (in the repository) shows every line this chapter adds.

## Background: segments, then and now
On the 8086, memory was addressed as *segment:offset*. Later CPUs turned segments into entries of a table – the
**GDT** – with a base address, a size limit and permissions. In 64-bit mode almost all of that is switched off: base
and limit are ignored, memory protection is done by page tables. What is left:

- the **code segment** (register CS) still tells the CPU two things: *this is 64-bit code* (the L bit) and *this is
  the kernel* (privilege level 0 – user programs will run at level 3);
- a **data segment** for the stack segment register SS;
- the **TSS**, which is no longer about "tasks" but holds **stacks the CPU switches to by itself** – for
  interrupts from user programs (a later chapter) and for chosen exceptions: the **Interrupt Stack Table**.

Limine already loaded a GDT before it started us (`cs` was `0x28`, `ss` was `0x30` when we measured). We replace it
because it lives in memory that belongs to the bootloader, because it has no TSS – and because we want to know
exactly what our kernel runs on.

## Step 1: the descriptors
A descriptor is 8 bytes with the bits spread over the word for historic reasons. For a 64-bit kernel only a few
matter, so the two we need are constants:

```rust,file=kernel/src/gdt.rs
const KERNEL_CODE: u64 = 0x00AF_9A00_0000_FFFF; // P | DPL0 | S | E | RW, L = 1, G = 1
const KERNEL_DATA: u64 = 0x00CF_9200_0000_FFFF; // P | DPL0 | S | RW, D/B = 1, G = 1
```

Read `0x9A` as: *present*, privilege level 0, a code/data (not system) segment, executable, readable. The flags
nibble after it, `0xA`, sets **G** (limit counted in 4 KiB pages – irrelevant in 64-bit mode) and **L** – 64-bit code. Entry 0 must be the **null descriptor**; a selector is simply the byte offset
of an entry, so our segments are `0x08` (code) and `0x10` (data).

## Step 2: the TSS
The 64-bit TSS is a 104-byte structure; Rust must lay it out exactly as the CPU expects:

```rust,file=kernel/src/gdt.rs
/// The 64-bit TSS (Intel SDM Vol. 3A, section 8.7): 104 bytes, some of its fields not 8-byte aligned.
#[repr(C, packed(4))]
pub struct TaskStateSegment {
```

`repr(C)` keeps the field order, `packed(4)` stops Rust from inserting padding before the 8-byte fields that start
at offset 4. A compile-time assertion makes sure we got it right:

```rust,file=kernel/src/gdt.rs
const _: () = assert!(size_of::<TaskStateSegment>() == 104);
```

The interesting field is `interrupt_stacks` – seven stack pointers, IST1 to IST7. We put the top of a 20 KiB
**double fault stack** into IST1. Why a separate stack? Imagine the kernel's stack overflows: the CPU tries to push
the exception frame for the resulting page fault – onto the broken stack – and fails again. That second failure is
a *double fault*. If the double fault handler also used the broken stack, a third failure would follow: a **triple
fault**, and the machine resets. With IST1 the CPU switches to a fresh stack *before* pushing anything. (The handler
itself comes in chapter 5; today we only prepare the stack.)

## Step 3: a TSS descriptor
The GDT points to the TSS with a **system descriptor** of 16 bytes – two entries – with the 64-bit address cut into
four pieces:

```rust,file=kernel/src/gdt.rs
fn tss_descriptor(base: u64) -> (u64, u64) {
    let limit = (size_of::<TaskStateSegment>() - 1) as u64;
    let mut low = limit & 0xFFFF;
    low |= (base & 0xFF_FFFF) << 16; // base bits 0..24
    low |= 0x89 << 40; // P = 1, DPL 0, type 0x9 = available 64-bit TSS
    low |= ((limit >> 16) & 0xF) << 48;
    low |= ((base >> 24) & 0xFF) << 56; // base bits 24..32
    (low, base >> 32) // the second entry holds base bits 32..64
}
```

So the table is: null, kernel code, kernel data, TSS low, TSS high – the TSS selector is `0x18`.

## Step 4: loading everything
Three instructions make the CPU use our structures:

1. `lgdt` loads the address and size of the new table. Nothing changes yet – the CPU still uses the old,
   *cached* segments.
2. The segment registers are reloaded. Data segments are easy (`mov ss, …`), but **CS cannot be written with `mov`**.
   The trick is a *far return*: push the new CS and a return address, then `retfq` "returns" to the very next
   instruction – with our code segment:

```rust,file=kernel/src/gdt.rs
        asm!(
            "push {selector}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            selector = in(reg) KERNEL_CODE_SELECTOR as u64,
            tmp = lateout(reg) _,
            options(preserves_flags),
        );
```

3. `ltr` loads the **task register** with the TSS selector.

GDT and TSS must live as long as the kernel – the CPU keeps using them – so they are `static`s, built once at
`init` with `spin::Once`.

## Step 5: calling it
`main.rs` gets `mod gdt;` next to the other modules, and both entry points call `gdt::init()` right after the
console – the normal `kmain` and the test `kmain`, because the tests check the loaded segments:

```rust,file=kernel/src/main.rs
    console::init();
    gdt::init();
    println!("Hello from YAK!");
```

At the end, the normal `kmain` reports what it loaded:

```rust,file=kernel/src/main.rs
    println!(
        "GDT loaded: code segment {:#x}, TSS {:#x}, double fault stack top {:#x}",
        gdt::KERNEL_CODE_SELECTOR,
        gdt::TSS_SELECTOR,
        gdt::double_fault_stack_top()
    );
```

## The tests
`make test` now runs 16 tests; four are new:
- `our_segments_are_loaded` – CS is `0x08`, SS is `0x10`, the task register is `0x18`;
- `gdtr_points_to_our_table` – `sgdt` (the opposite of `lgdt`) returns our table's address and size;
- `tss_descriptor_scatters_the_base` – the bit puzzle of step 3 with a known address;
- `the_double_fault_stack_is_in_the_tss` – IST1 is the top of our stack, 16-byte aligned.

Without the call to `gdt::init()` in the test `kmain`, the GDT test fails – try it.

## Run it
```sh
cd course/04-gdt
make run
```
```text
GDT loaded: code segment 0x8, TSS 0x18, double fault stack top 0xffffffff8…
```
(the exact address depends on where the linker put the stack.)

## What can go wrong
- **The machine resets right after `lgdt`:** a wrong descriptor – most often the code segment without the L bit.
  The CPU then believes it runs 32-bit code and the next instruction is garbage. `-d int,cpu_reset` on the QEMU
  command line shows the exception that started it.
- **General protection fault at `ltr`:** the TSS descriptor is wrong (type not `0x9`, or not present), or `ltr`
  was run twice – a loaded TSS is marked *busy*, and loading a busy TSS faults.
- **The TSS is 112 bytes instead of 104:** `repr(C)` alone adds padding; the assertion catches it at compile time.
- **A double fault still resets the machine (chapter 5):** an IST stack pointer to the *bottom* instead of the
  top of the stack – stacks grow downwards.

## Experiments
1. Remove the L bit from `KERNEL_CODE` (`0x00AF…` → `0x008F…`). What happens, and what does
   `-d int,cpu_reset -D qemu.log` say?
2. Call `gdt::init()` twice. Which instruction fails the second time, and why?
3. Print Limine's selectors before `gdt::init()` (`mov {0:x}, cs`). What does Limine's GDT probably contain in
   entries 1–4?
