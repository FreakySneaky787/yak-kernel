# 8 – Physical memory

**Goal:** know which RAM the kernel may use, and hand it out in 4 KiB pieces – the first thing part II builds
everything else on.
**You end up with:** the kernel prints Limine's memory map, builds a frame allocator on it, takes a frame, writes to
it through the higher-half direct map and gives it back; `make test` takes *every* frame there is and checks each one.
**Code:** [`course/08-frames/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/08-frames) (chapter 7
plus `mem.rs`, `pmm.rs` and two Limine requests)
**What changed:** `diff -ru course/07-keyboard course/08-frames` (in the repository) shows every line this chapter adds.

## Background: physical and virtual
Every address our code has used so far was **virtual**: the kernel at `0xffffffff80000000`, the stack at
`0xffff8000…`. Paging – which Limine switched on before our first instruction – translates each one into a
**physical** address, a place in the actual RAM chips. Both sides are cut into 4 KiB pieces: a *page* of virtual
memory is backed by a *frame* of physical memory. Chapter 9 builds that translation ourselves; this chapter manages
the frames.

Which physical memory is RAM at all, and which of it is free? Only the firmware knows, and Limine passes it on as the
**memory map**: a list of ranges, each with a type. On QEMU with 128 MiB it looks like this (the kernel of this
chapter prints it; shortened):

```text
Memory map (20 entries):
  0x0000001000 - 0x0000061000      384 KiB  Limine (page tables, our stack, the answers)
  0x0000061000 - 0x000009f000      248 KiB  usable
  0x000009fc00 - 0x00000a0000        1 KiB  reserved
  ...
  0x0000100000 - 0x0007dea000   127912 KiB  usable
  0x0007dea000 - 0x0007dec000        8 KiB  Limine (page tables, our stack, the answers)
  0x0007dec000 - 0x0007e0e000      136 KiB  our kernel
  ...
  0x00fd000000 - 0x00fd3e8000     4000 KiB  framebuffer
```

Only **usable** ranges are ours. "Limine" ranges (*bootloader reclaimable*) are free *later*, but right now they
hold the page tables the CPU is using, the stack we are running on, and Limine's answers to our requests – this
memory map included. The reference kernel takes them back once it has its own tables and stack; the course leaves
them alone.

To reach a physical address, the kernel needs a virtual one that maps to it. Limine maps all of physical memory once
more at a fixed offset in the higher half – the **higher-half direct map (HHDM)**: physical `p` is at virtual
`p + offset`.

## Step 1: two more requests
The memory map and the HHDM offset are answers to requests, like the framebuffer of chapter 3:

```rust,file=kernel/src/main.rs
/// Where Limine mapped all of physical memory (the higher-half direct map, see mem.rs).
#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM: HhdmRequest = HhdmRequest::new();
```

`MEMMAP: MemmapRequest` follows the same pattern.

## Step 2: physical addresses get a type
A physical address is a number, and a number can be turned into a pointer by mistake – which writes to whatever
*virtual* address it happens to be. So it gets a type of its own, and the only way to a pointer goes through the
HHDM:

```rust,file=kernel/src/mem.rs
    /// The virtual address at which this physical address is visible, through the HHDM.
    pub fn to_virt(self) -> u64 {
        self.0 + hhdm_offset()
    }
```

`mem::init` reads the offset from Limine, then builds the frame allocator. The normal `kmain` calls it after the
one-second sleep, the test `kmain` after `interrupts::start_keyboard()`.

## Step 3: a bitmap of frames
128 MiB are 32768 frames. The simplest bookkeeping that works is **one bit per frame**: 1 = used, 0 = free – 4 KiB
for the whole machine. A new bitmap starts with every frame used and frees those that lie *wholly* in a usable
range; a range that starts in the middle of a frame does not own that frame:

```rust,file=kernel/src/pmm.rs
            // a frame half inside the region is not ours: round the start up and the end down
            let start = align_up(region.base, PAGE_SIZE) / PAGE_SIZE;
            let end = align_down(region.base + region.length, PAGE_SIZE) / PAGE_SIZE;
```

Frame 0 always stays used: a physical address of 0 is too easy to confuse with "nothing".

Allocating means finding a 0 bit. Instead of testing 32768 bits one by one, `alloc` looks at 64 at once: a `u64`
that is not all ones has a free frame, and `trailing_ones` says which:

```rust,file=kernel/src/pmm.rs
            let frame = word * 64 + self.bits[word].trailing_ones() as usize;
```

`next_word` remembers where the full words end, so the next search does not start at the beginning again; `free`
moves it back if a lower frame becomes free. `free` also refuses nonsense – an address in the middle of a frame,
frame 0, a frame beyond the end, a frame that is free already. In the kernel each of those is a bug, so
`pmm::free_frame` panics on them.

`FrameBitmap` knows nothing about Limine: it gets a slice for its bits and a list of regions. That makes it testable
with a fake machine of 256 frames.

## Step 4: a home for the bitmap
The bitmap needs 4 KiB of memory before there is anything that hands out memory. So it takes the start of the
first usable range it fits into, through the HHDM – and then marks its own frames as used:

```rust,file=kernel/src/pmm.rs
    let mut frames = FrameBitmap::new(bits, frame_count, usable);
    let usable_frames = frames.free_frames();
    frames.reserve(bitmap.0, bitmap_bytes); // or the first frames handed out would overwrite the bitmap itself
```

The global allocator is a `spin::Mutex` like the keyboard buffer. Unlike that buffer, no interrupt handler touches
it, so taking its lock with interrupts on is fine – the rule from chapter 7 is about locks a handler shares.

## Step 5: using a frame
`kmain` prints the memory map, takes a zeroed frame, writes to it and reads it back:

```rust,file=kernel/src/main.rs
    let frame = pmm::alloc_frame_zeroed().expect("a free frame");
    let pointer = frame.as_mut_ptr::<u64>();
    unsafe { pointer.write(0x00C0_FFEE) };
```

## The tests
`make test` now runs 45 tests in the kernel; nine are new:
- `alignment_rounds_to_whole_pages` and `the_hhdm_is_in_the_higher_half` – the helpers of `mem.rs`;
- `only_whole_frames_are_free` – the fake machine: of two ranges, one unaligned, five frames count;
- `the_lowest_frame_comes_first_until_none_is_left` – then `None`;
- `freed_frames_come_back_and_nonsense_is_refused` – double free, unaligned, frame 0, beyond the end;
- `reserved_frames_are_not_handed_out`;
- `most_of_the_ram_is_usable` – between 100 and 128 MiB of the machine's 128;
- `every_frame_is_usable_ram_and_none_is_the_bitmap` – takes every frame the allocator has. Each must lie wholly in a
  usable range of the memory map and outside the bitmap, and each stores the address of the previous one: a chain
  through all of memory, which the test then walks back, freeing every frame. At the end the free count is what it was;
- `a_zeroed_frame_is_zero` – a frame filled with `0xAB`, freed and taken again, comes back as zeros.

The boot check wants the memory map, the "MiB usable" line and the value read back through the HHDM.

## Run it
```sh
cd course/08-frames
make run
```
After the timer lines, the memory map, then:

```text
Frames: 125 MiB usable, 32066 frames free; the bitmap takes 4 KiB at 0x61000
Frame 0x62000, seen at 0xffff800000062000 through the HHDM: wrote 0xc0ffee, read 0xc0ffee
```
The bitmap took the first usable frame, `0x61000`, so the first frame handed out is the next one.
The exact numbers depend on QEMU's firmware: under QEMU 8.2 one more frame is free.

## What can go wrong
- **The bitmap is not reserved:** the first frame handed out is the bitmap itself. `alloc_frame_zeroed` fills it
  with zeros – every frame is "free" now – and the kernel panics as soon as it gives the frame back:

  ```text
  Frame 0x61000, seen at 0xffff800000061000 through the HHDM: wrote 0xc0ffee, read 0xc0ffee
  KERNEL PANIC: panicked at src/pmm.rs:213:9:
  free_frame(0x61000): NotAllocated
  ```
  In `make test`, `a_zeroed_frame_is_zero` fails first.
- **A physical address used as a pointer:** `frame.0 as *mut u64` instead of `frame.as_mut_ptr()`. Nothing is mapped
  at virtual `0x62000`, and chapter 5's handler reports it:

  ```text
  EXCEPTION: PAGE FAULT at address 0x0000000000062000
    cause: page not present, write, kernel mode
  ```
- **Limine's memory counted as usable:** the treacherous one – the kernel boots normally; it only uses one frame,
  and that one happens to be harmless. `every_frame_is_usable_ram_and_none_is_the_bitmap`, which writes into every
  frame, ends in a page fault in the middle of the HHDM (`0xffff800007f79000`). Most likely one of the frames it
  wrote into was one of Limine's page tables, and with it the mapping of that part of the HHDM went away.
- **Frames half inside a range:** a region boundary that is not a multiple of 4 KiB, rounded the wrong way, hands
  out a frame that partly belongs to the firmware. `only_whole_frames_are_free` has such a region.

## Experiments
1. Start QEMU with `-m 512M` (in `run-qemu.sh`). How big is the bitmap now, and where does it live?
2. The test chains every frame to the one before. That chain *is* an allocator – a **free list**: allocating takes the
   first frame of the chain, freeing puts one in front. Write it. What does it need instead of a bitmap, and what can
   it not do that the bitmap can (hint: "is this frame free?")?
3. Remove `next_word` (always search from word 0) and time the test that takes every frame.
4. Write `alloc_contiguous(n)`: `n` frames that follow each other – devices that read memory themselves need that.
   Why is it easy with a bitmap and hard with a free list?
