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

## Tooling – GNU ld and binutils
- rustc compiles our code into object files; a **linker** combines them into the final ELF according to `kernel/linker.ld`
  (load address, section order, segments). We use **GNU ld** (`ld.bfd`) for that.
- `objdump -d` disassembles the kernel, `nm` lists symbols with addresses, `readelf -l` shows the segments Limine loads,
  `addr2line` turns a RIP from a crash message into a source file and line.
