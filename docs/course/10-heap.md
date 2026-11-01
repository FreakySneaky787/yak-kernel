# 10 – The heap

**Goal:** memory in any size, not just 4 KiB at a time – so that the kernel can use `Box`, `Vec` and `String` like
any Rust program.
**You end up with:** a free-list allocator behind `#[global_allocator]`, a heap that maps fresh pages with chapter
9's `map` whenever it is full, Rust's `alloc` crate working in the kernel, and a request too big for the heap
(`fault=heap`) that ends in a panic. This chapter ends part II.
**Code:** [`course/10-heap/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/10-heap) (chapter 9
plus `heap.rs`, a few lines in `main.rs` and a fifth boot check)
**What changed:** `diff -ru course/09-paging course/10-heap` (in the repository) shows every line this chapter adds.

## Background: why frames are not enough
Chapter 8 hands out whole frames, chapter 9 puts them where we want them. But most things a kernel keeps are small
and of a size known only at run time: a line of text, a list of open files, a table that grows. Taking a 4 KiB
frame for 20 bytes wastes almost all of it, and a list longer than a frame does not fit at all.

A **heap** cuts bigger pieces of memory into blocks of any size and takes them back in any order. Rust asks for
one with a `Layout` – a size and an alignment – and gives it back with the same `Layout`. That is the whole
interface, the trait `GlobalAlloc`. Once a kernel has one, the `alloc` crate works: `Box`, `Vec`, `String`,
`format!`, `BTreeMap` – everything of the standard library that needs nothing but memory.

The heap needs bookkeeping: which parts are free? Our allocator keeps a **free list**. Every gap of free memory
stores its size and a pointer to the next gap in its own first 16 bytes – the list costs no memory besides the gaps
themselves. The gaps are sorted by address:

```text
 first
   |
   v
 +-----------+--------+------------------+--------+------------+---------
 | size, next| block  | size, next       | block  | size, next | ...
 |  gap 64 B | in use |  gap 1024 B      | in use |  gap 32 KiB|
 +-----|-----+--------+-------|----------+--------+------------+---------
       +----------------------^          +--------^
```

## Step 1: the bookkeeping in the gap
A gap begins with this:

```rust,file=kernel/src/heap.rs
#[repr(C)]
struct Gap {
    size: usize,
    next: *mut Gap,
}
```

Every block starts and ends on a multiple of 16 bytes, and is at least 16 bytes big. Then whatever is left over
when a block is cut out of a gap is either nothing or big enough to hold a `Gap` of its own:

```rust,file=kernel/src/heap.rs
fn block_size(layout: Layout) -> usize {
    layout.size().max(BLOCK_ALIGN).next_multiple_of(BLOCK_ALIGN)
}
```

The list itself is a pointer to the first gap and a counter of the bytes handed out. `add_region` gives it a piece
of memory; it works on any memory, which is how the tests run it on a 16 KiB array.

## Step 2: allocating – first fit
`alloc` walks the list and takes the first gap the block fits into. The block may have to start further up than
the gap, to be aligned as the `Layout` asks:

```rust,file=kernel/src/heap.rs
            let block_start = gap_start.next_multiple_of(align);
            let block_end = block_start + size;
            if block_end <= gap_end {
```

Then the gap leaves the list, and what the block does not cover goes back: the padding in front of it, the rest
behind it. Both are multiples of 16 – step 1 made sure of that.

## Step 3: freeing – and merging
`dealloc` puts the block back into the list at its address. Without more than that, the heap would crumble: free
three neighbouring blocks of 100 bytes, and the list holds three gaps of 100 – none of them big enough for 250
bytes, although 300 free bytes lie side by side. This is **fragmentation**. So `insert` merges a new gap with the
one right after it and the one right before it if they touch:

```rust,file=kernel/src/heap.rs
            // touches the gap after it: one gap
            if !after.is_null() && start + size == after as usize {
                (*gap).size += (*after).size;
                (*gap).next = (*after).next;
            }
```

Because the list is sorted, the two neighbours are found in the same walk that finds the place for the gap.

## Step 4: the global allocator, which grows
The kernel heap is a free list behind a spin lock, plus the end of its mapped area. It has an area of its own,
PML4 entry 507 (`0xfffffd8000000000`) – 508 and 509 hold chapter 9's test pages, 510 its stacks. It starts with nothing mapped. When
a request does not fit, it maps at least 64 KiB more at its end – frames from chapter 8, `map` from chapter 9 – and
hands them to the list:

```rust,file=kernel/src/heap.rs
        let start = self.end;
        while self.end < start + bytes {
            let Some(frame) = pmm::alloc_frame() else {
                break;
            };
            if paging::map(self.end as u64, frame, Flags::DATA).is_err() {
                pmm::free_frame(frame);
                break;
            }
            self.end += PAGE_SIZE as usize;
        }
```

The new region starts where the old one ends, so `insert` merges it with the last gap. The heap stops at 16 MiB:
beyond that, `alloc` returns null. Rust's collections cannot report that to the caller – `Vec::with_capacity` has
no error – so Rust panics with `memory allocation of … bytes failed`. `fault=heap` does exactly that.

The `#[global_allocator]` attribute makes this the allocator behind the `alloc` crate, and `extern crate alloc;` in
`main.rs` brings the crate in:

```rust,file=kernel/src/heap.rs
#[global_allocator]
static HEAP: KernelHeap = KernelHeap(Mutex::new(State {
    list: FreeList::empty(),
    end: START,
}));
```

The lock is a spin lock with interrupts on, like the one of the page tables. That is only safe because no
interrupt handler allocates. If one did, it could interrupt a CPU in the middle of `alloc` and wait forever for
the lock the same CPU holds. The reference kernel turns interrupts off around every heap call for this reason.

What the reference kernel ([`kernel/src/mem/heap.rs`](https://github.com/FreakySneaky787/yak-kernel/blob/main/kernel/src/mem/heap.rs))
has besides: interrupts off around the lock, statistics (number and largest gap, shown by its shell), and a
bigger limit. Neither heap ever gives pages back.

## The tests
`make test` now runs 61 tests in the kernel; nine are new. Four test the free list on a 16 KiB array, without the
heap area:
- `freed_blocks_merge_into_one_gap` – three blocks freed in the order middle, left, right end up as one gap;
- `blocks_are_aligned_as_asked` – a block aligned to 4096 is, and its padding is merged back after freeing;
- `a_full_list_says_null` – and after a free there is room again;
- `first_fit_takes_the_first_hole` – a freed block is the first one used again.

Five test the kernel heap:
- `box_string_and_format_work` – and the `Box` lies in the heap area;
- `the_heap_grows_when_it_is_full` – a `Vec` bigger than everything mapped so far makes the heap map more;
- `freed_memory_is_used_again` – 10 000 boxes of 32 bytes, one after another, leave the heap exactly as it was;
- `a_btreemap_works` – 1000 entries;
- `more_than_the_limit_is_null` – a request over 16 MiB gets null, and maps nothing.

A fifth boot check runs `fault=heap` and wants the panic.

## Run it
```sh
cd course/10-heap
make run
```
After the stack line of chapter 9:

```text
Heap: Box at 0xfffffd8000000000 holds 42, a Vec of 1000 numbers sums to 500500, format! made "YAK has 10 chapters"
Heap: 8048 bytes in use, 64 KiB mapped at 0xfffffd8000000000 - after drop: 0 bytes in use
```
The first `Box` is the first block of the heap. 8048 bytes are 16 for the `Box` (8 rounded up), 8000 for the
numbers and 32 for the `String` (19 bytes); after `drop`, nothing is in use. And the request that is too big:

```sh
KERNEL_CMDLINE=fault=heap make run
```
```text
Asking the heap for 16 MiB and one byte ...
KERNEL PANIC: panicked at /rustc/76456a688bc0ed897fa23a677d4ddb5b3fdb575a/library/alloc/src/alloc.rs:699:9:
memory allocation of 16777217 bytes failed
```
The panic comes from Rust's `alloc` crate, not from our code: `alloc` returned null, and `Vec` gave up. The path
and line depend on your compiler version.

## What can go wrong
- **Gaps are not merged:** take out the merge with the following gap, and freed blocks stay gaps of their own. The
  first test that notices is `blocks_are_aligned_as_asked`: three gaps where there should be one, `left: 3,
  right: 1`.
- **The alignment is ignored:** start the block at the start of the gap, and the block that should be aligned to
  4096 is not: `left: 16, right: 0` – it is 16 bytes past a page boundary.
- **Sizes are not rounded to 16:** a `Box<u64>` takes 8 bytes, and the gap behind it starts at an address that is
  not a multiple of 16, and a leftover gap can be smaller than its own 16-byte header, which then reaches into
  the next block. The kernel soon follows a pointer that is none:
  `misaligned pointer dereference: address must be a multiple of 0x8 but is 0x1`, from the debug checks Rust
  builds in.
- **The pages are not mapped:** hand the area to the list without `map`, and the first `Box` writes a gap header
  into nothing: `PAGE FAULT at address 0xfffffd8000000000`, `page not present, write, kernel mode`.

## Experiments
1. Print the list – start and size of every gap – after each step of `freed_blocks_merge_into_one_gap`.
2. Change first fit to *best fit* (the smallest gap that is big enough). Which test notices? Is it better or
   worse for the 10 000 boxes?
3. Allocate in a loop until `fault=heap`'s panic, printing `heap::mapped()` each time. How often does the heap
   grow, and by how much?
4. Make `alloc` take interrupts off while it holds the lock (`cli`, and `sti` only if they were on before). Then
   allocate in the timer handler – what happened without it?

Part II is complete: frames, pages and a heap. Part III builds on them a first shell – a loop that reads a line,
splits it into words and runs a command.
