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
- The **PIT** is a counter running at 1.19 MHz. A divisor of 11932 gives ~100 interrupts per second = one "tick" every 10 ms.
  Uptime and `sleep` are built on top of it.
- `hlt` puts the CPU to sleep until the next interrupt – this way the kernel waits without burning 100 % CPU.
- **APIC**: modern CPUs have a local APIC per core sitting between PIC and CPU. Limine masked its PIC input,
  so for now we disable it completely (bug #2).

## M7 – Keyboard
- When a key is pressed, the PS/2 controller sends a **scancode** and raises IRQ1. We read the byte from port 0x60.
- Scancodes describe the **position** of a key, not the letter: on QWERTZ and QWERTY the key next to T has the same code (0x15)
  but means "z" or "y" respectively. That is why we need a **layout**.
- Press = code, release = code + 0x80. We remember Shift, Ctrl and AltGr as state; Caps Lock toggles on every press.
- The interrupt handler does almost nothing (byte into a ring buffer) so that it finishes quickly; the work happens outside.
- **Race while waiting**: "buffer empty? → `hlt`" is dangerous if the interrupt arrives exactly in between – then the CPU sleeps although
  a key is already there. Solution: interrupts off, check, then `sti; hlt` – the CPU guarantees that no interrupt can slip in between `sti` and `hlt`.

## M8 – Physical memory
- RAM is managed in **frames** of 4 KiB. Limine tells us via the **memory map** which address ranges are usable RAM and which are
  reserved (firmware, ACPI, devices, the kernel itself, ...).
- The **bitmap allocator** keeps one bit per frame: 0 = free, 1 = used. Allocating means finding a 0 bit and setting it; freeing clears it.
  For 256 MiB that is only 65536 bits = 8 KiB of bookkeeping.
- Physical addresses cannot be dereferenced directly because paging is on. Limine maps all of physical memory at a fixed
  offset (**HHDM**, higher-half direct map), so physical `p` is accessible at virtual `p + offset`.

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

## Tooling – GNU ld and binutils
- rustc compiles our code into object files; a **linker** combines them into the final ELF according to `kernel/linker.ld`
  (load address, section order, segments). We use **GNU ld** (`ld.bfd`) for that.
- `objdump -d` disassembles the kernel, `nm` lists symbols with addresses, `readelf -l` shows the segments Limine loads,
  `addr2line` turns a RIP from a crash message into a source file and line.
