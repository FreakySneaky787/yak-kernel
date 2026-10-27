# Design decisions

## Basics
| Topic | Decision | Why |
|---|---|---|
| Language | Rust nightly, `#![no_std]`, some inline assembly | memory safety; nightly is needed for `custom_test_frameworks` and `abi_x86_interrupt` |
| Target | `x86_64-unknown-none` (built-in) | bare metal, soft-float (no SSE in the kernel) → no custom target JSON needed |
| Linker | **GNU ld** (`ld.bfd`, binutils 2.46), flavor `ld` | my preference for GNU tools; linker script `kernel/linker.ld` |
| Architecture | x86_64 | matches the host PC, best documentation (OSDev, Intel SDM) |
| Bootloader | Limine v11, boot protocol base revision 6, crate `limine` 0.6.5 | starts directly in long mode with paging, provides memory map, HHDM and framebuffer |
| Boot medium | hybrid ISO (BIOS + UEFI) built with xorriso | boots in QEMU with `-cdrom`, both firmware paths testable |
| External crates | only `limine`, `spin` | everything else is written from scratch |
| Tools | GNU make, gdb, binutils (objdump/nm/readelf/addr2line), gcc for host helpers | my preference: Rust kernel, GNU tooling around it |

## Memory layout
- The kernel lives in the higher half at `0xffffffff80000000` (top 2 GiB), `code-model=kernel`, `relocation-model=static`.
- Limine additionally maps all physical memory at the HHDM offset (`boot::HHDM`):
  physical address `p` is reachable at `p + hhdm_offset`.
- Linker sections: `.limine_requests` (between start and end marker), `.text`, `.rodata`, `.data`/`.bss`,
  each page-aligned with its own PT_LOAD segment.

## Physical memory
- `mem::pmm` is a bitmap allocator: one bit per 4 KiB frame (1 = used), covering physical memory up to the end of the highest usable region.
- The bitmap lives in the first usable region big enough for it and is reserved in itself. Frame 0 is never handed out.
- The bitmap covers `USABLE` and `BOOTLOADER_RECLAIMABLE` memory; the latter starts as used (it holds Limine's stack, page
  tables and responses) and is handed over by `mem::reclaim_bootloader_memory` (M33) at the start of `kernel_main`:
  page tables living there are copied (`paging::copy_kernel_tables` – tables elsewhere are kept and only repointed), every
  thread's saved CR3 and every CPU's CR3 move to the new PML4, then `pmm::release_region` frees the frames. Limine
  responses must not be read afterwards – `pmm::init` keeps a copy of the memory map (`pmm::boot_memmap`).
  Test kernels other than `tests/reclaim.rs` do not reclaim (they still run on Limine's stack).
- All access to physical memory goes through the HHDM (`PhysAddr::to_virt`). Allocator calls run with interrupts disabled.
- `free_frame` panics on double free / invalid addresses – that is always a kernel bug.

## Virtual address space
| Range | PML4 slot | Use |
|---|---|---|
| `0x0000_0000_0000_0000` – `0x0000_7FFF_FFFF_FFFF` | 0–255 | user space: private per process (see "User space") |
| `0xFFFF_8000_0000_0000` + phys | 256 | HHDM (Limine): all RAM + framebuffer, **no MMIO** |
| `0xFFFF_FE00_0000_0000` | 508 | kernel heap (grows upward, max 1 GiB) |
| `0xFFFF_FE80_0000_0000` | 509 | scratch area for paging unit tests |
| `0xFFFF_FF00_0000_0000` | 510 | kernel stacks: `[guard page][stack pages]` per stack, never reused |
| `0xFFFF_FFFF_8000_0000` | 511 | kernel image (linker script) |

- `paging::ensure_hhdm_mapped(phys, size, flags)` maps pages Limine left out of the HHDM (ACPI tables, LAPIC `0xFEE0_0000`,
  I/O APIC `0xFEC0_0000`) at their usual HHDM address, so every physical address is still reached as `phys + hhdm_offset`.
- Page tables: we modify Limine's tables through the HHDM; intermediate tables come from the frame allocator and are not freed.
- `paging::init` remembers Limine's PML4 as `kernel_root()` and creates **all 256 upper-half PML4 entries** up front (empty PDPTs).
  Every process PML4 copies these entries, so later kernel mappings (heap growth, new stacks, MMIO) land in shared lower-level
  tables and are visible in every address space. The PML4 entries themselves never change after boot.
- Limine sets the USER bit on its upper-half intermediate tables (leaves are supervisor-only). `paging::init` clears USER in all
  upper-half PML4 entries, so ring 3 cannot reach the kernel half, whatever the lower levels say.
- Limine enables EFER.NXE (so `NO_EXECUTE` is usable) and CR0.WP (the kernel cannot write read-only pages either).
- `kmain` switches from Limine's stack to a 64 KiB guarded stack (`mem::stack`) immediately after `kernel::init()`.

## Threads and scheduling
- **SMP (since M26)**: `current[cpu]` / `idle[cpu]` per CPU, one shared ready queue, every CPU's LAPIC timer preempts. Each
  thread has `on_cpu` (set while a CPU executes it or is still switching away from it). `schedule()` takes the scheduler lock,
  picks the next thread, *forgets the guard* and switches; the next thread calls `finish_switch()`, which clears the previous
  thread's `on_cpu` and releases the lock (`force_unlock`). New threads do that first thing in `thread_start`.
- **Blocking**: `wait_for(reason, ready)` marks the current thread `Blocked(reason)`, then evaluates `ready()` (interrupts off,
  scheduler lock *not* held – `ready` may take other locks), then schedules. Wakers (`notify`, `wake`, `make_ready`): a blocked
  thread that is still `on_cpu` is set back to `Running` (it will see its condition), otherwise it is queued and an idle CPU
  gets a `WAKE_VECTOR` IPI. Lock order: `PROCESSES` → `SCHEDULER`; pipe/keyboard/serial locks are leaves.
- TLB: kernel virtual ranges are never reused after unmapping (stacks) or never unmapped (heap), so the kernel half needs
  no shootdowns. User pages of a process with threads may be cached on several CPUs: `sys_brk` unmaps without freeing
  (`unmap_keeping_frames`), calls `paging::shoot_down(root)` (a `run_on` job per CPU that reloads CR3 if it runs in
  `root`) and only then frees the frames. A CPU always reloads CR3 when it switches address spaces.
- **Sleeping lock** (`sync::Mutex`, M36): CAS on a flag; a contended `lock` blocks on `Wait::Lock(address)` (spins
  only before the scheduler runs or with interrupts off); unlock notifies when the waiter count is non-zero. Used for
  `drivers::DISK` and `fs::ROOT`, whose holders block on the AHCI interrupt during I/O. Not for interrupt handlers.
- `task::Scheduler` (behind a spin lock, only touched with interrupts disabled): `BTreeMap<Tid, Box<Thread>>` (boxed so the saved
  RSP has a stable address), a FIFO ready queue, the current TID and the idle TID. Thread 0 = the boot code ("main"), runs the shell.
- Context switch: `kernel_switch_context(old_rsp_ptr, new_rsp)` pushes rbp/rbx/r12–r15, swaps RSP, pops, `ret`. A new thread's stack
  is prepared so that `ret` lands in `kernel_thread_trampoline`, which calls `thread_start(entry)` with the boxed closure from r12.
- `schedule()` must run with interrupts disabled. A resumed thread continues with interrupts off and restores them itself (via
  `without_interrupts`, `iretq` of the interrupted handler, or `sti` in `thread_start`).
- Preemption: every timer tick, if another thread is ready. The timer handler sends EOI first, then calls `task::on_tick`.
- Wake-ups: timer (`Until`), keyboard/COM1 IRQ on the BSP (`Input`), `exit` (`Join`), pipes (`Pipe(id)`), `kill` (`wake`).
- Dead threads are reaped (stack unmapped and frames freed) by a later `schedule()` call – never while running on their stack.

## Storage
- PCI: legacy configuration mechanism (ports 0xCF8/0xCFC), brute-force scan of all bus/device/function numbers.
- AHCI (q35's ICH9 controller, BAR5): one driver instance per port, probed **once** at boot into `drivers::DISK`.
  Per port: one frame for command list + received FIS, one for the command table (FIS + 1 PRD), one 4 KiB DMA bounce frame.
  Commands are polled (no interrupts yet) and limited to 8 sectors; larger requests are split.
- `drivers::DISK` is a spin lock held with interrupts enabled during I/O – never use it from interrupt handlers.
- File systems only see the `BlockDevice` trait (512-byte sectors); tests use `RamDisk`.
- Disk images: FAT32 without partition table ("superfloppy"), 64 MiB, 512-byte clusters (FAT32 needs ≥ 65525 clusters),
  created by `scripts/mkdisk.sh` from `rootfs/`. QEMU: `-device ide-hd,bus=ide.0` = AHCI port 0 (the boot CD is ATAPI on port 2).
  Run mode uses the persistent `build/disk.img`; every test binary and the E2E test get fresh images.

## Disk caching and interrupts (M32)
- The root file system runs on `BlockCache<SystemDisk>`: write-through (the disk is always current, a power-off loses
  nothing), 1024 sectors, LRU eviction of the oldest eighth when full. Runs of missing sectors are fetched with one request.
- `FatFs` additionally keeps the last cluster chain and the last path lookup; any write to the volume drops both.
- AHCI: MSI to the boot CPU's LAPIC on vector 0x50; the handler acknowledges all ports, remembers task-file errors per port
  and notifies `Wait::Disk`. Commands wait with `wait_for` when interrupts are on and the scheduler runs, else they poll.
  The `DISK` lock stays held while waiting (other threads wanting the disk spin; fine for now).

## File system
- One FAT32 volume on the system disk, mounted at boot as `fs::ROOT` (a spin lock like `DISK`: never from interrupt handlers).
  `FatFs<D: BlockDevice>` is generic, so the same code runs on `SystemDisk` (adapter for `drivers::DISK`) and on `RamDisk` in tests.
- Mount: FAT32 boot sector at LBA 0 (superfloppy), otherwise the first MBR partition of type 0x0B/0x0C. Only 512-byte sectors.
- Caching: every sector – FAT and data – goes through the write-through `BlockCache` below the file system (see "Disk
  caching and interrupts"); `FatFs` itself keeps the FAT sector it used last, the last cluster chain and the last path
  lookup. FAT updates are written through to **every** FAT copy immediately. Every mutating operation ends with `sync`
  (FSInfo hints + disk flush), so a `poweroff` or reset right *after* a command never leaves the volume inconsistent.
  A power loss or disk error *during* a command is a different case. What holds then is measured (M53,
  `fs::fat32::fault_tests`: every I/O of create, replace, append, `write_at`, mkdir, rename and remove made to fail
  once, as a transient error and as a power loss with a torn write, then the volume re-mounted and checked):
  - the command returns `FsError::Io`, never panics, and the volume mounts again;
  - files the command did not touch are intact; its target is in the old or the new state (`write_at` may have
    written part of its bytes);
  - the damage is what `fsck.fat` repairs without losing data that was there before: lost clusters (new clusters not
    yet in a directory entry, or old ones not yet freed), the two FAT copies differing in one sector, after
    append/`write_at` a chain longer than the file size, after rename both names pointing to the same clusters
    (the new entry is written before the old one is freed – the order Linux's vfat uses too);
  - after a transient error the same mount keeps working (later writes do not damage the volume).
  There is no journal: FAT32 has none, and the kernel does not run `fsck` at boot.
- Names: a name that fits 8.3 in one case per part (`hello.txt`, `README`) is stored as a plain short entry with the NT
  lower-case flags, like Windows/Linux do. Everything else gets LFN entries plus a unique 8.3 alias (`MYNOTE~1`).
  Lookup is case-insensitive on long and short names.
- Paths at the FS API are absolute and normalized (`/a/b`); the shell owns the current directory and calls `fs::normalize`.
- `rm` refuses non-empty directories; `mv` only rewrites directory entries (and the `..` entry of moved directories).
- Timestamps come from the CMOS RTC (`drivers/rtc.rs`, treated as UTC).
- Validation outside the kernel: after `make test`, `scripts/check-test-disks.sh` runs `fsck.fat -n` on every test disk and
  checks kernel-written files with mtools; the E2E test does the same for its disk after two boots.

## User space
- **Address space** (`mem::address_space::AddressSpace`): own PML4, user half empty, kernel half shared. Frames are filled via the
  HHDM, so loading never switches CR3. `Drop` frees every user frame and page table (`paging::free_user_half`); all user
  mappings own their frames. It must not be the active address space when dropped.
- **User layout**: ELF segments between `USER_IMAGE_START` (64 KiB, null page area stays unmapped) and `USER_IMAGE_END`;
  our programs link at `0x40_0000`. The heap starts at the first page after the image and is moved with `brk` (max 64 MiB). Stack: up to 1 MiB
  (`USER_STACK_MAX`) below `USER_STACK_TOP = 0x7FFF_FFFF_0000`, the top 4 pages mapped at start, the rest on demand (M35);
  nothing is mapped below it (overflow → page fault → process killed). Below that gap: 64 **thread stack slots**
  (M52), each `[64 KiB guard][256 KiB stack]`; a slot belongs to a thread started with `thread_spawn(entry, 0, arg)`,
  its stack pages are demand-paged, the guard never. A fault in a guard kills the process with "stack overflow in a
  thread". Slots of ended threads are unmapped (TLB shoot-down first) at the next `thread_spawn` or `thread_join`.
  `USER_IMAGE_END` leaves room for the largest heap plus a guard below the slots (bug #21). The argument block is copied to the stack top; `_start` gets pointer and length
  in RDI/RSI. Arguments inside the block are separated by NUL bytes (`ulib::Args`); the shell joins its words that way.
- **ELF loader** (`process::elf`): static `ET_EXEC` x86_64 only, `PT_LOAD` segments with R/W/X → USER, WRITABLE, NO_EXECUTE.
  Every header field is bounds-checked; segments outside the user image area or an entry point outside an X segment are refused.
- **Processes** (`process::PROCESSES`, interrupts off while locked): one kernel thread per process. `task::spawn_user` creates a
  thread with its own CR3 and a `UserEntry`; `thread_start` drops the closure and then `iretq`s to ring 3 (`enter_user`).
  The thread's kernel stack is abandoned at that point and reused from its top by every interrupt/syscall from ring 3.
- **Scheduler**: each thread has `cr3`; on a switch the scheduler loads it (if different) and sets TSS.RSP0 + the syscall stack
  to the next thread's kernel stack top (`syscall::set_kernel_stack`). Switching CR3 on the old thread's stack is fine: kernel
  stacks are in the shared half.
- **End of a process**: `exit` syscall, CPU exception with CS.RPL = 3 (handlers call `process::kill_current` instead of panicking),
  or a fatal signal (`process::deliver_signals`, run when the timer/wake IPI interrupts user code and on every syscall return). `exit_current` records the
  status, moves the thread to the kernel CR3, frees the address space and exits the thread. The status stays in the table until
  `process::wait` (zombie); the shell always waits.
- **System calls** (`syscall`/`sysret`): STAR = kernel CS 0x08 / sysret base 0x10, LSTAR = `syscall_entry`, FMASK clears
  IF/TF/DF/AC. The stub does `swapgs` (IA32_KERNEL_GS_BASE = this CPU's `CpuLocal`), parks the user RSP in `gs:[0]`, loads the
  kernel stack from `gs:[8]`, pushes the user RSP and swaps GS back *before* enabling interrupts (a preempted syscall may
  resume on another CPU, so GS must never stay swapped), saves a
  `SyscallFrame`, enables interrupts and calls `process::syscall::dispatch`. Convention like Linux: number in RAX, args in
  RDI/RSI/RDX/R10/R8/R9, result in RAX, negative = `-errno`; only RCX/R11 are clobbered. Syscalls may block (read, sleep).
  | # | Name | |
  |---|---|---|
  | 0 | `exit(code)` | never returns |
  | 1 | `write(fd, buf, len)` | fd 1/2 → console (or a capture buffer in tests) |
  | 2 | `read(fd, buf, len)` | fd 0 → keyboard/serial keys as terminal bytes (`\n`, `0x08`, UTF-8), blocks for ≥ 1 byte |
  | 3 | `sleep_ms(ms)` | `-EINTR` when a signal arrives |
  | 4 | `getpid()` | |
  | 5 | `uptime_ms()` | |
  | 6 | `yield()` | |
  | 7 | `open(path, len, flags)` | flags READ=1, WRITE=2, CREATE=4, TRUNCATE=8, APPEND=16; a directory opens for `readdir` |
  | 8 | `close(fd)` | |
  | 9 | `readdir(fd, *DirEntryInfo)` | 1 = entry written, 0 = end; 296-byte `repr(C)` struct shared with `ulib` |
  | 10 | `stat(path, len, *Stat)` | 32-byte `repr(C)`: size, kind (1 file, 2 dir), modified time |
  | 11 | `getcwd(buf, len)` | `-ERANGE` if too small |
  | 12 | `unlink(path, len)` | files and empty directories |
  | 13 | `mkdir(path, len)` | |
  | 14 | `brk(new_end)` | 0 = query; growing only moves the end (pages come on first touch), shrinking frees used pages; `-ENOMEM` outside the range |
  | 15 | `spawn(path, len, args, args_len)` | 4th argument in R10; child inherits cwd and output; `-ENOENT`, `-ENOEXEC`, `-E2BIG` |
  | 16 | `wait(pid, *WaitStatus)` | children only (`-ECHILD`); 128-byte status: kind (1 exited, 2 killed), code (exit code / signal number), kill reason |
  | 17 | `kill(pid, sig)` | any user process (`-ESRCH`); sig 0 = existence check |
  | 18 | `chdir(path, len)` | `-ENOTDIR` for files |
  | 19 | `kshell(line, len)` | transitional: runs a kernel shell built-in (not cd/pwd/history/run) in the caller's cwd and output |
  | 20 | `kcommands(buf, len)` | NUL-separated names usable with `kshell` |
  | 21 | `pipe(*[u32; 2])` | read end, write end |
  | 22 | `dup(fd)` | lowest free fd for the same object |
  | 23 | `dup2(fd, new_fd)` | closes what was at `new_fd` |
  | 24 | `isatty(fd)` | 1 for the console (stdin, stdout to the console), else 0 |
  | 25 | `sysinfo(*SysInfo)` | 176 bytes: uptime, ticks, memory, kernel heap, FS size/free/label, CPUs, CPU vendor/brand, date |
  | 26 | `proclist(*[ProcInfo], max)` | one 64-byte entry per thread (tid, cpu ticks, parent, state, user?, name) |
  | 27 | `rename(from, len, to, len)` | |
  | 28 | `seek(fd, offset, whence)` | SET/CUR/END for files, `-ESPIPE` otherwise; past the end is allowed (gap = zeros on write) |
  | 29 | `termsize()` | console rows << 32 \| columns (the serial side has no size; programs draw for the console) |
  | 30 | `ignore_interrupt(on)` | Ctrl+C arrives as 0x03 instead of killing the caller (the editor) |
  | 31 | `net_info(*NetInfo)` | 40 bytes: MAC, link, IPv4, netmask, gateway, frame counters; `-ENODEV` without a card |
  | 32 | `ping(ip, seq, timeout_ms)` | round trip in µs; `-ETIMEDOUT`, `-EHOSTUNREACH` |
  | 33 | `udp_socket(port)` | 0 = ephemeral (≥ 0xC000); `-EADDRINUSE` |
  | 34 | `sendto(fd, buf, len, ip, port)` | ≤ 1472 bytes, `-EMSGSIZE` above |
  | 35 | `recvfrom(fd, buf, len, *SockAddr, timeout_ms)` | 5th argument in R8; `-ETIMEDOUT`, `-EINTR` |
  | 36 | `clock_us()` | microseconds since boot (TSC) |
  | 37–43 | TCP, `shutdown`, `setenv`/`getenv`/`environ` | see `kernel/src/process/syscall.rs` |
  | 44 | `sigaction(sig, disposition, trampoline)` | `SIG_DFL` 0, `SIG_IGN` 1 or a handler; old disposition; SIGKILL → `-EINVAL` |
  | 45 | `sigreturn()` | called by the trampoline when a handler returns (unblocks the next one) |
  | 46 | `thread_spawn(entry, stack, arg)` | tid; ≤ 64 running further threads (`-EAGAIN`); entry/stack must be user addresses; stack 0 = a kernel slot with a guard |
  | 47 | `thread_exit(code)` | ends the thread; in the main thread the process |
  | 48 | `thread_join(tid)` | exit code as u32; `-ESRCH` if not a further thread of the caller or already joined |
  | 49 | `futex_wait(addr, expected, timeout_ms)` | `-EAGAIN` if `*addr != expected`, `-ETIMEDOUT`, `-EINTR` |
  | 50 | `futex_wake(addr, n)` | number of threads woken |
  | 51 | `gettid()` | `getpid()` returns the process id in every thread |
  | 52 | `uname(buf, len)` | "YAK <version> x86_64", `-ERANGE` if `len` is too small (the example of `docs/HACKING.md`) |
- **`/bin/edit`**: `buffer.rs` is pure logic (tested by `edit --selftest`), `main.rs` draws with `ESC[r;cH`, `ESC[K`, SGR 7.
  The text area is all rows but the last; the cursor line and the status bar are redrawn per key, the whole screen only after
  scrolling or when lines are inserted/removed. Writing the bottom-right cell is safe: the console wraps lazily (only when the
  next character comes).
- **Pipes** (`process::pipe`): bounded queue (64 KiB) + reader/writer counts. A descriptor holds a `PipeEnd`; `Clone`
  (dup, dup2, spawn) increments, `Drop` (close, process exit) decrements and notifies. Read blocks while empty with writers,
  returns 0 without; write blocks while full with readers, `EPIPE` without. Waiters sleep on `Wait::Pipe(id)`; the pipe lock is
  only taken with interrupts disabled (descriptors are dropped under the process table lock). Kills abort the wait (`EINTR`).
- **Descriptor inheritance**: `spawn` clones the parent's fds 0–2 into the child (other fds are not inherited). A shell
  redirects by `dup2`-ing a pipe end or file over 0/1 before `spawn` and restoring its saved copies afterwards.
- **Process tree**: `parent` (None = started by the kernel; only the kernel waits for those). When a process ends, finished
  children are dropped and running ones become orphans, removed from the table at their own exit (nobody waits for them).
- **Threads** (M36): `thread_spawn(entry, stack, arg)` starts a kernel thread with the process's CR3 whose `owner` is the
  main thread; `task::current_owner()` is the pid, so `with_current`, `getpid`, files and signals are per process.
  `Process::threads` maps further tids to `None` (running) / exit code (until `thread_join`). The end of a process
  (`exit` in any thread, a fatal signal or a fault) sets `exiting`; `signal_pending()` is then true for every thread
  (blocking calls give up) and `deliver_signals` sends any thread that is about to enter ring 3 into `exit_current`.
  A further thread just wakes the others and exits; the main thread joins all further threads, then records the status,
  frees the address space and exits – so `wait` (which joins the main thread) only returns when every thread is gone.
  Signals are taken by whichever thread returns to user mode first.
- **Futexes** (`process/futex.rs`): `futex_wait(addr, expected, timeout)` reads the word under the `FUTEXES` lock (inside
  `with_current`, so no other thread can unmap it meanwhile), queues the thread under the key (pid << 47 | addr) and
  blocks (`Wait::Futex`, or `Wait::Until` with a timeout); `futex_wake(addr, n)` moves up to n waiters to the `woken`
  set and `task::wake`s them. `ulib::sync::RawLock` (0 free / 1 locked / 2 contended) only calls the kernel when
  contended; the user heap uses it, `ulib::thread::spawn` asks for a kernel stack slot (256 KiB with a guard, see "User layout").
- **Signals** (M34, `process/signal.rs`): per process a disposition per signal (default = end, ignore, handler), a pending
  mask and an `in_handler` flag. `send_signal` sets the bit and `task::wake`s the target; blocking syscalls use abortable
  waits (`input::read_key_or_abort`, `task::join_or_abort`, `time::sleep_ms_or_abort`, pipes, sockets) that give up when
  `signal_pending()` and return `EINTR`. Delivery happens on the way back to ring 3 – at the end of `dispatch` (it may
  change `SyscallFrame.rip/rsp`) and in the timer / wake-IPI handlers (they change the CPU-pushed `InterruptStackFrame`
  with volatile writes). SIGKILL first, then the lowest pending signal; default → `kill_current(reason)` (exit status
  `Killed(reason)`, `wait` reports the number via `signal::number_for_reason`, exceptions = SIGSEGV).
- **Handlers without a kernel register frame**: the kernel only pushes `sig, handler, rflags, rip` below the user RSP and
  jumps to the trampoline registered with `sigaction` (`ulib::signal`). The trampoline pushes all 15 GPRs and XSAVEs (or FXSAVEs), calls
  the handler, calls `sigreturn`, restores everything and ends with `popfq; ret` (which also restores RSP). So neither the
  syscall stub nor the interrupt handlers need a full register frame. Price: no red zone in user code (the target sets
  `disable-redzone`; the kernel writes right below RSP). While a handler runs, further handled signals stay pending
  (default actions still happen); a signal arriving during the trampoline's epilogue nests safely, because everything below
  RSP has been consumed by then. The trampoline must be a user address (`< USER_END`, it becomes a `sysret` target).
  `spawn` resets handlers but keeps ignored signals (like `exec`). `FOREGROUND` = pid owning the console: set by the kernel shell for its programs, inherited by
  `process::wait_child` while the foreground process waits for that child (handed back when the wait ends); `spawn` alone never
  changes it (bug #4). The keyboard and COM1 interrupt handlers recognise
  Ctrl+C themselves (raw 0x03 / Ctrl tracker on set-1 scancodes, the C key is 0x2E on QWERTY and QWERTZ) and call
  `process::interrupt_foreground`: the key is swallowed and SIGINT sent (default: killed, "interrupted"), unless the
  process has `ignore_interrupt` and no SIGINT handler – then the key reaches it as byte 0x03.
- **init**: `kernel_main` runs `process::run_init("/bin/sh")` (foreground, `ignore_interrupt`) and waits; when it is missing or
  exits, the kernel shell runs on thread 0 as before.
- **`/bin/sh`** (`user/src/bin/sh/`): compiles the kernel's `shell/line_editor.rs` and `shell/parser.rs` with `#[path]`; both
  sides provide a `keys` module (`Key`, `KeyEvent`) next to them. Pipelines: for each stage, stdin = previous pipe (or `<`
  file), stdout = new pipe (or `>`/`>>` file), then the stage runs (built-in inline, program spawned); all children are waited
  for in order, only the last exit code is reported, kills always; an "interrupted" stage kills the rest. `read(0)` delivers keys as terminal bytes (VT100 sequences
  for arrows, Home/End, Delete, PageUp/Down, Insert); `keys::Decoder` turns them back into `KeyEvent`s. Order of lookup:
  sh built-ins, functions, kernel built-ins (`kshell`), program path / `$PATH`.
- **sh grammar** (M35, `syntax.rs`): the text is tokenized into *raw* words (quotes kept, `$((...))` one word) and
  separators, then parsed into `List` → `AndOr` → `Pipeline` → `Command` (simple / compound + redirections / function
  definition). Words are expanded only when a command runs (`expand_words`: variables, arithmetic, wildcards, then the
  kernel's `parser::split`), so loops see new values. Interactive input that parses as `Incomplete` (open quote,
  missing `fi`/`done`/`}`) continues with the prompt `> `; scripts and `sh -c` are parsed as a whole first.
  Execution returns a `Flow` (`Next`, `Exit`, `Break(n)`, `Continue(n)`, `Return`, `Interrupted`) up the call chain.
  Compound commands and functions in a pipeline run inside the shell like built-ins. `set -e` is ignored inside
  `if`/`while` conditions and for all but the last pipeline of `&&`/`||`. Ctrl+C: the shell's SIGINT handler (and a
  child killed by SIGINT) sets `INTERRUPTED`, which every list and loop checks; at the prompt the line is cancelled.
  Wildcards: the expander marks unquoted `*`/`?` with private-use characters, `glob.rs` matches them per path component.
- **Command substitution and `"$@"`** (M55): `$(...)` stays inside its word when tokenizing (`syntax::subst_end` skips
  quotes and nested parentheses). At expansion `Shell::substitute` parses the text and runs it *in the shell* with
  stdout on a pipe, while a second thread (`ulib::thread`) drains the pipe – so the output can exceed the 64 KiB pipe.
  Afterwards variables, the directory, functions and `$@` are restored, which gives subshell semantics without a new
  process (functions keep working inside); `exit` ends only the substitution, `$?` is its last status. Because expansion
  produces text that `parser::split` splits again, substituted output and arguments are inserted *quoted*: in double
  quotes as one escaped string, unquoted as one quoted word per field, after `NAME=` as one word (no splitting).
  `"$@"` closes and reopens the quotes between the arguments; without arguments it produces no word. Variables live in
  the kernel's per-process environment: values up to 4096 bytes, up to 256 variables – a larger assignment is an error.
- **`case` and `local`** (M56): `;;` is its own token; a pattern is a raw word ending in `)` (an optional leading `(`,
  alternatives split at unquoted `|`), its body a list ended by `;;` or `esac`. Subject and patterns are expanded without
  splitting; unquoted `*`/`?` in a pattern are wildcards (`glob::matches`), no `[...]` classes. `local` records the
  variable's old value in the running function's frame (`Shell::locals`); `call` restores the frame in reverse order when
  the function returns, so nested functions see the caller's locals (dynamic scope, like bash).
- **Files**: each process has a cwd (inherited from the spawner, paths are normalized against it) and up to 32 file descriptors
  (`process::Handle`): 0 stdin, 1/2 the process's `Output`, then files (path + one offset for reads and writes, shared
  with `dup`-ed and inherited copies like a POSIX open file description – the shell's save/restore of fds 0-2 depends
  on it; writes use `FatFs::write_at`, or `append_file` with the APPEND flag)
  and directories (snapshot of the entries at `open`). Handlers copy what they need out of the table, do the disk I/O
  without holding `PROCESSES` (only `fs::ROOT`), then update the descriptor. `read` of a file uses `FatFs::read_at`, ≤ 64 KiB per call.
  FS errors map to Linux errno values (`ENOENT`, `EISDIR`, ...).
- **User pointers**: every buffer is checked page by page against the process's page tables (present + USER, + WRITABLE for
  `read`) before the kernel touches it; then it is accessed directly (the process CR3 is active) inside
  `cpu::user_access` (M37): with SMAP that is `stac` … `clac`, the only window in which the kernel may touch user pages
  (`copy_from_user`, `copy_to_user`, the futex read). SMEP keeps the kernel from executing user pages. Both are enabled
  per CPU by `cpu::enable_protection` when CPUID has them; QEMU runs `-cpu max` (TCG) so they are there
  (`KERNEL_CPU=qemu64` runs without SMEP/SMAP/XSAVE/AVX - the fallback paths). `tests/smap.rs` checks that a kernel
  read and a kernel jump into a user page fault.
- **User programs** (`user/`, crate `ulib` + `src/bin/*`): own target `user/x86_64-yak-user.json` (x86-64 with SSE2 and
  the normal hard-float ABI, static, `code-model=small`, red zone allowed – interrupts from ring 3 switch stacks), built with
  `-Zbuild-std=core,alloc` (+ `compiler-builtins-mem`; needs `rust-src` and `gcc` as host linker for build scripts),
  linked by GNU ld at `0x40_0000` (`user/linker.ld`). `make user` copies them to `build/user/bin`; `scripts/mkdisk.sh`
  and (for the persistent disk) `run-qemu.sh` install them into `/bin` via `scripts/install-user.sh`.
- **User heap** (`ulib::heap`): first-fit allocator, free list sorted by address with coalescing, 16-byte units, grows the
  break by ≥ 64 KiB at a time. No lock (programs are single-threaded).
- **Demand paging** (M33, stack since M35): the heap `[heap_start, align_up(heap_end))` and the stack area are only reserved ranges. A user page fault with the
  present bit clear inside it maps one zeroed RW/NX page (`process::handle_page_fault`, called first in the page-fault
  handler) and returns, so the instruction is retried; any other fault, or no free frame, kills the program as before.
  Syscalls call `Process::populate` for user buffers before `is_accessible`, so the kernel never faults on them.
- **Environment** (M31): `Process::env` (BTreeMap), copied into children by `spawn`; syscalls `setenv`/`getenv`/`environ`.
  `/bin/sh` keeps no variables of its own – shell variables *are* environment variables.
- **Shell**: a word that is not a built-in is a program path or a name in `/bin`; `run <path> [args]` does the same explicitly.
  Programs run in the shell's cwd. `ls`, `cat` and `cp` are programs (no built-ins any more). Non-zero exit codes and kills
  are reported as errors. `Shell::capture_programs` makes program output go to the command's `out` (unit tests).
- **FPU/SSE/AVX** (`arch::x86_64::fpu`): the kernel is soft-float and never uses x87/MMX/XMM/YMM, so user register contents
  survive syscalls and interrupts untouched. Each thread has a boxed, 64-byte aligned 1 KiB `FpuState` (initialised from a
  clean image taken after `fninit` + `ldmxcsr 0x1F80`); `schedule()` saves the outgoing (unless dead) and restores the
  incoming thread's state right before `kernel_switch_context`. With XSAVE (M37): CR4.OSXSAVE, XCR0 = x87 | SSE | AVX
  (set by `xsetbv` on every CPU), `xsave64`/`xrstor64` with the XCR0 mask, area size from CPUID 0xD (832 bytes, must fit
  1 KiB); without XSAVE `fxsave64`/`fxrstor64`. No lazy switching (CR0.TS stays clear), no AVX-512.
- **User programs and AVX**: the user target stays at SSE2, so every program runs on any CPU; code that wants AVX checks
  `ulib::avx_enabled()` (CPUID OSXSAVE + XCR0) and uses `#[target_feature(enable = "avx")]` (`fputest`, `sigtest`).
  The signal trampoline saves with `xsave64` (header zeroed first) when `ulib::XSAVE_ENABLED`.

## Time
- `time::ticks` (100 Hz, boot CPU's LAPIC timer) for sleeping and timeouts; `time::uptime_us` from the TSC (frequency measured
  at boot against PIT channel 2) for precise time stamps. All CPUs' TSCs are assumed to run in step.

## Network
- **Driver** (`drivers/e1000.rs`): Intel 82540EM as emulated by QEMU, BAR0 MMIO (128 KiB, mapped via `ensure_hhdm_mapped`).
  RX ring 32 × 16-byte descriptors (one frame), TX ring 8; DMA buffers 2 KiB (two per frame). RCTL: broadcast accepted, CRC
  stripped; TCTL: pad short packets. TX descriptors start with DD set so "DD = slot free". Interrupts (M38): no MSI on this model,
  and the INTA# → I/O APIC wiring would need the ACPI `_PRT` (AML). `net::probe_irq` routes each free PCI input (GSI 16-23,
  level-triggered, active low) in turn to vector 0x51 and makes the card raise "link status change" by software (`ICS`);
  the input where it arrives is kept. The handler reads `ICR` through a register base saved at probe time (no NIC lock:
  the net thread holds it with interrupts on), which clears the causes and releases the level-triggered line, sets
  `RX_PENDING` and wakes the net thread. Enabled causes: RXT0, RXO, RXDMT0, LSC. No interrupt found → polling.
- **Stack** (`net/`): configured by DHCP at boot (`net/dhcp.rs`), static fallback = QEMU's slirp defaults (10.0.2.15/24,
  gateway and host = 10.0.2.2, DNS 10.0.2.3). TCP (`net/tcp.rs`) is described in its module docs; packets to our own address
  go through a loopback queue handled by the net thread. DNS is a user-space library (`ulib::dns`). The `net`
  kernel thread runs a round when the card interrupts or a packet is looped back (`RX_PENDING` + `task::wake`), at the latest
  every 10 ms; each round ends with `notify(Wait::Net)` – that doubles as the tick for timeouts of everyone waiting on the
  network and for TCP's retransmission timer. Ping to the gateway: ~0.5 ms (polled: 0-10 ms).
- **DHCP lease** (`net/dhcp.rs`): the "dhcp" thread renews at T1 (unicast REQUEST with `ciaddr`), retries after half
  the remaining time (≥ 60 s), rebinds by broadcast from T2, falls back to the static configuration at the end.
- **TCP windows** (M38): window scaling (RFC 7323, shift 3) when both SYNs carry the option; receive and send buffers
  256 KiB, up to 128 segments in flight. ARP: cache learned from requests/replies addressed to us and from IPv4 packets of on-link hosts; `resolve` sends up
  to 3 requests. IPv4: no options sent, fragments dropped, DF set. ICMP: echo requests answered, replies with our id recorded.
  UDP: ports map to bounded queues (64 datagrams); checksums sent and verified on receipt (a datagram with a wrong
  checksum is dropped; checksum 0 = "none computed" is accepted, as RFC 768 allows for IPv4).
- **User API**: `ping` (one echo, waits ≤ timeout), `udp_socket` → `Handle::Udp(Arc<UdpSocket>)` (port released when the last
  descriptor closes), `sendto` / `recvfrom` (timeout, abortable by kill), `net_info`. Small state locks are taken with
  interrupts off; the NIC lock with interrupts on (it is never needed by `wait_for` conditions).

## Kernel heap
- `mem::heap::LinkedListHeap`: free blocks form a singly linked list sorted by address; each free block stores `size` + `next` in its first 16 bytes.
- First fit; a block is split into [front padding][allocation][rest], padding and rest go back to the list. Freeing inserts by address and merges neighbours.
- All addresses and sizes are multiples of 16, so leftovers are either 0 or big enough for a header (no lost bytes).
- `KernelHeap` wraps it in a spin lock (interrupts disabled while held) and grows by mapping ≥64 KiB at the heap end when an allocation fails.
- Allocation failure → null → Rust's default alloc error handler panics.

## SMP (multiple cores)
- `arch::x86_64::smp::init` (end of `kernel::init`): every enabled MADT CPU except the BSP gets INIT, 10 ms, SIPI(s), one at
  a time. Trampoline: one frame below 1 MiB reserved by `pmm` at boot (`pmm::low_frame`), identity-mapped in the kernel
  PML4 only while APs start. It uses its own tiny GDT (code32/data32/code64/data64), sets PAE, CR3 = kernel PML4 (must be
  < 4 GiB), EFER.LME + NXE, CR0.PG + WP, and calls `ap_main(cpu)` on a 64 KiB guarded stack.
- CPU index: `smp::cpu_index()` maps the local APIC ID to 0..MAX_CPUS-1 (BSP = 0; 0 before the LAPIC is set up). Used for
  the per-CPU TSS. (M26 will move this to a GS-based per-CPU block.)
- APs (M25): own GDT/TSS (+ double fault IST stack), shared IDT, FPU/SSE, syscall MSRs, LAPIC (timer masked); they idle with
  `sti; hlt` and run jobs from a per-CPU mailbox (`run_on`, woken by an IPI on `WAKE_VECTOR` 0xF0). Since M26 every AP also runs
  threads (its start-up context becomes its idle thread, its LAPIC timer preempts). Device IRQs (keyboard, COM1) stay on the BSP.

## GDT / TSS
| Selector | Entry |
|---|---|
| 0x00 | null |
| 0x08 | kernel code (64-bit, DPL 0) |
| 0x10 | kernel data (DPL 0), also used for SS/DS/ES/FS/GS |
| 0x18 | user data (DPL 3), selector `0x1B` with RPL 3 |
| 0x20 | user code (64-bit, DPL 3), selector `0x23` |
| 0x28 | TSS (16 bytes, occupies 2 slots) |

One GDT and TSS per CPU (`gdt::init_cpu`), same layout everywhere. The user data/code order is dictated by `sysret` (SS = STAR base + 8, CS = base + 16). IST1 (`gdt::DOUBLE_FAULT_IST`) =
20 KiB static stack for double faults. RSP0 = kernel stack of the running thread, rewritten on every context switch.

## Interrupt vectors
| Vector | What |
|---|---|
| 0–31 | CPU exceptions (`arch/x86_64/interrupts.rs`), double fault (8) on IST1 |
| 32 | local APIC timer, periodic 100 Hz (drives `time::tick`) |
| 33 | ISA IRQ1 – PS/2 keyboard (I/O APIC GSI 1) |
| 36 | ISA IRQ4 – COM1 receive (I/O APIC GSI 4) |
| 39 / 47 | spurious IRQ 7 / 15 of the masked 8259 (ignored, no EOI) |
| 0x50 | AHCI command completion (MSI, M32) |
| 0x51 | e1000 (INTA# through the I/O APIC input found by `net::probe_irq`, level / active low, M38) |
| 0xF0 | wake-up IPI between CPUs (`smp::WAKE_VECTOR`) |
| 0xF1 | TLB shootdown IPI (`smp::TLB_VECTOR`, M41): reload CR3 if running the given address space |
| 0xFF | local APIC spurious vector (ignored, no EOI) |

- Interrupt controllers: **local APIC + I/O APIC** (since M12). Information comes from the ACPI MADT (`acpi.rs`).
  ISA IRQ `n` is delivered on vector `32 + n`; the GSI and polarity/trigger come from the MADT interrupt source overrides
  (QEMU: IRQ0 → GSI 2). All handlers acknowledge with `lapic::eoi()`.
- The 8259 PICs are remapped to 32–47 and then fully masked (never deliver anything). PIT channel 0 is not used;
  PIT channel 2 (polled via port 0x61) only serves as the reference clock to calibrate the LAPIC timer at boot.
- LAPIC LINT0 is masked (no ExtINT from the 8259), LINT1 = NMI. Registers are reached through the HHDM after `ensure_hhdm_mapped(.., Flags::MMIO)`.
- `kernel::init()` enables interrupts with `sti` at the very end. Anything holding a lock that a handler also takes must use `without_interrupts`.
- Handlers use `extern "x86-interrupt"` (nightly `abi_x86_interrupt`): the compiler saves registers and emits `iretq`.
  The IDT structure and all gate descriptors are our own code.
- `interrupts::install_exception_handlers(&mut Idt)` is separate from `init()`; tests build their own IDTs (see `tests/page_fault.rs`).
- Address `0x0000_4000_0000_0000` (lower half) is unmapped under Limine and is used by tests as a "guaranteed to fault" address.

## Keyboard
- The IRQ1 handler only reads port 0x60 and pushes the byte into a `RingBuffer<128>`; translation happens outside the
  interrupt in `drivers::scancode::Decoder`.
- Scancode set 1 (the 8042 controller translates by default). Layout switchable (`keyboard::set_layout`), default **DE**.
  QEMU forwards physical keys, so the kernel layout must match the user's real keyboard.
- `keyboard::inject_scancode()` feeds bytes without hardware (tests, later e.g. serial input).

## Input and shell
- Two input sources, one API: `input::read_key()` returns `KeyEvent`s from the PS/2 keyboard (scancode decoder) or from COM1
  (`TerminalDecoder`: UTF-8, CR/LF, DEL, Ctrl+letter, ANSI arrows/Home/End/Delete). It sleeps race-free with `sti; hlt`.
- The line editor only emits a terminal-compatible byte stream (`\x08` = cursor left, characters overwrite), so the same output
  works on the framebuffer console and on a serial terminal. No cursor-addressing escape sequences are needed.
- Commands are a static table (`shell::COMMANDS`) of `fn(&mut Shell, &[String], &mut dyn Write) -> Result<(), String>`;
  unit tests run them against a `String`. Errors are printed in red by the shell, not by the commands.
- `poweroff`: ACPI PM1a control port 0x604 (QEMU q35), fallback isa-debug-exit. `reboot`: 8042 reset line, fallback triple fault.

## Output
- `serial_println!`: COM1 only. `println!`: console **and** COM1. Both disable interrupts while holding their lock (`without_interrupts`).
- The boot binary's panic handler (`kernel::panic_report`) prints to serial and, if `CONSOLE.try_lock()` succeeds, in red on the screen.
- Console: keeps a `Vec<Cell>` character grid (char + colours) for redrawing the cursor cell; cursor = inverted cell.
  ANSI subset: `ESC[2J`, `ESC[H`, `ESC[row;colH`, `ESC[K`, `ESC[..m` (0, 1, 22, 30–37, 39, 40–47, 49, 90–97).
- Console: works on a `Surface` (32 bpp, stride in pixels). Scrolling copies pixel rows inside the framebuffer
  (slow on real hardware, fine in QEMU). Font: Terminus 8×16 (PSF2) via `include_bytes!`; Latin-1 characters through the
  Unicode table, unknown characters become `?`.

## Testing
- Crate layout: `src/lib.rs` contains the whole kernel, `src/main.rs` only `kmain` + panic handler.
  Every test binary (lib unit tests, `tests/*.rs`) defines its own `kmain`; the linker uses `ENTRY(kmain)` for all of them.
- Unit tests: `#[test_case]` inside the module. Integration tests: `kernel/tests/<name>.rs` with their own harness setup (see `tests/boot.rs`).
  Tests that must panic or fault get `harness = false` in `Cargo.toml` (see `tests/should_panic.rs`).
- `scripts/run-qemu.sh` is the cargo runner. A hash suffix in the ELF name means test binary → headless QEMU with a timeout.
- Result via the `isa-debug-exit` device on port 0xF4: value 0x10 → QEMU exit status 33 = PASS, 0x11 → 35 = FAIL.
- All test output goes through COM1 (`serial_println!`).
- `scripts/e2e-shell.py` (`make e2e`) boots the real kernel with stdin/stdout on COM1, types commands (incl. escape sequences)
  and checks the output between prompts; the final `poweroff` must end QEMU. `make check` = `make deps` + `make test` +
  `make e2e` + `make e2e-uefi`.
- Robustness (M41): `/bin/syscheck` calls every syscall with bad arguments and races user copies against `brk`;
  `make soak` (`scripts/soak.py` + `rootfs/soak.sh`) runs a mixed workload for `SOAK_ROUNDS` rounds; `make stress` runs
  the suite `STRESS_N` times with 1/2/4 CPUs, `qemu64` and UEFI, then a soak run.
- User memory is only touched in `copy_from_user` / `copy_to_user`: check and copy under the process table lock, in
  pieces of ≤ 64 KiB (bug #14). Empty buffers are never dereferenced (bug #12).
