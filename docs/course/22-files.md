# 22 – Files for programs

**Goal:** let a program use the disk – read a file, make one, copy one – without ever being trusted with a path, a
cluster or a kernel pointer. The kernel gives it *file descriptors*.
**You end up with:** the system calls `open`, `read`, `write`, `seek` and `close`, a table of open files for every
program that goes away with it, `FatFs::read_at` underneath, and two programs from the disk: `cat` and `copy`. Every
call checks what it is given and fails with an error number – the kernel survives whatever a program passes. Part VI
continues.
**Code:** [`course/22-files/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/22-files)
(chapter 21 plus `files.rs` and `programs/cat.S`, `programs/copy.S`; `user.rs` gets the new calls, `fat32.rs`
gets `read_at`)
**What changed:** `diff -ru course/21-elf course/22-files` (in the repository) shows every line this chapter adds.

## Background: a number instead of a file
A file system call must not take "the file" from the program. A path is a claim the kernel has to check on every use, and
anything the kernel *hands out* that points into its own data – a cluster number, an address – is something a program
can alter, replay or guess. Unix uses a trick that most systems since have copied: the kernel keeps the open files in a
table *that belongs to the program*, and gives the program the **index** into that table – the *file descriptor*. The number means nothing outside this one program. The program can pass any number it likes; the kernel
looks it up in the program's own table and finds a file, or finds nothing and says `EBADF`.

Three numbers are special by convention: 0, 1 and 2 – input, output and error output. Here 1 and 2 are the console (what
`write` did in chapter 20: it is `write(1, …)` now), 0 is an input that has ended, and `open` hands out 3 and up, the
lowest free number first.

Three more rules every call of this chapter follows – each of them was a bug in the reference kernel (`docs/BUGS.md`):
- **A failing call changes nothing** (#13). A `read` whose buffer is bad must not have moved the file's offset: the
  buffer is checked *first*.
- **The kernel's own memory use does not depend on the program** (#16). One call moves at most 64 KiB; a program that
  asks for 1 GiB gets a *short count* and asks again – the same convention as `read` in Unix. A path is at most 1024 bytes.
- **Errors are values.** A call returns the error as a negative number with Linux's values (`-2` is `ENOENT`, `-9`
  `EBADF`, `-14` `EFAULT` …), so a program can tell "no such file" from "disk full" – and the kernel never panics because
  of what a program did.

## Step 1: reading a file in pieces (`fat32.rs`)
Chapter 17 reads a file as a whole. A program reads 16 bytes, then 16 more – the file system needs a read that starts
anywhere:

```rust,file=kernel/src/fat32.rs
    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let file = self.lookup(path)?;
        if file.is_dir {
            return Err(FsError::IsADirectory);
        }
        let size = file.size as u64;
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let wanted = (buf.len() as u64).min(size - offset) as usize;
        let chain = self.chain(file.cluster)?;
        let cluster_size = self.cluster_size();
        let mut cluster_bytes = vec![0u8; cluster_size];
        let mut done = 0;
        while done < wanted {
            let at = offset as usize + done;
            let cluster = *chain
                .get(at / cluster_size)
                .ok_or(FsError::Corrupt("a file whose chain is shorter than its size"))?;
            self.read_cluster(cluster, &mut cluster_bytes)?;
            let inside = at % cluster_size;
            let take = (cluster_size - inside).min(wanted - done);
            buf[done..done + take].copy_from_slice(&cluster_bytes[inside..inside + take]);
            done += take;
        }
        Ok(done)
    }
```

It returns how many bytes it put into the buffer: fewer at the end of the file, 0 at or behind it. It finds the clusters
by walking the chain from the start every time (still the chain check of chapter 17: a circle is an error, not a hang),
and reads only the clusters the piece touches. A system with open-file objects would remember the walk; ours
stays stateless and keeps this chapter short – the cache of chapter 19 keeps the FAT sectors the walk reads.

## Step 2: three arguments, interrupts on (`user.rs`)
`write` has a new first argument – the file descriptor – so the calls now use RDI, RSI and RDX as Linux does. The
dispatcher:

```rust,file=kernel/src/user.rs
extern "C" fn syscall(registers: &mut Registers) {
    debug_assert_eq!(registers.cs & 3, 3, "only ring 3 calls int 0x80");
    // The gate is an interrupt gate: interrupts are off. A call may take long - a disk read waits for the disk's
    // interrupt - so it runs with them on, like the kernel code of any thread; the thread may even be taken off the
    // CPU in the middle of a call, and goes on later with its own kernel stack (RSP0 follows it, chapter 20).
    unsafe { asm!("sti", options(nomem, nostack)) };
    let (a, b, c) = (registers.rdi, registers.rsi, registers.rdx);
    registers.rax = match registers.rax {
        SYS_WRITE => files::to_register(files::write(a, b, c)),
        SYS_EXIT => end(Exit::Code(a)),
        SYS_READ => files::to_register(files::read(a, b, c)),
        SYS_OPEN => files::to_register(files::open(a, b, c)),
        SYS_CLOSE => files::to_register(files::close(a)),
        SYS_SEEK => files::to_register(files::seek(a, b, c)),
        _ => NO_SUCH_CALL,
    };
}
```

The `sti` is new, and it matters. The gate of `int 0x80` is an interrupt gate: the CPU enters the handler with interrupts
off, and until now every call was short enough not to care. A disk read waits for the disk – chapter 19's commands sleep
until the disk's interrupt arrives, and *poll* with the interrupts off, which would stop this CPU's timer for as long as
it takes. A call now runs with interrupts on, like the kernel code of any thread: the timer may take the CPU away in the
middle of it, and the thread goes on later with its own kernel stack (RSP0 follows the thread, chapter 20).

## Step 3: the table (`files.rs`)
What the kernel remembers of an open file is small – where it is, how far the program is, what it may do:

```rust,file=kernel/src/files.rs
struct Handle {
    path: String,
    /// Where the next `read` starts - and the next `write`, which must be the end of the file.
    offset: u64,
    readable: bool,
    writable: bool,
}
```

and the tables are in one map by the id of the program's thread, made by the first call that needs one and removed when
the program has ended (`join` calls `forget`):

```rust,file=kernel/src/files.rs
static TABLES: Mutex<BTreeMap<task::Id, Table>> = Mutex::new(BTreeMap::new());
```

`open` checks the flags and the path, finds a free slot *before* it touches the disk (a program that cannot use a file
gets no file made), and then does what the flags say with the file system's lock held:

```rust,file=kernel/src/files.rs
pub fn open(address: u64, length: u64, flags: u64) -> Outcome {
    let valid = flags & !ALL_FLAGS == 0
        && flags & (READ | WRITE) != 0
        && (flags & (CREATE | TRUNCATE | APPEND) == 0 || flags & WRITE != 0);
    if !valid {
        return Err(EINVAL);
    }
    let path = user_path(address, length)?;
    // a free slot first: a program that cannot use the file gets no file made
    let slot = with_table(|table| table.iter().position(Option::is_none)).ok_or(EMFILE)?;
    let size = {
        let mut guard = fs::FS.lock();
        let fs = guard.as_mut().ok_or(EIO)?;
        match fs.lookup(&path) {
            Ok(entry) if entry.is_dir => return Err(EISDIR),
            Ok(_) if flags & TRUNCATE != 0 => {
                fs.write_file(&path, b"").map_err(errno)?;
                0
            }
            Ok(entry) => entry.size as u64,
            Err(FsError::NotFound) if flags & CREATE != 0 => {
                fs.write_file(&path, b"").map_err(errno)?;
                0
            }
            Err(error) => return Err(errno(error)),
        }
    };
    let handle = Handle {
        path,
        offset: if flags & APPEND != 0 { size } else { 0 },
        readable: flags & READ != 0,
        writable: flags & WRITE != 0,
    };
    with_table(|table| table[slot] = Some(handle));
    Ok(FIRST_FILE + slot as u64)
}
```

The path is the first thing a program hands over, so the first thing to check: not longer than 1024 bytes, in memory the
program may read, text. `read` – the buffer is checked before anything is read:

```rust,file=kernel/src/files.rs
pub fn read(fd: u64, address: u64, length: u64) -> Outcome {
    #[cfg(test)]
    INTERRUPTS_ON_IN_READ.store(interrupts::enabled(), core::sync::atomic::Ordering::SeqCst);
    if fd == 0 {
        return Ok(0); // the console has no input for programs yet: always the end
    }
    let (slot, handle) = handle(fd)?;
    if !handle.readable {
        return Err(EBADF);
    }
    let length = length.min(MAX_IO);
    // the buffer is checked *before* anything happens: a call that fails changes nothing - the next `read` must
    // still start where the file offset says (bug #13 of the reference kernel)
    let buffer = user::user_bytes_mut(address, length).ok_or(EFAULT)?;
    let mut bytes = vec![0u8; length as usize];
    let count = {
        let mut guard = fs::FS.lock();
        let fs = guard.as_mut().ok_or(EIO)?;
        fs.read_at(&handle.path, handle.offset, &mut bytes).map_err(errno)?
    };
    buffer[..count].copy_from_slice(&bytes[..count]);
    with_table(|table| {
        if let Some(open) = &mut table[slot] {
            open.offset += count as u64;
        }
    });
    Ok(count as u64)
}
```

`write` and `seek` are shaped alike. A `write` to descriptor 1 or 2 goes to the console. A `write` to a file works only
**at its end**: the only way FAT32 of chapter 18 can grow a file is `append`, so a program that has seeked back must
say so with an error (`EINVAL`) instead of getting its bytes in the wrong place. `seek` takes an offset from the start,
the current place or the end of the file, and may go behind the end – a `read` there finds nothing.

## Step 4: the kernel writes into the program (`user.rs`)
`read` is the first call where the kernel writes into the program's memory. The check must be the stricter one: the
kernel must not write where the *program* may not – its own code page, say. (The CPU would stop the kernel too – the
first counter-check below ends in a kernel page fault – but a call must answer a bad buffer with `EFAULT`, not
bring the kernel down.)


```rust,file=kernel/src/user.rs
fn range_allows(address: u64, length: u64, needed: Flags) -> bool {
    let Some(end) = address.checked_add(length) else {
        return false;
    };
    if end > USER_END {
        return false;
    }
    let mut page = align_down(address, PAGE_SIZE);
    while page < end {
        if !paging::flags(page).is_some_and(|flags| flags.contains(needed)) {
            return false;
        }
        page += PAGE_SIZE;
    }
    true
}
```

```rust,file=kernel/src/user.rs
pub fn user_bytes_mut(address: u64, length: u64) -> Option<&'static mut [u8]> {
    if length == 0 {
        return Some(&mut []);
    }
    range_allows(address, length, Flags::USER_WRITABLE)
        .then(|| unsafe { core::slice::from_raw_parts_mut(address as *mut u8, length as usize) })
}
```

## Step 5: two programs
`programs/cat.S` opens `/hello.txt`, reads 16 bytes at a time and writes each piece to the console until `read` returns 0:

```text
# Chapter 22: opens /hello.txt, reads it in pieces of 16 bytes and writes every piece to the console, until `read`
# says 0 - the end of the file. Exit code 0; 1 if a call failed.
# System calls: 1 write(fd, address, length), 2 exit(code), 3 read(fd, address, length), 4 open(path, length, flags),
# 5 close(fd). A negative result is an error.
        .intel_syntax noprefix
        .text
        .globl  _start
_start:
        mov     eax, 4                  # open("/hello.txt", READ = 1)
        lea     rdi, [rip + path]
        mov     esi, offset path_length
        mov     edx, 1
        int     0x80
        test    rax, rax
        js      failed
        mov     ebx, eax                # the file descriptor, in a register the kernel keeps
again:
        mov     eax, 3                  # read(fd, buffer, 16)
        mov     edi, ebx
        lea     rsi, [rip + buffer]
        mov     edx, 16
        int     0x80
        test    rax, rax
        js      failed
        jz      done                    # 0 bytes: the end of the file
        mov     edx, eax                # write(1, buffer, what was read)
        mov     eax, 1
        mov     edi, 1
        lea     rsi, [rip + buffer]
        int     0x80
        jmp     again
done:
        mov     eax, 5                  # close(fd)
        mov     edi, ebx
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

`programs/copy.S` opens the same file for reading and `/copy.txt` with `WRITE | CREATE | TRUNCATE` (2 + 4 + 8 = 14),
copies in pieces of 16 bytes and ends with the number of bytes it copied as exit code – 21 for the test disk's file,
which takes two reads and two writes. Both keep their file descriptors in `rbx` and `r12`: the registers the kernel
promises to leave alone (the others `int 0x80` may change).

## The tests
`make test` runs 178 tests in the kernel (chapter 21: 166). To test many calls with little assembler, the tests drive the
calls from ring 3 with a **script**: a program that reads a table of calls from its data page (the call, three
arguments), makes them one after the other, and writes every result over its call. The kernel fills the table, starts the
program, and when it has ended reads the results out of the program's memory (`Program::join_with`). New:
- `fat32.rs`: a file is read in pieces (the end, behind the end, an empty buffer, an empty file, a directory, a missing
  file) and 777-byte pieces of the 100 000-byte `big.bin` add up to the file – the pieces start and end inside
  clusters and cross them.
- `files.rs`: a file is opened, read in pieces, sought in from the start, the current place and the end, read at the
  end, closed – twice (the second `close` is `EBADF`); descriptors come lowest first until the table is full (`EMFILE`),
  and the lowest free one is reused; **thirty-three calls**, most of them bad, each with the exact result it has to get – missing and
  too-long paths, a path in the kernel, a path that is not text, no flags, flags nobody knows, CREATE without WRITE, a file as a
  directory, a kernel pointer, a buffer in the read-only code, an address that wraps around, descriptors that do not
  exist or are the console, `seek` before the start, an unknown call – and a read after the failures still starts where
  the file's offset was; files are made, written, appended, truncated – and a write at the start of a file that has
  bytes is refused; what a program wrote is on the disk; one call moves at most 64 KiB (1 GiB asked: 65 536 come, then the
  rest of the file, then nothing; and a 100 000-byte write is taken in two pieces); a program that ends – or is killed –
  with files open leaves no table behind; two programs have tables of their own; a system call runs with interrupts on;
  `/bin/cat` prints the file and `/bin/copy` copies it (twice: the second `TRUNCATE`s the copy first).

`make test` also types `run /bin/copy` into the shell and checks with `fsck.fat` and mtools that `/copy.txt` is on
the disk, byte for byte. Four counter-checks, one run each:
- `read` into any page the program may *read* (not only write): the test with the buffer in the program's code page dies
  with a **page fault of the kernel** (`interrupts.rs`) – the kernel tried to write into the read-only page;
- `write` without the "only at the end" rule: `files_are_made_written_and_appended` finds the write at the start of the
  file taken (the call returns 1 where the test expects `EINVAL`, and the next `seek` reports 12 bytes, not 11);
- `join` without `forget`: `a_program_that_ends_closes_its_files` finds one table left;
- no `sti` in the system call: `a_system_call_runs_with_interrupts_on` fails.

**Found on the way (bug #33):** with the new tests the *old* test `our_segments_are_loaded` (chapter 4: is SS the
kernel's data selector?) failed – SS was 0. A probe in a test that runs before it: SS is 16 on all four CPUs before four
programs have run and `[0, 0, 16, 16]` after. An interrupt that comes from ring 3 (a system call, the timer) makes the CPU
load a *null* SS, and nothing loads it again. In 64-bit mode that is legal – the base and limit of the stack segment are
ignored – and nothing in the kernel depends on it, so the test is fixed, not the kernel: SS may be the data segment or null.
(The test never failed before because it ran before any program did. The new tests run earlier in the alphabet.)

## Run it
```sh
cd course/22-files
make run
```
After the disk lines:

```text
Hello from ring 3!
Ring 3: /bin/hello ended with Code(42)
Ring 3: general protection fault at 0x400000 - the program is ended
Ring 3: /bin/cli ended with Killed("general protection fault")
Hello from the disk!
Ring 3: /bin/cat ended with Code(0)
```

and in the shell: `run /bin/copy` prints `/bin/copy: Code(21)`, and `cat copy.txt` shows `Hello from the disk!`.

## What can go wrong
- **A `read` that moves the offset before it knows it can deliver:** a bad buffer would still have eaten the file's
  bytes. The reference kernel's bug #13, and the reason for "check first" above.
- **Trusting the length:** a 1 GiB `read` into a 1 GiB `vec!` is a kernel that stops when somebody asks (#16). `MAX_IO` is
  a rule, not a tuning knob.
- **A kernel write the program could not do:** `user_bytes_mut` must ask for WRITABLE too. (The first counter-check.)
- **Leftover files:** a program that crashes with files open must not leave them open – `join` closes whatever is left.
  Without it, its table stays in the map for good (the third counter-check).
- **`seek` is not `write`:** a program that seeks to the start of a file and writes gets `EINVAL` here, not a silent append –
  and it needs a different file system (chapter 18's writing is append-only) to do what Unix allows.
- **Holding the file system lock while the program's memory is touched:** `read` reads into a kernel buffer and copies
  into the program's buffer only after the lock is released.

## Experiments
1. Write `programs/wc.S` that counts the bytes of `/big.bin` by reading it in pieces of 16 and exits with the count modulo
   256. Predict the exit code (100 000 mod 256 = 160) before you run it. How many `read` calls is that?
2. Make `MAX_IO` 16 and run `cat`. Why does it still work? Which of the tests now fail, and what do they say about the
   rule?
3. Let `read` return the error `EINTR` (−4) once, the first time it is called. What must `cat.S` do for the file still to
   arrive whole?
4. In the reference kernel (`kernel/src/process/syscall.rs`) find `target`: it copies the handle out of the process table
   before the disk is read. This chapter's `handle()` does the same with its table. Why must the table lock not be
   held while the disk is read?

## Next
A program can use the disk, the console and the CPU; what it cannot do yet is *get memory*: its stack is fixed and its
data segment is whatever its file says. Chapter 23 of the plan gives programs a heap – `brk` – whose pages are mapped on
the first touch.
