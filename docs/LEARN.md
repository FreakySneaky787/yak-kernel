# Learning notes

Short explanations of every subsystem: what it is and why we need it.

## M1 – Booting with Limine
- At power-on the **firmware** (BIOS or UEFI) starts and loads the **bootloader** (Limine) from the ISO.
- Limine reads `limine.conf`, loads our ELF (`/boot/kernel`), switches the CPU into **64-bit long mode**, sets up paging and jumps to `kmain`.
- We talk to Limine through **requests**: static structs with magic numbers in a dedicated section. Limine finds them in the ELF
  and fills in pointers to the responses (framebuffer, memory map, ...).
- **Base revision**: which protocol version we use. If Limine does not support it, the field is not cleared and we halt.

## M1 – Serial port (UART 16550)
- An ancient standard chip, accessed through **I/O ports** (`in`/`out` instructions) starting at 0x3F8.
- Init: set the baud rate via a divisor (DLAB bit), data format 8N1, enable the FIFO.
- Sending: wait until bit 5 of the line status register says "transmit buffer empty", then write the byte.
- QEMU redirects it to your terminal with `-serial stdio`. This is our `printf` debugging, and tests evaluate exactly this output.

## M1 – Panic handler
- Without `std` nobody prints a message when the program crashes – we do it ourselves with `#[panic_handler]`:
  message to serial, then `cli; hlt` forever.

## M2 – Testing without an operating system
- `cargo test` normally needs the standard library, which we do not have. With `custom_test_frameworks` the compiler collects
  all `#[test_case]` functions and passes them to **our** `runner` function.
- Every test is built as its own kernel. For each binary, `cargo test` calls the **runner** (`scripts/run-qemu.sh`): build the ISO, start QEMU headless.
- The way back is the virtual QEMU device **isa-debug-exit**: a write to port 0xF4 terminates QEMU immediately with exit status
  `(value << 1) | 1` – turning "kernel says OK" into an exit status for cargo.
- A **timeout** catches hangs (endless loops, or a triple fault after which QEMU stops because of `-no-reboot`).

## M3 – Text on the screen
- The **framebuffer** is a piece of memory where every 32-bit word is a pixel (0x00RRGGBB). What we write there appears on the screen immediately.
- A **bitmap font** stores 16 bytes per character, one per pixel row; bit 7 is the leftmost pixel. Drawing means checking every bit
  and setting the foreground or background colour.
- **PSF2** is the Linux console font format: header, glyphs, and a table "which Unicode character is which glyph" (that is where the umlauts come from).
- **Scrolling**: when the last row is full, all pixel rows are copied up by one font height (16 px) and the bottom row is cleared.

## M4 – GDT and TSS
- The **GDT** is a table of segment descriptors, a leftover from 16/32-bit times. In 64-bit mode almost only two things matter:
  whether a code segment is "64-bit" (L bit) and which privilege level applies (ring 0 = kernel, ring 3 = programs).
- The **CS register** cannot simply be written. We push selector and return address on the stack and execute a "far return" (`retfq`),
  which makes the CPU reload CS.
- In 64-bit mode the **TSS** no longer holds task state but stack pointers. The **IST** (interrupt stack table) provides up to 7 emergency
  stacks: if the kernel stack overflows it is broken – and the double fault handler would itself need a stack. So we let the CPU switch
  to a separate IST stack for it. Without that we would get a **triple fault** and the CPU resets.

## M5 – Interrupts and exceptions
- An **exception** is raised by the CPU itself when something goes wrong: division by zero, invalid instruction,
  access to unmapped memory (**page fault**), ...
- The **IDT** is a table with 256 entries: for each vector it says which function the CPU should jump to. Before that the CPU
  pushes RIP, CS, RFLAGS, RSP and SS (the **interrupt stack frame**), and for some exceptions an additional **error code**.
- Handlers end with `iretq` instead of `ret` so the CPU pops the frame again – `extern "x86-interrupt"` does this for us.
- **Page fault**: CR2 holds the address that caused the fault; the error code tells whether the page was missing or protected,
  whether it was a read or a write, and whether kernel or user code made the access.
- **Double fault**: happens when another exception occurs while handling one. If that fails too: **triple fault** → CPU reset
  (with `-no-reboot` QEMU simply stops).

## M6 – Hardware interrupts, PIC and timer
- Devices signal via an **IRQ** (interrupt request). The **8259 PIC** collects 16 IRQ lines (two chips of 8, cascaded) and forwards them
  to the CPU as interrupt vectors.
- **Remap**: out of the box IRQ0 would arrive as vector 8 – which is also the double fault. So we move the IRQs to 32–47.
- **EOI** (end of interrupt): every handler must tell the PIC it is done, otherwise no further interrupt arrives.
- The **PIT** is a counter running at 1.19 MHz. A divisor of 11931 (1193182 / 100, rounded down) gives ~100 interrupts per second = one "tick" every 10 ms.
  Uptime and `sleep` are built on top of it.
- `hlt` puts the CPU to sleep until the next interrupt – this way the kernel waits without burning 100 % CPU.
- **APIC**: modern CPUs have a local APIC per core sitting between PIC and CPU. Limine masked its PIC input,
  so for now we disable it completely (bug #2). The course instead starts QEMU with a CPU without local APIC
  (`-cpu qemu64,-apic`): switching it off does not help under QEMU 8.2 (bug #25).

## M7 – Keyboard
- When a key is pressed, the PS/2 controller sends a **scancode** and raises IRQ1. We read the byte from port 0x60.
- Scancodes describe the **position** of a key, not the letter: on QWERTZ and QWERTY the key next to T has the same code (0x15)
  but means "z" or "y" respectively. That is why we need a **layout**.
- Press = code, release = code + 0x80. We remember Shift, Ctrl and AltGr as state; Caps Lock toggles on every press.
- The interrupt handler does almost nothing (byte into a ring buffer) so that it finishes quickly; the work happens outside.
- **Race while waiting**: "buffer empty? → `hlt`" is dangerous if the interrupt arrives exactly in between – then the CPU sleeps although
  a key is already there. Solution: interrupts off, check, then `sti; hlt` – the CPU guarantees that no interrupt can slip in between `sti` and `hlt`.
  While a periodic timer runs, the lost wake-up only delays the key until the next tick; without one it waits for the next key.
- The buffer's spin lock is shared with the handler: the rest of the kernel takes it only with interrupts off – a handler
  that interrupts the lock's holder would wait for it forever.

## M8 – Physical memory
- RAM is managed in **frames** of 4 KiB. Limine tells us via the **memory map** which address ranges are usable RAM and which are
  reserved (firmware, ACPI, devices, the kernel itself, ...).
- The **bitmap allocator** keeps one bit per frame: 0 = free, 1 = used. Allocating means finding a 0 bit and setting it; freeing clears it.
  For 256 MiB that is only 65536 bits = 8 KiB of bookkeeping.
- Physical addresses cannot be dereferenced directly because paging is on. Limine maps all of physical memory at a fixed
  offset (**HHDM**, higher-half direct map), so physical `p` is accessible at virtual `p + offset`.
- Only **usable** ranges may be handed out at first. *Bootloader reclaimable* memory holds the page tables the CPU is using,
  the boot stack and Limine's responses (the memory map itself); it becomes free only after the kernel has its own.

## M9 – Paging
- Every memory access uses a **virtual** address; the CPU translates it to a physical address through **page tables**.
  On x86_64 there are 4 levels: PML4 → PDPT → PD → PT, each a 4 KiB table with 512 entries. The address bits 47–39, 38–30, 29–21 and 20–12
  pick the entry on each level, bits 11–0 are the offset inside the 4 KiB page.
- Each entry holds a physical address plus flags: present, writable, user-accessible, no-execute, ... A PD/PDPT entry can also map a
  2 MiB / 1 GiB **huge page** directly.
- **CR3** holds the physical address of the PML4. After changing an entry, the old translation may still sit in the **TLB** (a cache),
  so we flush it with `invlpg`.
- **Guard page**: we leave the page below each kernel stack unmapped. If the stack overflows, the CPU touches that page and faults
  immediately, instead of silently corrupting the memory below.
- An entry's bits depend on the level: in a 2 MiB / 1 GiB entry bit 12 is **PAT** (caching type), not an address bit –
  `translate` must round the address down to the page size (bug #26). Limine builds the HHDM from 2 MiB pages where it can.

## M10 – Heap
- The frame allocator only hands out whole 4 KiB pages. Most kernel data is much smaller and has arbitrary sizes – that is the job of the **heap**.
- Our **linked-list allocator** remembers all free gaps in a list. The list needs no extra memory: each free gap stores its size and a pointer
  to the next gap inside itself.
- **Allocate**: walk the list, take the first gap that is big enough (*first fit*), cut off what is not needed and put it back.
- **Free**: put the block back into the list at its address position; if it touches a neighbouring gap, merge them (*coalescing*) –
  otherwise the heap would crumble into many tiny unusable pieces (*fragmentation*).
- Once a `#[global_allocator]` exists, Rust's `alloc` crate works: `Box`, `Vec`, `String`, `BTreeMap` – just like in normal programs.

## M11 – Shell
- A **shell** is a loop: print a prompt, read a line, split it into words, look up the first word as a command, run it.
- The **line editor** lets you move the cursor and edit in the middle of the line. Because we only use the backspace character
  (`\x08`) to move left and simply overwrite characters, the same output works on our screen and on any serial terminal.
- **Terminals talk in escape sequences**: an arrow key on a serial terminal arrives as three bytes `ESC [ A`, colours are sent as
  `ESC [ 31 m`. Our console understands a small subset, and the input decoder turns incoming sequences back into keys.
- **Serial input** works like the keyboard: the UART raises IRQ4 when a byte arrives, the handler stores it in a ring buffer.
  This lets automated tests "type" into the real kernel through QEMU's stdin.

## M12 – ACPI and the APIC
- **ACPI** tables are a description of the machine provided by the firmware. Limine gives us the **RSDP**, which points to the
  **XSDT** – a list of all other tables. Each table starts with a 4-letter signature and a checksum (all bytes add up to 0).
- The **MADT** (signature "APIC") lists every CPU's **local APIC**, the **I/O APIC**s, and *overrides* such as "the timer (ISA IRQ 0)
  is actually wired to I/O APIC input 2".
- The **I/O APIC** replaces the old PIC: for every input line it has a *redirection entry* saying which vector to raise on which CPU,
  and whether the line is edge/level triggered and active high/low.
- The **local APIC** sits in each CPU. It receives the interrupts, needs an **EOI** after each one, and has its own **timer**.
  Its speed is unknown, so we measure it once against the PIT (whose frequency is fixed at 1.193182 MHz) and then program it for 100 Hz.
- APIC registers are **memory-mapped I/O (MMIO)**: reading/writing special physical addresses talks to hardware, not RAM.
  Such pages must be mapped **uncached**, otherwise the CPU might serve stale values from its cache.

## M13 – Multitasking
- A **thread** is "a CPU state + its own stack". To switch threads we save the registers of the running one on its stack,
  remember its stack pointer, load the stack pointer of the next one and restore its registers – the **context switch**.
  The next `ret` then continues wherever that thread stopped.
- **Preemptive** scheduling: the timer interrupt arrives every 10 ms and may switch to another thread, even if the running one never
  gives up the CPU voluntarily. **Round robin**: ready threads take turns in a queue.
- **Blocking** instead of busy-waiting: a thread that waits (sleep, keyboard input, another thread finishing) is taken off the ready
  queue and woken up by the event it waits for. If nobody is ready, the **idle thread** halts the CPU.
- The classic trap is the **lost wake-up**: check "no input yet", then the key arrives, then we go to sleep – and never wake up.
  We avoid it by checking and blocking with interrupts disabled.

## M14 – PCI and the SATA disk
- **PCI** devices are found by reading their *configuration space*: vendor/device ID, class (storage, network, ...) and **BARs**
  (base address registers) that tell where the device's registers are mapped in physical memory.
- **AHCI** is the standard interface of SATA controllers. The kernel prepares command structures in RAM and the controller fetches
  them by itself – **DMA** (direct memory access). That is why the buffers must be real *physical* frames, and why the device needs
  "bus mastering" enabled.
- A disk is just a long array of **sectors** (512 bytes). `READ DMA EXT` / `WRITE DMA EXT` move sectors between disk and RAM,
  addressed by their number (**LBA**, logical block address). File systems are built on top of this.

## M15 – The FAT32 file system
- A **file system** turns the flat array of sectors into named files and directories. FAT32 is old and simple, and every OS
  (and Linux tools like `fsck.fat` and mtools) can read it – ideal to check our own code against.
- The disk is split into **clusters** (here 512 bytes). The **FAT** (file allocation table) has one entry per cluster saying
  which cluster comes next in the same file – a file is a linked list of clusters. 0 = free, `0x0FFFFFF8+` = end of the file.
- A **directory** is just a file full of 32-byte entries: name, attributes, first cluster, size, timestamps.
- Names were originally limited to 8+3 upper-case characters. **Long file names** (VFAT) are stored in extra hidden entries
  right before the 8.3 entry, 13 UTF-16 characters each, protected by a checksum of the short name.
- **Persistence** means: after `write`, the data must really be on the disk image, so the kernel flushes the disk after every
  change. The E2E test proves it by writing a file, powering off, booting again and reading it back.
- The **RTC** (real-time clock) is a battery-powered clock chip; we read it via ports 0x70/0x71 for file timestamps and `date`.

## M16 – User space
- The CPU has **privilege levels** (rings). The kernel runs in ring 0 and may do everything; programs run in **ring 3**, where
  privileged instructions (`cli`, `hlt`, writing CR3, port I/O) raise a general protection fault and pages without the
  **USER** bit cannot be touched. A misbehaving program therefore cannot crash the kernel – it gets killed instead.
- Each process has its own **address space**: its own top-level page table (PML4). The lower half is private, the upper half
  (the kernel) is the same in all of them. Switching processes = loading another PML4 into **CR3**.
- Entering ring 3 the first time: build the stack frame an interrupt would push (SS, RSP, RFLAGS, CS, RIP – with ring-3
  selectors) and execute `iretq`. Coming back happens through interrupts/exceptions (the CPU switches to the kernel stack
  stored in the TSS, **RSP0**) or through `syscall`.
- A **system call** is a controlled door into the kernel: the program puts a number and arguments into registers and executes
  `syscall`; the CPU jumps to the address in the LSTAR MSR in ring 0, the kernel does the work and returns with `sysret`.
  The kernel must never trust pointers from user space – it checks every buffer against the process's page tables.
- An **ELF file** describes how to build the program image: the *program headers* list segments ("map these bytes from the
  file at that address, with these permissions, plus zeroed memory for `.bss`"). Our loader maps exactly these and jumps to
  the *entry point*.
- Our programs live in `user/`: a tiny runtime (`ulib`: `_start`, syscall wrappers, `println!`) plus one file per program.
  They are ordinary Rust `no_std` binaries, linked at 0x40_0000, and are loaded from `/bin` on the FAT32 disk.

## M17 – Files for programs
- A **file descriptor** is just a small number that stands for an open file inside one process: the kernel keeps a table per
  process, `open` puts an entry in it and returns its index, `read`/`write`/`close` take that index. 0, 1 and 2 are
  pre-opened: keyboard input, screen output, error output. Because the console is "just another descriptor", `cat` writes a
  file to the screen with exactly the same `write` call it would use for a file.
- The kernel remembers the **position** in an open file, so consecutive `read`s continue where the last one stopped.
- Every process has a **current directory**; relative paths in system calls start there. A program inherits it from the
  shell – that is why `ls` without arguments lists the directory you `cd`ed into.
- Errors come back as negative numbers (`-2` = `ENOENT` "no such file or directory"), the same numbers Linux uses.
- Programs get their arguments as one block with NUL bytes between them, so `cp "My Notes/a.txt" b.txt` arrives as exactly
  two arguments even though the first one contains a space.

## M18 – A heap for programs
- The kernel gives a process memory in big pieces: `brk` moves the **program break**, the end of the heap area right after
  the program image. Growing maps fresh zeroed pages, shrinking frees them again.
- Handing out small pieces (a `String` of 20 bytes) is the job of an **allocator** inside the program – here `ulib::heap`.
  It keeps a list of free blocks, takes the first one that is big enough (*first fit*), and when a block is freed it is
  merged with free neighbours so that the heap does not crumble into unusable fragments.
- With a `#[global_allocator]` in place, the standard `alloc` crate works: `Vec`, `String`, `BTreeMap`, `format!` – the
  `sort` program reads a whole file into memory and sorts its lines without knowing anything about pages.

## M19 – Processes starting processes
- `spawn` loads a program from the disk and starts it as a **child** of the caller; `wait` blocks until the child ends and
  returns how it ended (exit code, or the reason the kernel killed it). Only the parent may wait for or kill its children.
- A child whose parent ends first is an **orphan**; since nobody will ever ask for its exit status, the kernel throws the
  status away as soon as the orphan ends.
- **Ctrl+C** must work even if the program never reads the keyboard (an endless loop). So the interrupt handler itself
  watches for it and marks the **foreground process** – the one the console currently belongs to – as killed. A process that
  is blocked (sleeping, waiting for input) is woken up so it notices immediately.

## M20 – The shell becomes a program
- In a real OS the shell is not part of the kernel: it is an ordinary program that reads lines, starts other programs and
  waits for them. The kernel only starts the very first program, **init** – here `/bin/sh`.
- Because the keyboard reaches programs as bytes, special keys must be encoded: the arrow keys become the same **escape
  sequences** a serial terminal sends (`ESC [ D` = left). The shell decodes them again and edits the line.
- The kernel shell's line editor and parser are plain logic without hardware access, so the very same source files are
  compiled into the kernel *and* into `/bin/sh`.
- If `/bin/sh` is missing or you type `exit`, the kernel shell takes over – a safety net while the user space grows.

## M21 – Pipes and redirection
- A **pipe** is a small buffer inside the kernel with two ends: one process writes into it, another reads from it.
  If it is empty the reader waits, if it is full the writer waits – so `cat big.txt | wc` works with a fixed 64 KiB buffer.
- When the last writer closes its end, the reader gets "end of file"; when the last reader is gone, writing fails.
- **Redirection** is just descriptor juggling: the shell makes fd 1 point to a file (`> file`) or to a pipe, starts the
  program (which inherits fds 0–2 and has no idea), then puts its own fd 1 back. `dup2(a, b)` = "let b refer to what a
  refers to". Programs only ever write to fd 1.
- Programs ask `isatty` whether they talk to a person (echo typed characters, Ctrl+D ends input) or to a file/pipe.

## M22 – System information for programs
- Commands like `ps` or `mem` need data only the kernel has. Instead of letting programs call kernel code directly, the
  kernel offers **system calls that fill a struct** in the program's memory (`sysinfo`, `proclist`). The struct layout is
  fixed (`repr(C)`) and checked for its size on both sides, so kernel and programs agree byte for byte.
- An open file has one **position**: `read` and `write` continue there, `seek` moves it. Writing in the middle overwrites,
  writing at the end grows the file, and writing after seeking past the end leaves a gap that reads as zeros.

## M23 – A full-screen editor
- A terminal is not only a stream of lines: escape sequences can move the cursor anywhere (`ESC [ row ; col H`), erase the
  rest of a line (`ESC [ K`) or swap colours (`ESC [ 7 m`). A full-screen program draws its whole picture with them.
- The editor keeps the text as a list of lines in memory and only writes the file on Ctrl+S. Separating the **text model**
  (insert, delete, cursor movement) from the **drawing** makes the model testable without a screen (`edit --selftest`).
- Redrawing everything on every key would be slow; the editor redraws only the line that changed unless it had to scroll.

## M24 – Floating point and SSE registers
- Besides the general registers, the CPU has the x87 FPU and 16 **XMM registers** (128 bit each) for floating point and
  vector work, plus the control register **MXCSR** (e.g. rounding mode). Compilers use them for `f64` maths and fast copies.
- When the scheduler switches threads, these registers belong to the thread too. If the kernel does not save them, one
  program suddenly sees another program's numbers. `fxsave` writes all of them (512 bytes) to memory, `fxrstor` loads them.
- The kernel itself is built without floating point, so it never disturbs them – only the context switch has to care.
- `fputest` proves it: four programs keep different values in all XMM registers while being preempted constantly; with the
  switch code removed they immediately see each other's values.

## M25 – Waking the other CPU cores
- At power-on only one core runs (the bootstrap processor). The others sleep until their local APIC receives an
  **INIT** signal followed by a **STARTUP IPI** carrying a page number: the core then starts executing at that page in
  16-bit **real mode** – just like a PC in 1985.
- So we need start code below 1 MiB that walks through the history of the x86: real mode → protected mode (32 bit,
  segment table) → long mode (64 bit, needs PAE paging and the EFER.LME bit) and finally jumps into our Rust kernel.
- Each core needs its own stack, its own TSS (where its interrupt stacks are), and its own local APIC set up. Memory and
  page tables are shared – that is what makes it one machine.
- For now the extra cores only run small jobs we hand them; the scheduler learns to use them in the next milestone.

## M26 – One scheduler, many cores
- With several cores, "interrupts off" no longer means "nobody else runs": the other cores keep going. Every shared
  structure needs a real lock, and the order of operations suddenly matters.
- **Switching threads**: a core must not let go of the old thread before its registers are saved, or another core could
  start running it with stale registers. So the scheduler lock is held *through* the switch and released by the new thread.
- **Lost wake-ups**: "check if there is input, then go to sleep" breaks if the input arrives on another core between the
  two steps. The fix is to announce the sleep first ("I am waiting for input"), then check, then sleep – a waker that comes
  in between simply cancels the sleep.
- Idle cores sleep with `hlt`; when work appears, another core wakes them with an inter-processor interrupt.

## M27 – Networking
- A network card is another DMA device: we give it rings of descriptors pointing at buffers in RAM; it fills receive
  buffers with incoming **Ethernet frames** and sends the frames we put into the transmit ring.
- Frames are layered like envelopes: **Ethernet** (MAC addresses) carries **IPv4** (IP addresses), which carries **ICMP**
  (ping) or **UDP** (datagrams to a port). Each layer has a small header in front of its payload.
- To send to an IP address, we first need the neighbour's MAC address: **ARP** asks "who has 10.0.2.2?" on the whole
  network and remembers the answer. Hosts outside our subnet are reached through the **gateway**.
- QEMU emulates a whole small network for us: a router at 10.0.2.2 that answers pings and forwards UDP to the host, which
  is how the E2E test talks to the kernel over UDP.

## M28 – A fine clock
- The timer interrupt comes every 10 ms – too coarse to measure a ping. Every x86 core also has a **time stamp counter**
  that counts up at a fixed, high rate (GHz). We only have to find out *how* fast: count TSC ticks while the PIT (whose
  frequency is known exactly) measures 50 ms. After that, `TSC difference / frequency` gives microseconds.

## M29/M30 – TCP, DHCP, DNS, HTTP
- **TCP** turns lost, duplicated or reordered packets into a reliable byte stream: every byte has a sequence number, the
  receiver acknowledges what it got, and the sender repeats what was not acknowledged in time. A three-way handshake
  (SYN, SYN-ACK, ACK) opens a connection, FIN packets close each direction.
- **DHCP**: a new machine shouts "I need an address" (broadcast), a server offers one, the machine requests it, the server
  confirms – together with gateway and DNS server.
- **DNS** translates names into addresses: a small UDP question to a name server, which answers with A records.
- **HTTP** is plain text over TCP: `GET /path HTTP/1.0`, an empty line, and the server answers with a status line,
  headers, an empty line and the file.

## M31 – A shell you can program
- **Environment variables** are name/value pairs every process carries; a child gets a copy of its parent's. `PATH` tells
  the shell where to look for programs.
- Before a line is split into words, the shell replaces `$NAME` with the value – except inside single quotes.
- `2>` redirects the *error* output (fd 2) the same way `>` redirects fd 1; `2>&1` makes fd 2 point where fd 1 points.
- `cmd &` starts a program without waiting; the shell keeps a job list and reports when the job is done.
- A **script** is just a file with shell lines; `#!` at the start tells the shell to run it with `sh`.

## M32 – Measure before you optimise
- The first guess ("the disk is slow, add a cache") was only part of the truth: the virtual disk answered in microseconds;
  most of the time went into walking the same FAT chain and directory again for every 4 KiB read. Timing each step separately
  showed where to work.
- A **cache** keeps recently used sectors in RAM; *write-through* means every write still goes to the disk immediately, so
  the cache can never lose data.
- **MSI** (message signaled interrupts): instead of an interrupt wire, a PCI device writes a small message to a special
  address – the local APIC – which turns it into an interrupt. No routing tables needed.

## M33 – Getting memory back
- The bootloader's memory (its stack, page tables, the answers to our requests) is marked *reclaimable*: free once we no
  longer need it. The tricky part is the page tables – the CPU is using them right now. So we build copies elsewhere,
  switch every CPU over (writing CR3), and only then release the old frames.
- **Demand paging**: asking for heap memory with `brk` only reserves addresses. The first access to a page causes a page
  fault; the handler sees "page not present, inside the heap", maps a fresh zeroed page and returns – the CPU repeats the
  instruction, which now works. A program that reserves 32 MiB but touches three pages costs three frames.

## M34 – Signals
- A **signal** is a tap on the shoulder of a running program: "the user pressed Ctrl+C", "please terminate". The kernel
  notes it (a bit per signal) and acts the next time the program would continue running in user mode.
- Without a handler the program simply ends. With a handler, the kernel makes the program "call" the handler as if the
  interrupted instruction had called it: it pushes the return address onto the program's stack and jumps to a small
  assembly routine in the program (the trampoline) that saves all registers, calls the handler and restores everything.
  The program never notices the interruption – except that a blocking call (sleep, read) returns "interrupted" (EINTR).
- Signals are why a shell can stop its own loop on Ctrl+C, and why `kill -TERM` lets a program clean up first, while
  `kill -KILL` cannot be caught.

## M35 – A shell that is a programming language
- Until now every line was one command. A real shell first **parses** the text into a tree: lists of commands, `&&`/`||`
  chains, pipelines, and compound commands like `if`, `while` and `for` that contain lists themselves. Only when a command
  is about to run are its words **expanded** (`$x`, `$((1 + 2))`, `*.txt`), so a loop sees the new value of `$i` each round.
- `break`, `continue`, `return` and `exit` travel up the tree as a small "flow" value until the loop, function or shell
  that handles them.
- When a line is not finished (an open quote, an `if` without `fi`), the shell asks for more with the prompt `> `.

## M55 – Using the output of a command
- `$(command)` runs a command and puts what it printed in its place: `echo "today: $(date)"`, `for f in $(ls)`. The
  shell connects the command's output to a pipe and reads it back – a second thread reads while the command writes,
  because a pipe only holds 64 KiB and a writer with nobody reading would wait forever.
- `"$@"` passes on a script's or function's arguments exactly as they came, each one a separate word even when it
  contains spaces. `"$*"` glues them into one word; unquoted `$@` splits them again.

## M56 – Choosing by pattern, and variables of your own
- `case $file in *.txt) …;; *.md) …;; esac` compares one word with patterns in order and runs the first branch that
  matches – the same `*` and `?` as for file names, applied to a string.
- `local x=1` inside a function gives the function its own `x`: the old value is saved and put back when the function
  returns. Functions it calls still see this `x` (*dynamic* scope) – unlike most programming languages.

## M57 – A line longer than the screen
- A terminal knows nothing about "lines you are editing": it has a cursor and rows. When the text reaches the right
  edge it continues on the next row – so the editor must work out itself where position 170 of the line is (row 1,
  column 10 on a 160-column screen) and move the cursor there with "up", "down" and "right" escape sequences.
- A small trap: after writing in the last column, most terminals leave the cursor *in* that column until the next
  character arrives. The editor writes a space and returns to the row's start in that case, so it always knows where
  the cursor really is.

## M58 – Losing packets on purpose
- A network loses packets all the time; TCP's job is to hide that. The test makes the kernel throw away every tenth
  packet it sends to itself – data, acknowledgements, even the very first SYN – and checks that the bytes still arrive
  complete and in order. They do, because every unacknowledged segment is sent again when its timer runs out.
- How long TCP keeps trying matters too: the wait doubles after each attempt, so a peer that has vanished is noticed
  only after several attempts. The test measures exactly that time and compares it with the sum it should be.

## M36 – Threads and futexes
- A **thread** is a second "line of execution" inside the same program: same memory, same files, own registers and stack.
  For the kernel it is just one more schedulable thread that happens to use the process's page tables.
- Ending a process gets harder: other threads may be running on other CPUs right now. The kernel marks the process as
  ending, every thread notices on its next way into or out of the kernel and leaves, and the last step (freeing the
  memory) waits until all are gone.
- A **futex** lets user programs build locks that cost nothing when uncontended: an atomic instruction takes the lock;
  only a thread that has to wait asks the kernel to sleep "while this word still has that value", and the unlocking
  thread asks the kernel to wake it.
- The kernel got a **sleeping mutex** too: a thread that finds the disk busy now sleeps instead of spinning.

## M37 – Guard rails in the CPU, and wider registers
- **SMEP** and **SMAP** are two CPU switches that protect the kernel from itself: with SMEP the kernel can never execute
  code that lives in a user program's memory, with SMAP it cannot even read or write that memory – except in short,
  explicit windows (`stac` … `clac`) around the few functions that copy data between user programs and the kernel. A
  bug that follows a user-supplied pointer elsewhere now crashes at once instead of silently doing the wrong thing.
- **AVX** registers (YMM) are 256 bits wide, twice the SSE registers. The kernel must save and restore them on every
  thread switch; the `xsave` instruction does that for every state component the kernel enabled in the register XCR0.

## M38 – Finding an interrupt by trying
- Old PCI devices signal interrupts on a wire (INTA#). Which input of the I/O APIC that wire reaches is written down
  only in the ACPI tables' byte code (AML), which we do not run. So the kernel *asks the hardware*: it connects one
  possible input after the other to the network handler and makes the card fake an event; where the interrupt shows
  up, the wire is. Now the network thread sleeps until a packet really arrives instead of looking every 10 ms.
- **Window scaling** lets TCP say "I can take more than 64 KiB" (the window field has only 16 bits): both sides agree
  on a multiplier in their first packets.
- A DHCP address is only *leased*: half-way through, the client asks the server to extend it.

## M41 – Testing how a kernel fails
- Normal tests check that the right input gives the right result. A kernel must also survive **every wrong input**:
  any program may pass any number as a pointer, length, descriptor or syscall number.
- `syscheck` does that on purpose: null, kernel, non-canonical and unmapped pointers; lengths up to `u64::MAX`; closed
  and absurd descriptors; unknown syscall numbers – and then 20 000 random mixes with a fixed seed (so a failure can be
  reproduced). Every call must return an error, and the kernel must still be alive.
- It found five bugs on the first runs (`docs/BUGS.md` #11–#15): a length that froze the machine, an empty buffer
  that panicked the kernel, a directory entry lost on an error, a **race** between two threads that crashed the kernel,
  and a TLB shootdown that could wait forever.
- **Races** only show up when timing is right. `syscheck race` makes the bad timing likely: one thread shrinks the heap
  thousands of times a second while another hands exactly that memory to the kernel. With one CPU the timer interrupt
  switches between them at random points – that is how the bug appeared. Running with 1, 2 and 4 CPUs matters.
- **Soak test** (`make soak`): the same mix of real work for many rounds (files, pipelines, all self-tests,
  background jobs, ping, HTTP over loopback). Slow leaks and rare hangs only show up over time.
- **Stress** (`make stress`): the whole test suite N times with 1, 2 and 4 CPUs, with an old CPU model and under UEFI.
  A test that fails once in five runs is a bug, not bad luck.

## M52 – A guard below every thread stack
- A stack grows downwards. If nothing stops it, a thread that recurses too deeply simply keeps writing – into whatever
  lies below its stack: the heap, another thread's stack. The program then fails much later, somewhere else, for no
  visible reason.
- A **guard** is a range of addresses that is deliberately never mapped. The first write into it is a page fault, and
  the kernel knows at once what happened: the fault address is in a guard, so it is a stack overflow.
- The kernel stacks have had guards since M9, and nothing is mapped below the main user stack. Thread stacks came from the heap, where there is
  no room for a hole. Now the kernel hands out fixed **stack slots** – stack on top, guard below – and frees a slot once
  its thread has ended.

## M53 – When the disk fails in the middle
- One file system command is many disk writes: the FAT (which clusters belong together), the data, the directory
  entry (name, size, first cluster). If the power goes out after some of them, the disk holds a mix of old and new.
- The order of the writes decides how bad that mix is. YAK writes new data and links it in the FAT *before* the
  directory entry points to it, and frees old clusters only *after* nothing points to them any more. A cut then
  leaves at worst clusters that are marked used but belong to no file ("lost clusters") – wasted space, no wrong data.
- Real file systems (ext4, NTFS) add a **journal**: they first write down what they are about to do, so after a crash
  they can finish or undo it. FAT32 has no journal; `fsck.fat` cleans up afterwards.
- The test does not trust this reasoning: a fake disk fails at every single I/O of an operation, and a checker
  (like a tiny `fsck`) looks at what is left.

## Tooling – GNU ld and binutils
- rustc compiles our code into object files; a **linker** combines them into the final ELF according to `kernel/linker.ld`
  (load address, section order, segments). We use **GNU ld** (`ld.bfd`) for that.
- `objdump -d` disassembles the kernel, `nm` lists symbols with addresses, `readelf -l` shows the segments Limine loads,
  `addr2line` turns a RIP from a crash message into a source file and line.
