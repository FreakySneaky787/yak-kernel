# 9 – Paging

**Goal:** read the page tables the CPU uses, change them, and use that for the first thing paging buys a kernel: a
stack that cannot overflow silently.
**You end up with:** `translate` (what the CPU does on every memory access, in software), `map` and `unmap`, kernel
stacks with a guard page, and a real stack overflow (`fault=overflow`) that ends in the double fault handler of
chapter 5.
**Code:** [`course/09-paging/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/09-paging) (chapter 8
plus `paging.rs`, `stack.rs`, one more Limine request and a fourth boot check)
**What changed:** `diff -ru course/08-frames course/09-paging` (in the repository) shows every line this chapter adds.

## Background: four levels of tables
A virtual address on x86-64 has 48 meaningful bits (the 16 above them repeat bit 47). The CPU cuts them into five
pieces:

```text
 47      39 38      30 29      21 20      12 11            0
+----------+----------+----------+----------+---------------+
| PML4     | PDPT     | PD       | PT       | offset        |
+----------+----------+----------+----------+---------------+
   9 bits     9 bits     9 bits     9 bits      12 bits
```

Each 9-bit piece picks one of 512 entries in a table. **CR3** holds the physical address of the top table, the
PML4. Its entry names the next table (a PDPT), whose entry names a PD, whose entry names a PT, whose entry names the
frame. The 12-bit offset is the position inside that frame's 4 KiB. Every table is itself one frame of 512 entries ×
8 bytes – which is why chapter 8's allocator comes first: new tables are new frames.

An entry is a physical address plus **flags** in the bits the address does not need: *present* (bit 0), *writable*
(bit 1), *no execute* (bit 63), and a few more. A PD entry can also say *huge* (bit 7): "I map 2 MiB myself, there
is no PT below me" – a PDPT entry the same with 1 GiB. Limine builds the HHDM mostly from 2 MiB pages.

The CPU caches translations in the **TLB**. Changing an entry does not change the TLB: after removing a mapping we
must tell the CPU with `invlpg`.

## Step 1: entries and tables
An entry is a `u64`; the address is bits 12–51, everything else is flags. A table is 512 of them, aligned to 4 KiB.
We reach every table through the HHDM – an entry holds a physical address, and `PhysAddr::as_mut_ptr` from chapter
8 turns it into a pointer:

```rust,file=kernel/src/paging.rs
/// The entry numbers of `virt` in the four tables, PML4 first.
pub fn indices(virt: u64) -> [usize; 4] {
    [39, 30, 21, 12].map(|shift| ((virt >> shift) & 0x1FF) as usize)
}
```

## Step 2: translating by hand
`translate` walks the four levels like the CPU: an entry that is not present ends the walk, a huge entry ends it
early with a bigger page, and the PT entry ends it with a 4 KiB frame:

```rust,file=kernel/src/paging.rs
        let page_size = match level {
            1 if entry.is(Flags::HUGE) => 1 << 30,
            2 if entry.is(Flags::HUGE) => 1 << 21,
            3 => PAGE_SIZE,
            _ => {
                entries = &table(entry.address()).0;
                continue;
            }
        };
```

One trap: in a huge page entry, bit 12 is not part of the address but the **PAT** bit, which picks the caching
type. Limine sets it for the framebuffer. Rounding the address down to the page size removes it:

```rust,file=kernel/src/paging.rs
        let start = entry.address().0 & !(page_size - 1);
```

The reference kernel had exactly this bug until this chapter found it ([bug #26](../BUGS.md)).

A new Limine request, `EXECUTABLE_ADDRESS`, says where the kernel lies physically. It lets a test check `translate`
against an answer that does not come from our own code.

## Step 3: mapping and unmapping
`map` walks the same way, but a missing table on the way is created – a zeroed frame from chapter 8 (a frame with
old contents would be read as 512 random entries):

```rust,file=kernel/src/paging.rs
        if !entry.is(Flags::PRESENT) {
            // a new, empty table: zeroed, or its garbage would be read as entries
            let new = pmm::alloc_frame_zeroed().ok_or(MapError::OutOfFrames)?;
            // the tables above allow everything; the last entry decides what the page may do
            *entry = Entry::new(new, Flags(Flags::PRESENT.0 | Flags::WRITABLE.0));
```

The CPU combines the flags of all four levels: a page is writable only if *every* entry on the way says so. So
the upper entries allow everything and the last one decides. `map` refuses an address inside a huge page
(splitting it is possible, but not needed yet), an address that is mapped already, and anything not aligned to
4 KiB.

`unmap` clears the last entry, flushes the TLB entry, and gives the frame back to the caller:

```rust,file=kernel/src/paging.rs
    entries[i1] = Entry(0);
    // the CPU keeps recent translations in its TLB; without this it could go on using the old one
    unsafe { asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
```

Empty tables are not freed again – a few frames, and much simpler.

## Step 4: a stack with a guard page
Chapter 5 could only *fake* a stack overflow, by pointing the stack at an unmapped address. Now we can build a stack
whose end really is unmapped. `stack::alloc` takes a slice of an area reserved for stacks (PML4 entry 510), leaves
its lowest page unmapped – the **guard page** – and maps the pages above it:

```rust,file=kernel/src/stack.rs
    for page in 1..=pages {
        let frame = pmm::alloc_frame_zeroed().ok_or(MapError::OutOfFrames)?;
        paging::map(guard + page * PAGE_SIZE, frame, Flags::DATA)?;
    }
```

`fault=overflow` switches to such a stack and calls a function that calls itself forever, 512 bytes deeper each
time:

```rust,file=kernel/src/interrupts.rs
    fn deeper(depth: u64) -> u64 {
        let room = core::hint::black_box([depth; 64]); // 512 bytes of stack per call
        deeper(depth + 1) + room[0]
    }
```

After 16 KiB the next push lands in the guard page: a page fault. To report it, the CPU would push the exception
frame on the same stack – into the guard page again – and that is a double fault, which chapter 4 gave its own stack.

## The tests
`make test` now runs 52 tests in the kernel; seven are new:
- `an_address_is_four_indices_and_an_offset` – the kernel's own address is entries 511, 510, 0, 0;
- `the_kernel_is_where_limine_put_it` – `translate` of a function in the kernel gives the physical address Limine
  reported;
- `the_hhdm_maps_every_frame_to_itself` – and the lower half is empty (chapter 8's page fault at `0x62000`);
- `a_mapped_frame_appears_at_its_new_address` – the first page in an empty 512 GiB region costs exactly three new
  tables; what is written at the new address is in the frame; mapping twice and unaligned addresses are refused;
  after `unmap` the address translates to nothing;
- `the_framebuffer_translates_exactly` – the PAT bit of bug #26;
- `huge_pages_are_not_split` – an HHDM address inside a 2 MiB page;
- `the_stack_is_mapped_and_its_guard_page_is_not` – and two stacks do not overlap.

A fourth boot check runs `fault=overflow` and wants the double fault on its own stack, with the last page fault in
the guard page.

## Run it
```sh
cd course/09-paging
make run
```
After the memory lines of chapter 8:

```text
Paging: PML4 at 0x7f7d000; kmain at 0xffffffff800104d0 is physical 0x7de44d0 (the kernel starts at physical 0x7dd4000)
Mapped frame 0x62000 at 0xfffffe0000000000: wrote 0xbeef there, read 0xbeef through the HHDM
Stack: 16 KiB below 0xffffff0000005000, its guard page at 0xffffff0000000000 is mapped: no
```
The exact physical addresses depend on the firmware and on the size of your kernel. And the real overflow:

```sh
KERNEL_CMDLINE=fault=overflow make run
```
```text
running on the double fault stack: yes (rsp=0xffffffff800240d0)
KERNEL PANIC: panicked at src/interrupts.rs:153:5:
EXCEPTION: DOUBLE FAULT (code 0x0), last page fault at 0xffffff0000000be8
rip=0xffffffff800099e7 cs=0x8 rflags=0x286 rsp=0xffffff0000000bf0 ss=0x10
```
The page fault was at `…0be8`, inside the guard page `…0000`–`…0fff`; the stack pointer `rsp` of the interrupted
code was in it too.

## What can go wrong
- **The guard page is mapped as well:** start the loop in `stack::alloc` at 0, and the overflow does not stop at the
  end of the stack. Here it ends one page further down, at `last page fault at 0xfffffeffffffff28` – below the stack
  area, where nothing is mapped. A stack allocated before it would have been overwritten without a sound.
- **The TLB is not flushed:** without `invlpg` in `unmap`, the old translation survives. Reading the page right after
  `unmap` still gives `0xbeef`; with `invlpg` the same read is a page fault (`page not present, read`).
- **A new table without the writable flag:** the page itself says writable, but one entry above it does not, and
  the first write is a `protection violation, write`. All four levels count.
- **A huge page translated with the 4 KiB mask:** bug #26 – the result is 4 KiB off wherever the PAT bit is set.

## Experiments
1. Print the four indices and every entry on the way for `kmain`, a stack variable and the framebuffer. Which levels
   are huge pages?
2. Map the same frame at two addresses and write through one. Then map a page without `WRITABLE` and write to it –
   what does the page fault's error code say?
3. Count how deep `deeper` gets before the fault (store `depth` in a static). Does it match 16 KiB / 512 bytes? Why
   not exactly?
4. Make `unmap` free the tables that became empty. When is a table empty, and why is the PML4 never freed?

The next chapter puts a heap on top: memory in any size, not just 4 KiB at a time – and with it, `Vec` and `String`.
