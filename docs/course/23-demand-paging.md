# 23 – A heap for programs: `brk` and demand paging

**Goal:** give a program memory it can ask for while it runs – and map the frames only when the program really uses
the pages.
**You end up with:** the system call `brk`, a heap per program that starts behind its image and may grow to 64 MiB, a page
fault handler that maps a zeroed frame when a program first touches a page of its heap, and a heap that gives its frames
back when it shrinks and when the program ends. A heap of 32 MiB with three touched pages costs 17 frames, not 8000. Part VI
continues.
**Code:** [`course/23-demand-paging/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/23-demand-paging)
(chapter 22 plus `demand.rs` and `programs/heap.S`; the page fault handler in `interrupts.rs`, `spawn` and the system
call table in `user.rs` change)
**What changed:** `diff -ru course/22-files course/23-demand-paging` (in the repository) shows every line this chapter
adds.

## Background: ask now, pay when you use it
A program that reads a file whose size it learns at run time needs a buffer of that size. Its data segment is whatever its
file says and its stack is fixed, so it needs to ask the kernel for memory. The classic call is `brk`: a program has a
*heap*, a region that starts right behind its image, and `brk(address)` moves the heap's end – the *break*. Moving it up
grows the heap, moving it down gives memory back. `brk(0)` moves nothing and says where the break is.

The simple kernel would map frames for every page between the old and the new break. A program that asks for 32 MiB
because it *might* need that much would take 8192 frames – a large part of this kernel's RAM – and never use them. So the
kernel does the cheap thing: moving the break changes a number. The page tables are not touched. The first time the
program *uses* a page of the new heap, the CPU finds no mapping and raises a **page fault** (vector 14) – and the
handler, which until now only knew how to end a program, looks at the address: is it in this program's heap? Then it maps
a fresh, zeroed frame there and returns. The `iretq` of the fault handler runs the instruction that faulted again – this time
it works – and the program never knows. That is *demand paging*; every real operating system does it, for the heap, the
stack, and – with the file behind the page – for the program itself.

## Step 1: the heap of a program (`demand.rs`)
A heap is two numbers: where it starts (a page boundary) and where it ends. Programs have one each, in a map by the
id of their thread, like the file tables of chapter 22:

```rust,file=kernel/src/demand.rs
struct Heap {
    start: u64,
    end: u64,
}
```

The pages that belong to the heap are those below the break, rounded *up*: the break is exact to the byte, the pages are
whole – a break in the middle of a page keeps the page.

The heap is registered by **the program's own thread**, as the first thing it does, before it goes to ring 3:

```rust,file=kernel/src/user.rs
    // the heap starts at the first page behind the last segment
    let heap = elf
        .segments
        .iter()
        .map(|s| align_up(s.vaddr + s.memsz, PAGE_SIZE))
        .max()
        .unwrap_or(PAGE_SIZE);
    let thread = task::spawn_in("user", space.root(), move || {
        demand::begin(heap); // by the program's own thread: before it is in ring 3, its heap exists
        enter(entry, STACK_TOP)
    });
```

If the code that *starts* the thread did it, the entry could come too late: a thread is runnable from the moment it exists,
and a program's first call could look for its heap before it was made. (The reference kernel's bug #19 was exactly this:
a process table entry made after the thread could already run.) The heap starts at the first page behind the last
segment of the ELF file.

## Step 2: moving the break
```rust,file=kernel/src/demand.rs
pub fn brk(address: u64) -> Result<u64, i64> {
    let me = task::current();
    let heap = heap().ok_or(ENOMEM)?;
    if address == 0 {
        return Ok(heap.end);
    }
    if address < heap.start || address - heap.start > HEAP_MAX {
        return Err(ENOMEM);
    }
    // moving down: the pages that are above the new break are given back. Moving up needs no work.
    let (old_end, new_end) = (align_up(heap.end, PAGE_SIZE), align_up(address, PAGE_SIZE));
    for page in (new_end..old_end).step_by(PAGE_SIZE as usize) {
        // a page that was never touched is not there: nothing to give back
        if let Some(frame) = paging::unmap(page) {
            pmm::free_frame(frame);
        }
    }
    interrupts::without_interrupts(|| {
        if let Some(entry) = HEAPS.lock().get_mut(&me) {
            entry.end = address;
        }
    });
    Ok(address)
}
```

A break below the start of the heap or more than 64 MiB above it is refused with `ENOMEM` (Linux's 12) and nothing
changes. The subtraction is written so that nothing overflows (`address - heap.start`, after the check that `address` is
not below), and the address space has room: the image ends below 0x7000_0000_0000 (chapter 21) and the stack begins at
0x7FFF_FFFF_0000, so the heap at its largest never reaches either.

Moving the break *down* gives the pages above it back. A page that was never touched is not mapped, so there is
nothing to give back – `unmap` returns `None`. A page that was is unmapped, and its frame goes to the allocator. The
`unmap` of chapter 9 ends with `invlpg`: the CPU keeps recent translations in the TLB, and without that instruction it
would go on using the old one – the program could still touch the page it just gave back, and write into a frame that now
belongs to someone else. (The test below shows what happens without it.)

## Step 3: the page fault handler
Before, every page fault from ring 3 ended the program. Now the handler asks the heap first:

```rust,file=kernel/src/demand.rs
pub fn handle_page_fault(address: u64, error_code: u64) -> bool {
    if error_code & 1 != 0 {
        return false;
    }
    let page = align_down(address, PAGE_SIZE);
    heap().is_some_and(|heap| heap.pages().contains(&page)) && map_fresh(page)
}
```

and `page_fault` in `interrupts.rs` calls it for faults that come from ring 3:

```rust,file=kernel/src/interrupts.rs
    // A program touched a page of its heap that is not there yet: it gets one, and runs the same instruction again
    // (chapter 23). Everything else a program does wrong is the end of it.
    if frame.cs & 3 == 3 && demand::handle_page_fault(read_cr2(), error_code) {
        return;
    }
```

The error code the CPU pushes says what kind of fault this is. Bit 0 set means the page *is* there and the access was not
allowed – a write into read-only code, a jump into a data page (no-execute): never the heap's business, and never a reason
to allocate. Bit 0 clear means "no page there", and only then does the handler look at the address. A fault outside the
heap still ends the program, exactly as in chapter 20.

## Step 4: the kernel touches the heap, too
A program may hand the kernel a buffer it has not touched yet – `read(fd, heap + 8, 5)` into a fresh heap page. The
kernel *checks* the buffer before it writes into it (chapter 22), and a page that is not there fails the check. So the check
asks the heap first:

```rust,file=kernel/src/demand.rs
pub fn populate(address: u64, length: u64) {
    let Some(heap) = heap() else {
        return;
    };
    let Some(end) = address.checked_add(length) else {
        return;
    };
    let pages = heap.pages();
    let first = align_down(address, PAGE_SIZE).max(pages.start);
    let last = align_up(end, PAGE_SIZE).min(pages.end);
    for page in (first..last.max(first)).step_by(PAGE_SIZE as usize) {
        if paging::translate(page).is_none() && !map_fresh(page) {
            return; // out of frames: the check that follows fails for the pages that are missing
        }
    }
}
```

`populate` is called at the top of `range_allows` in `user.rs`, which every system call goes through. Only the part of the
range *inside the heap* is looked at, so the work is bounded by the heap's size (at most 16 384 pages) whatever length
the program names – bug #11 of the reference kernel was a loop over the whole range the program passed, and a length
of 2^47 froze the machine.

## Step 5: a program with a heap
`programs/heap.S` asks for 32 MiB, stores a number at the first page, 16 MiB further, and in the last eight bytes, and
reads them back:

```text
# Chapter 23: asks for a heap of 32 MiB with `brk` - and touches three pages of it: the first, one 16 MiB further, the
# last. The kernel maps a frame when each is first touched, three in all, not 8192.
# System calls: 1 write(fd, address, length), 2 exit(code), 7 brk(address): the new end of the heap, or the old one for
# address 0. Exit code 0 if what the program stored in the three places is what it reads back.
        .intel_syntax noprefix
        .text
        .globl  _start
_start:
        mov     eax, 7                  # brk(0): where does the heap start?
        xor     edi, edi
        int     0x80
        mov     rbx, rax
        mov     eax, 7                  # brk(start + 32 MiB)
        lea     rdi, [rbx + 0x2000000]
        int     0x80
        lea     rcx, [rbx + 0x2000000]
        cmp     rax, rcx                # the break is where we asked for it - or the kernel said no
        jne     failed
        mov     rax, 0x1CAB1E           # three stores, far apart: first page, middle, last 8 bytes
        mov     [rbx], rax
        mov     [rbx + 0x1000000], rax
        mov     [rbx + 0x2000000 - 8], rax
        cmp     [rbx], rax              # and back
        jne     failed
        cmp     [rbx + 0x1000000], rax
        jne     failed
        cmp     [rbx + 0x2000000 - 8], rax
        jne     failed
        mov     eax, 1                  # write(1, message, length)
        mov     edi, 1
        lea     rsi, [rip + message]
        mov     edx, offset length
        int     0x80
        xor     edi, edi                # exit(0)
        jmp     leave
failed:
        mov     edi, 1
leave:
        mov     eax, 2
        int     0x80
        ud2
```

## The tests
`make test` runs 187 tests in the kernel (chapter 22: 178). The tests of the new calls use the *script* of chapter 22, which
moved into a file of its own (`script.rs`) and also looks at the program's heap when it ends. New:
- `brk` starts where the image ends, and the break stays in its limits: `brk(0)`, up by 100 bytes, below the start (twice),
  one byte more than 64 MiB, the end of the address space, exactly 64 MiB, and all of it back;
- the first touch maps the page zeroed: a program that only *reads* page 4 of a 64 KiB heap finds zeros;
- **`/bin/heap` – 32 MiB, three pages touched, 17 frames in use** (the test measures them when the program has ended, before
  its memory is given back: image, stack, three heap pages and the tables above them – no more than 40 allowed);
- pages given back are gone: a program touches four pages, shrinks to nothing and touches the first again – a page fault; four
  such programs at once, six rounds, so the programs move between the CPUs; and the frames of the 96 pages are back;
- only the heap is demand paged: a page outside it, the page behind the break, and the byte after the last byte of the
  break's page end the program, while the last eight bytes of the page the break is in belong to the heap;
- a system call fills an untouched page: the kernel's `read` into the second page of a heap whose other pages were never
  touched – only that page appears (and the one below the break that a failed call looked at: the program cannot tell, it
  reads zeros from a page that is there as from one that is not); a buffer behind the break is `EFAULT`;
- twelve runs of `/bin/heap` lose at most 16 frames – the stack of the last thread;
- the heap is data, not code: a program stores a `ret` in its heap and jumps there – the page is there and not
  executable, a *protection* fault, and the program ends;
- a thread that is no program has no heap: `brk` is refused, and a fault of such a thread is nobody's business.

The boot check now also runs `/bin/heap`, and types `run /bin/heap`. Counter-checks, one run each:
- **`unmap` without `invlpg`:** `pages_given_back_are_gone_for_good` finds a program that ended with `Code(0)` where a page fault was due –
  the CPU still knew the page and let the program write into it. The one instruction is all that makes giving pages back safe;
- `brk` that unmaps but does not free the frames: 96 frames lost in the same test;
- no `populate`: the `read` into the untouched page returns `-14` where 5 is expected;
- the page fault handler not asking the heap: `/bin/heap` is killed by a page fault at its first touch (0x40002d);
- no limit of 64 MiB: with a break of `u64::MAX - 8` the kernel itself panics – "attempt to add with overflow" in `align_up`
  (`mem.rs`): a number from a program broke an assertion of the kernel. The check is not only a limit, it is the argument
  check;
- a handler that does not look at bit 0 of the error code: **no test notices**. The jump into the heap still ends the program, because
  `map_fresh` on a page that is there fails and the handler returns false. The line only saves the frame
  the handler would allocate for nothing, and says what the handler means.

## Run it
```sh
cd course/23-demand-paging
make run
```
After the disk lines:

```text
Heap: 32 MiB asked for, three pages touched
Ring 3: /bin/heap ended with Code(0)
```

and in the shell: `run /bin/heap` prints the same and `/bin/heap: Code(0)`.

## What can go wrong
- **No `invlpg` after `unmap`:** the first counter-check. The kernel and the program disagree about what is mapped, until some
  unrelated event flushes the TLB. A bug like that shows up as a program that sometimes survives what should have killed it.
- **Another CPU's TLB:** a program has one thread, which runs on one CPU at a time. When it stops running there, the CPU
  switches to a thread with other page tables (the next thread's or the idle thread's) and loading them flushes the TLB:
  no CPU keeps translations of a program that does not run on it. That is an argument, not something a counter-check
  showed – and it stops being true the day a program has two threads at once. Then giving pages back needs a
  *TLB shootdown*: an interrupt to every other CPU that runs the program, to make it flush. The reference kernel does this for
  threads (`paging::shoot_down`, bug #15).
- **A heap entry made by the wrong thread:** see step 1.
- **Looping over the program's length:** see step 4 – bound every loop by something the kernel owns.
- **A zero-length request:** `populate` with length 0 maps nothing; chapter 21's rule that an empty range is nobody's memory
  still holds.
- **Out of frames in the fault handler:** it returns false, and the program ends with a page fault. The kernel does not
  panic for a program that asked for more than there is.

## Experiments
1. Set `HEAP_MAX` to 1 MiB. Which tests fail, and what do they tell you?
2. Change `heap.S` to touch *every* page of the 32 MiB (a loop). How many frames does the test measure now, and how long does
   it take under QEMU's emulation?
3. A program could `brk` once for 64 MiB and never touch it. What does that cost the kernel? Is there a number that a
   program could make large that costs real frames without ever being touched? (Hint: the page tables.)
4. Make the fault handler map the page *and the next three*. Which test notices, and is the result worse or better?

## Next
A program can ask for memory, use the disk and the console. What it cannot yet do is use the *floating point unit*: the
registers of `xmm0`–`xmm15` belong to whoever ran last. Chapter 24 gives every thread its own.
