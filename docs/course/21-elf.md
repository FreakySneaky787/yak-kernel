# 21 – Address spaces and ELF files

**Goal:** give every program a set of page tables of its own, and load programs the way real systems do – from a
file, in the ELF format – instead of copying machine code into three pages the kernel keeps for them.
**You end up with:** a PML4 per program whose upper half is the kernel's, `AddressSpace` (map, copy in, give back
everything on `drop`), an ELF loader that refuses what it should not load, a CR3 per thread that the scheduler loads
at every switch, and programs built with `as` and `ld` that sit in `/bin` of the test disk. Two programs run at once
at the *same* virtual addresses without meeting. Part VI continues.
**Code:** [`course/21-elf/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/21-elf)
(chapter 20 plus `address_space.rs`, `elf.rs` and `programs/`; `paging.rs`, `task.rs` and `user.rs` change, the
shell gets `run`)
**What changed:** `diff -ru course/20-ring3 course/21-elf` (in the repository) shows every line this chapter adds.

## Background: one address, many meanings
In chapter 20 a program lived in the kernel's own page tables: three pages at 0x400000, 0x401000 and 0x7FF000. That
worked because there was only ever one program – and it could only work that way: a second program would have needed
the same addresses for different bytes.

Page tables are what makes that possible. The CPU turns every virtual address into a physical one through the tables
CR3 points to. Load other tables and the same address means other memory. So an operating system gives every
program its own tables – an *address space* – and loads the program's tables when it runs. A program cannot even
*name* the memory of another one: there is no address it could use.

The kernel must be there in every address space, or the first interrupt would jump into nothing: the handler, its
stack, the scheduler's data are all kernel memory. The usual solution, and ours: **the upper half of every PML4 is the
kernel's** (entries 256–511, addresses from 0xFFFF_8000_0000_0000), and the lower half (entries 0–255) is the
program's. We do not copy the kernel's *tables*, only the 256 PML4 entries – which point to the same PDPTs. So when
the kernel later maps a heap page or a stack, it changes a table below the PML4 that every address space shares, and
every program sees the change at once. That has a condition, and it is the first thing this chapter does.

The second half of the chapter is the file. A compiler and a linker turn a program into an **ELF file**. For loading we
need only a small part of it: a header (what kind of file, which CPU, *where the first instruction is*) and a table of
*program headers*. Every header of type LOAD says "the `filesz` bytes at `offset` of this file belong at the virtual
address `vaddr`; the region is `memsz` bytes long – the bytes beyond `filesz` are zeros – and may be read, written
and/or executed". Loading a program is: for every LOAD header, map the pages, copy the bytes.

## Step 1: tables for programs (`paging.rs`)
Until now `map`, `translate` and the others worked on the tables in CR3. To fill the tables of a program that is not
running – we are building them – they get a twin that takes the PML4 as a parameter (`translate_in`, `map_in`); the
old functions are one-liners that pass `read_cr3()`:

```rust,file=kernel/src/paging.rs
pub fn translate(virt: u64) -> Option<PhysAddr> {
    translate_in(read_cr3(), virt)
}
```

```rust,file=kernel/src/paging.rs
pub fn map(virt: u64, frame: PhysAddr, flags: Flags) -> Result<(), MapError> {
    map_in(read_cr3(), virt, frame, flags)
}
```

The condition from the background: a PML4 entry that is empty when the copy is made stays empty in the copy, and a
mapping the kernel makes later would create the entry *in the kernel's PML4 only*. So `init` gives every entry of the
upper half a table behind it – an empty PDPT where nothing is mapped – and remembers the kernel's PML4. It also
clears the USER bit that Limine set on two of the entries (256, the HHDM's, and 511, the kernel's – the test below
found them): the pages underneath are supervisor-only, so ring 3 could not use them anyway, but nothing of the
kernel's half should look open to a program, and every program's PML4 gets a copy of these entries:

```rust,file=kernel/src/paging.rs
pub fn init() {
    let root = read_cr3();
    KERNEL_ROOT.store(root.0, Ordering::Relaxed);
    for entry in &mut table(root).0[KERNEL_HALF..] {
        if entry.is(Flags::PRESENT) {
            entry.0 &= !Flags::USER.0;
        } else {
            let pdpt = pmm::alloc_frame_zeroed().expect("a frame for a table of the kernel half");
            *entry = Entry::new(pdpt, Flags(Flags::PRESENT.0 | Flags::WRITABLE.0));
        }
    }
}
```

and a program's PML4 is a zeroed frame with the upper 256 entries copied from it:

```rust,file=kernel/src/paging.rs
pub fn new_root() -> Option<PhysAddr> {
    let root = pmm::alloc_frame_zeroed()?;
    let kernel = &table(kernel_root()).0;
    table(root).0[KERNEL_HALF..].copy_from_slice(&kernel[KERNEL_HALF..]);
    Some(root)
}
```

Giving a space back is a walk over the tables of the lower half. Every page of a program owns its frame, so every
frame found there is freed, then the tables, then the PML4 – but never the upper half, whose tables are the kernel's:

```rust,file=kernel/src/paging.rs
pub fn free_user_half(root: PhysAddr) {
    assert!(root != read_cr3(), "the tables in use cannot be freed");
    assert!(root != kernel_root(), "the kernel's tables are not a program's");
    free_tables(root, 4);
}
```

```rust,file=kernel/src/paging.rs
fn free_tables(frame: PhysAddr, level: u32) {
    // in the PML4 only the lower half is ours
    let count = if level == 4 { KERNEL_HALF } else { 512 };
    for entry in &table(frame).0[..count] {
        if !entry.is(Flags::PRESENT) {
            continue;
        }
        if level == 1 {
            pmm::free_frame(entry.address()); // a page of the program
        } else {
            assert!(!entry.is(Flags::HUGE), "a program's address space has no huge pages");
            free_tables(entry.address(), level - 1);
        }
    }
    pmm::free_frame(frame);
}
```

(`kmain` calls `paging::init()` right after `smp::reserve_frame()` – the frame for the other CPUs' start code below
1 MiB must be taken before this allocates – and the test `kmain` does the same.)

`USER_READ_ONLY` is new in `Flags` too: a page that ring 3 may read but neither write nor execute – constants.

## Step 2: `AddressSpace`
`address_space.rs` wraps a PML4 in a type that owns what is mapped under it:

```rust,file=kernel/src/address_space.rs
pub struct AddressSpace {
    root: PhysAddr,
}
```

`map_fresh` maps a new zeroed frame – from then on the address space's – and refuses the upper half. `write` copies
bytes in, over page boundaries, through the HHDM: the kernel reaches a frame by its physical address whatever the
page's flags say, so it can fill a code page that is read-only for the program itself. And `drop` is the end of the
program's memory:

```rust,file=kernel/src/address_space.rs
impl Drop for AddressSpace {
    fn drop(&mut self) {
        paging::free_user_half(self.root);
    }
}
```

## Step 3: the ELF loader (`elf.rs`)
`parse` reads the header and the program headers and decides whether this is a program we can load. **Nothing in a
file is trusted** – every offset and size is somebody's claim – so the checks come first, and they use `checked_add`:
a segment at 0xFFFF_FFFF_FFFF_FFF0 must not wrap around into low addresses:

```rust,file=kernel/src/elf.rs
pub fn parse(data: &[u8]) -> Result<ElfImage, ElfError> {
    const HEADER: usize = 64;
    const PROGRAM_HEADER: usize = 56;
    if data.len() < HEADER {
        return Err(ElfError::TooShort);
    }
    // 0x7F 'E' 'L' 'F', class 2 (64-bit), data 1 (little endian)
    if data[..4] != *b"\x7FELF" || data[4] != 2 || data[5] != 1 {
        return Err(ElfError::BadMagic);
    }
    if u16_at(data, 18) != 0x3E {
        return Err(ElfError::WrongArchitecture);
    }
    if u16_at(data, 16) != 2 {
        return Err(ElfError::NotExecutable); // 2 = ET_EXEC; 3 would be a position-independent file
    }
    let entry = u64_at(data, 24);
    let table = u64_at(data, 32);
    let (entry_size, count) = (u16_at(data, 54) as usize, u16_at(data, 56) as usize);
    let table_end = (count * PROGRAM_HEADER) as u64;
    if entry_size != PROGRAM_HEADER
        || count > 64
        || table.checked_add(table_end).is_none_or(|end| end > data.len() as u64)
    {
        return Err(ElfError::BadProgramHeaders);
    }
    let mut segments = Vec::new();
    for i in 0..count {
        let at = table as usize + i * PROGRAM_HEADER;
        if u32_at(data, at) != 1 {
            continue; // not LOAD: notes, stack flags ... nothing for us to do
        }
        let segment = Segment {
            flags: u32_at(data, at + 4),
            offset: u64_at(data, at + 8),
            vaddr: u64_at(data, at + 16),
            filesz: u64_at(data, at + 32),
            memsz: u64_at(data, at + 40),
        };
        if segment.memsz == 0 {
            continue;
        }
        if segment.filesz > segment.memsz {
            return Err(ElfError::BadProgramHeaders);
        }
        if segment
            .offset
            .checked_add(segment.filesz)
            .is_none_or(|end| end > data.len() as u64)
        {
            return Err(ElfError::SegmentOutsideFile);
        }
        let in_range = segment.vaddr >= IMAGE_START
            && segment
                .vaddr
                .checked_add(segment.memsz)
                .is_some_and(|end| end <= IMAGE_END);
        if !in_range {
            return Err(ElfError::SegmentOutsideUserSpace);
        }
        if segment.flags & FLAG_W != 0 && segment.flags & FLAG_X != 0 {
            return Err(ElfError::WritableAndExecutable);
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        return Err(ElfError::NoLoadableSegment);
    }
    let entry_in_code = segments
        .iter()
        .any(|s| s.flags & FLAG_X != 0 && (s.vaddr..s.vaddr + s.memsz).contains(&entry));
    if !entry_in_code {
        return Err(ElfError::BadEntryPoint);
    }
    Ok(ElfImage { entry, segments })
}
```

What it refuses, as `ElfError`: a file shorter than the header, a wrong magic number (0x7F "ELF"), 32-bit or big-endian
files, other CPUs, files that are not fixed-address executables, a damaged program header table, segments that are
larger on disk than in memory, whose bytes are not all in the file, that would be loaded outside
`0x1_0000..0x7000_0000_0000` (a null pointer must fault; the kernel's half is not for programs), that are both
writable and executable – and an entry point that is not in an executable segment.

`load` maps the pages of every segment with the flags its header asks for and copies the file's bytes in. A fresh
frame is zero, and that is all `.bss` needs:

```rust,file=kernel/src/elf.rs
pub fn load(space: &mut AddressSpace, data: &[u8], image: &ElfImage) -> Result<(), MapError> {
    for segment in &image.segments {
        let first = align_down(segment.vaddr, PAGE_SIZE);
        let end = align_up(segment.vaddr + segment.memsz, PAGE_SIZE);
        for page in (first..end).step_by(PAGE_SIZE as usize) {
            space.map_fresh(page, segment.page_flags())?;
        }
        let file_bytes = &data[segment.offset as usize..(segment.offset + segment.filesz) as usize];
        space.write(segment.vaddr, file_bytes)?;
    }
    Ok(())
}
```

## Step 4: a thread of its own tables (`task.rs`)
A thread gets a `cr3`: the kernel's for kernel threads, a program's own for its thread (`spawn_in`). At every switch
the scheduler loads the next thread's tables, right after it set RSP0 for it:

```rust,file=kernel/src/task.rs
    // ... and the thread runs on its own page tables (chapter 21). Both threads' stacks, the scheduler's data and
    // this code are in the upper half, which every PML4 shares: nothing we use until the switch is gone.
    let new_cr3 = thread.cr3;
    if new_cr3 != paging::read_cr3() {
        paging::write_cr3(new_cr3);
    }
```

This is safe because everything the code touches between this line and the end of the switch – both threads' kernel
stacks, the scheduler's data, the code itself – is in the upper half, which is the same in every address space. Writing
CR3 also flushes the TLB: the CPU forgets every translation it cached from the old tables. (A program has one
thread, which runs on one CPU at a time, and a CPU that switches to other tables flushes – so no other CPU holds
translations of a program's tables. Chapter 23 will unmap pages and has to keep that argument true.)

## Step 5: `spawn`, `join` and the exit of a program (`user.rs`)
`spawn` is chapter 20's `run` taken apart: parse the file, make an address space, load the segments, map a stack of
four pages below 0x7FFF_FFFF_0000, and start a thread on the new tables that goes to ring 3 at the entry point:

```rust,file=kernel/src/user.rs
pub fn spawn(image: &[u8]) -> Result<Program, SpawnError> {
    let elf = elf::parse(image)?;
    let mut space = AddressSpace::new().ok_or(MapError::OutOfFrames)?;
    // an error from here on drops `space`, which gives back what was mapped so far
    elf::load(&mut space, image, &elf)?;
    for page in 1..=STACK_PAGES {
        space.map_fresh(STACK_TOP - page * PAGE_SIZE, Flags::USER_DATA)?;
    }
    let entry = elf.entry;
    let thread = task::spawn_in("user", space.root(), move || enter(entry, STACK_TOP));
    Ok(Program { thread, space })
}
```

It returns a `Program` at once – the program runs concurrently – and `join` waits for it, then drops the address space:

```rust,file=kernel/src/user.rs
impl Program {
    /// Waits until the program has ended, frees its memory and says how it ended.
    pub fn join(self) -> Exit {
        task::join(self.thread);
        // the thread is off its CPU, and that CPU has loaded other tables: the address space is nobody's now
        drop(self.space);
        interrupts::without_interrupts(|| EXITS.lock().remove(&self.thread))
            .expect("a program ends by exit or by a fault")
    }
}
```

The order in `join` matters: the frames are the program's code and stack, so the address space must outlive the thread.
`Program` has no `drop` of its own to do this behind your back, and `#[must_use]` makes the compiler complain about a
`Program` that is thrown away unjoined. And how a program ended no longer sits in one global variable (there are
several programs now) but in a table by thread id:

```rust,file=kernel/src/user.rs
pub fn end(exit: Exit) -> ! {
    let me = task::current();
    interrupts::without_interrupts(|| EXITS.lock().insert(me, exit));
    task::exit()
}
```

## Step 6: programs that are files
The programs of chapter 20 were machine code in the kernel's `global_asm!`. Now they are source files in `programs/`,
written in the assembler's *Intel syntax*:

```text
# The first program that is a file (chapter 21): it writes a line - system call 1, `write(address, length)` -
# and ends with exit code 42 - system call 2, `exit(code)`.
# Built by the Makefile: `as` turns it into an object file, `ld` (with user.ld) into an ELF file.
        .intel_syntax noprefix
        .text
        .globl  _start
_start:
        mov     eax, 1                  # write
        lea     rdi, [rip + message]
        mov     esi, offset length      # "offset": the number itself, not the memory at that address
        int     0x80
        mov     eax, 2                  # exit(42)
        mov     edi, 42
        int     0x80
        ud2                             # exit does not return

        .section .rodata
message:
        .ascii  "Hello from ring 3!\n"
        .set    length, . - message
```

`make programs` turns each of them into an ELF file with two tools every Linux has: `as` assembles, `ld` links – with
a linker script, `programs/user.ld`, that says where the segments go: code and constants at 0x400000 (readable,
executable), data at 0x401000 (readable, writable):

```text
ENTRY(_start)

PHDRS
{
    code PT_LOAD FLAGS(5);  /* R X */
    data PT_LOAD FLAGS(6);  /* R W */
}

SECTIONS
{
    . = 0x400000;
    .text   : { *(.text .text.*) } :code
    .rodata : { *(.rodata .rodata.*) } :code

    . = 0x401000;
    .data : { *(.data .data.*) } :data
    .bss  : { *(.bss .bss.* COMMON) } :data

    /DISCARD/ : { *(.note* .comment .eh_frame*) }
}
```

`mkdisk.sh` copies the results to `/bin` of the test disk, and the boot check and the tests run them from there. Here
is what the linker made of `hello` (`readelf -l build/programs/hello`):

```text
Program Headers:
  Type           Offset   VirtAddr           PhysAddr           FileSiz  MemSiz   Flg Align
  LOAD           0x001000 0x0000000000400000 0x0000000000400000 0x000034 0x000034 R E 0x1000
  LOAD           0x0000b0 0x0000000000000000 0x0000000000000000 0x000000 0x000000 RW  0x1000
```

0x34 = 52 bytes of code and text in a segment that is readable and executable. The second header is a LOAD of size
zero – the program has no data – which is why `parse` skips headers with `memsz == 0`.

`kmain` runs two of them after the disk lines, and the shell has a new command:

```rust,file=kernel/src/shell.rs
fn run_program(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let [path] = args else {
        return Err(String::from("usage: run <program>"));
    };
    let path = absolute(path);
    let exit = user::run_file(&path).map_err(|e| alloc::format!("{}: {:?}", path, e))?;
    let _ = writeln!(out, "{}: {:?}", path, exit);
    Ok(())
}
```

## The tests
`make test` runs 166 tests in the kernel (chapter 20: 145); the new ones:
- `address_space.rs`: a new space has the kernel and nothing else (the kernel's code is at the same frame as in the
  kernel's own tables); the same address leads to different frames in two spaces – and the kernel's own tables never saw
  it; what the kernel maps *later* shows up in a space that already exists; writing crosses page boundaries and stops
  at holes; dropping a space gives back every frame (41 pages + 1 PML4 + 6 tables = 48, counted exactly).
- `elf.rs`: a valid file is understood, and the flags become page flags; eleven damaged headers, refused with seven
  different errors; segments outside the file or the lower half, with sizes and addresses that would wrap around; a segment that is both
  writable and executable, an entry point in a data segment; loading copies the bytes and zeros the rest, and gives
  everything back; two segments on one page are an error.
- `user.rs`: `user_bytes` is looked at from a kernel thread that runs on a program's tables – the edges of chapter 20
  (past the lower half, wrapping around, an unmapped page) plus a page without USER; **two programs, both at
  0x400000 and 0x401000, write their own number into the data page, one after the other, and each finds its own**
  (and the frames behind the address are different); programs from `/bin/hello` and `/bin/cli` – and `/hello.txt`,
  `/big.bin`, a missing file and a directory refused with the right error; a program that cannot be loaded leaves no
  frame behind; ten programs one after the other lose at most 16 frames (the stack of the last thread, freed when the
  next one starts – a program takes 13: code, data, four stack pages, PML4 and two chains of PDPT, PD and PT); the
  kernel's tables are in CR3 again after a program; four programs and four kernel threads in one run queue.
- `paging.rs`: every entry of the upper half exists and has no USER bit.
- `user.rs` once more: `write` of nothing from the null pointer (and from the end of the address space) returns 0 –
  see "What can go wrong".
- `shell.rs`: `run` starts a program from the disk.

The boot check types `run /bin/hello` and expects `/bin/hello: Code(42)`. The unit tests pass with 1, 2 and 4
CPUs (`KERNEL_SMP=1 cargo test` …). Six counter-checks, one run each:
- no table behind the entries of the upper half (`init` does nothing): `what_the_kernel_maps_later_shows_up_in_every_space`
  fails – the new mapping is not there;
- `drop` of the address space does not call `free_user_half`: `dropping_a_space_gives_every_frame_back` fails, 48 frames
  short (29467 free, 29515 expected);
- the scheduler does not load the thread's CR3: the first program started from a file dies with a page fault at
  0x400000 – on the kernel's tables there is nothing there;
- `new_root` leaves the upper half empty: `a_new_space_has_the_kernel_and_nothing_else` fails; the kernel's code is not
  there;
- `init` leaves the USER bit alone: "entry 256 has the USER bit";
- `user_bytes` without the early return for length 0: the kernel panics (see below).

## Run it
```sh
cd course/21-elf
make run
```
After the disk lines:

```text
Hello from ring 3!
Ring 3: /bin/hello ended with Code(42)
Ring 3: general protection fault at 0x400000 - the program is ended
Ring 3: /bin/cli ended with Killed("general protection fault")
```

and in the shell: `run /bin/hello` prints the greeting and `/bin/hello: Code(42)`; `ls /bin` shows `cli` and `hello`.

## What can go wrong
- **No table behind an entry of the kernel's upper half when a space is copied:** a mapping the kernel makes later (a
  heap page, a stack) is then missing in every space made before it. The first counter-check shows exactly this.
- **Freeing a space that is in use:** `free_user_half` refuses the PML4 that CR3 holds, but only on the CPU that runs
  it. That is why `join` waits for the thread to be off its CPU, *and that CPU to have loaded other tables*, before it
  drops the space – the scheduler loads the next thread's tables before it switches.
- **An empty buffer from a null pointer:** `user_bytes` made a slice from whatever address the program passed – for
  `write(0, 0)` a slice of length 0 at the null pointer, which Rust calls undefined behaviour and the debug build
  stops with `unsafe precondition(s) violated: slice::from_raw_parts requires the pointer to be aligned and non-null`:
  *any program could panic the kernel.* This chapter's tests found it (it is bug #12 of the reference kernel, back
  again as bug #32 in `docs/BUGS.md` – chapters 20 and 21 had it). The rule: a length of 0 never looks at the address.
- **Trusting the file:** an ELF file is input like any other. A segment at the end of the address space that wraps
  around, a size larger than the file, an entry point in the data – without the checks of `parse` a file can make the
  kernel map or copy anywhere. (The fixes are the tests above.)
- **`mov esi, length` in Intel syntax:** a bare symbol is a *memory operand*. The first version of `hello.S` loaded
  ESI from the address 0x13 and the program ended with "Ring 3: page fault at 0x40000c" – the instruction that did
  it. `mov esi, offset length` is the number itself.
- **Segments that share a page:** a page has one set of flags. Code and data in one page would be both writable and
  executable, or neither; `load` stops with `AlreadyMapped` and the linker script keeps them a page apart.
- **No `mkfs.fat` or `mtools` on the host,** or no `build/programs`: `mkdisk.sh` says so; `make run` and `make test`
  build the programs first.

## Experiments
1. Write `programs/poke.S` that writes a byte to its own code page (`mov byte ptr [rip], 0x90`, as chapter 20's test
   did) and run it. Which message do you expect? Then make `user.ld` put the code in a segment with `FLAGS(7)`: what
   changes, and who stops it?
2. Predict, then try: a file whose second segment has a `memsz` of 1 GiB, in a test with `build_test_elf`. Where does
   `load` stop, and are the frames back afterwards?
3. Set `STACK_PAGES` to 0. Which of the programs of this chapter still run, and why? What would you have to change
   in `hello.S` to make it fail?
4. Find the limit on the number of program headers in `parse`. What would a file with 65 535 of them cost without it?

## Next
A program is a file on a disk now, and it cannot reach another's memory. What it cannot do yet is *use the disk*:
it can only write to the console. Chapter 22 of the plan gives programs file descriptors and the calls `open`,
`read`, `write`, `close` and `seek`.
