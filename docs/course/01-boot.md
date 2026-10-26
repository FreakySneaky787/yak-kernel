# 1 – Booting with Limine

**Goal:** a kernel that a real bootloader starts in 64-bit mode and that greets us on the serial port.
**You end up with:** `make run` shows `Hello from KernelTry!` in your terminal; `make test` checks it automatically.
**Code:** [`course/01-boot/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/01-boot)

## Background: who starts a kernel?
When a PC starts, the **firmware** (BIOS or UEFI) runs first. It knows nothing about our kernel; it loads a
**bootloader** from a disk or CD. The bootloader's job is to load the kernel file into memory, put the CPU into the
mode the kernel expects and jump to the kernel's first instruction.

x86-64 CPUs start in 16-bit *real mode*, like a PC from 1981. Getting to 64-bit *long mode* needs a descriptor table,
page tables and several mode switches – interesting, but not where we want to begin. We use **Limine**, a modern
bootloader that does all of this and then jumps into our kernel with:
- the CPU in 64-bit long mode, paging on, interrupts off,
- our kernel loaded at the address its ELF file asks for,
- a stack to run on,
- answers to **requests** our kernel left for it (we use one in this chapter).

The kernel is an ordinary **ELF file** – the format Linux uses for programs – built by Rust and linked by GNU ld.

## Step 1: a Rust project without an operating system
Every chapter folder pins the Rust compiler in `rust-toolchain.toml`; rustup installs it on the first build. We
need a *nightly* compiler for features later chapters use, plus the `x86_64-unknown-none` target: x86-64 code that
expects **no** operating system underneath.

`kernel/Cargo.toml` names our only dependency, the Rust definitions of the Limine protocol:

```toml,file=kernel/Cargo.toml
[dependencies]
# Rust definitions of the Limine boot protocol (the only crate besides `spin` in the whole project)
limine = "0.6.5"
```

and makes a panic stop the kernel instead of trying to unwind the stack – there is no runtime that could do that:

```toml,file=kernel/Cargo.toml
[profile.dev]
panic = "abort"
```

`kernel/.cargo/config.toml` sets the target and tells the linker how to build a kernel:

```toml,file=kernel/.cargo/config.toml
[build]
# A target without an operating system underneath: no std, no libc, no default linker script
target = "x86_64-unknown-none"
```

The `rustflags` there choose GNU ld, our own linker script, and the *kernel code model*: the code lives in the top
2 GiB of the 64-bit address space, at `0xffffffff80000000` – the **higher half**. Programs will later get the
lower half; the kernel stays out of their way.

## Step 2: the memory layout – `linker.ld`
The linker script decides where every part of the kernel lands:

```text,file=kernel/linker.ld
ENTRY(kmain)
```

`kmain` is the function Limine jumps to. Then the address of everything that follows:

```text,file=kernel/linker.ld
    . = 0xffffffff80000000;

    .limine_requests : {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    } :limine_requests
```

The Limine requests come first, in their own section, between a start and an end marker – that is where Limine
looks for them. `KEEP` stops the linker from throwing them away (no code refers to them). After that come code
(`.text`), constants (`.rodata`) and variables (`.data`, `.bss`), each starting on a new page so that later we can
give them different permissions (code executable but read-only, data writable but not executable).

## Step 3: talking to Limine
`kernel/src/main.rs` starts with two attributes that switch off what a normal Rust program takes for granted:

```rust,file=kernel/src/main.rs
#![no_std] // no standard library: it needs an operating system, and we are writing one
#![no_main] // no `main`: nobody calls it - Limine jumps to `kmain` (see linker.ld: ENTRY(kmain))
```

Without `std` we still have `core`: integers, `Option`, iterators, `fmt` – everything that needs no operating
system.

The one request we make is the **base revision** – which version of the boot protocol we speak:

```rust,file=kernel/src/main.rs
/// Which version of the boot protocol we speak. Limine sets a field in it to 0 if it supports it.
#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();
```

`#[used]` keeps the static in the binary, `link_section` puts it where the linker script collects requests. The two
markers `REQUESTS_START` and `REQUESTS_END` are declared the same way. Limine reads the magic numbers inside these
structures directly from the kernel file before it starts the kernel.

## Step 4: the serial port
The screen needs a driver (chapter 3). The **serial port** needs about 20 lines, and QEMU connects it to our
terminal – so it is the first thing a kernel uses to talk, and later the channel all our tests run over.

The 16550 UART of COM1 sits at **I/O port** `0x3F8`. x86 has a separate small address space for devices, reached
with the instructions `in` and `out`; Rust has no word for them, so we use inline assembly:

```rust,file=kernel/src/serial.rs
/// Writes `value` to an I/O port (the x86 `out` instruction).
unsafe fn outb(port: u16, value: u8) {
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") value) };
}
```

Sending a byte means waiting until the UART can take it, then writing it to its data register:

```rust,file=kernel/src/serial.rs
fn send(byte: u8) {
    // Line status register, bit 5: the transmit register is empty and takes the next byte
    while unsafe { inb(COM1 + 5) } & 0x20 == 0 {
        core::hint::spin_loop();
    }
    unsafe { outb(COM1, byte) };
}
```

To get `format!`-style output for free, we implement `core::fmt::Write` for a zero-sized `Serial` type and wrap
`writeln!` in a macro, `serial_println!`. (`init` sets the speed and the data format; the comments in
`serial.rs` explain each register.)

## Step 5: the entry point
```rust,file=kernel/src/main.rs
/// Limine jumps here: 64-bit long mode, paging on, interrupts off, on a stack Limine set up.
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    serial_println!("Hello from KernelTry!");
    if !BASE_REVISION.is_supported() {
        serial_println!("Limine does not support our base revision");
        halt();
    }
    serial_println!("Limine base revision {} is supported.", BaseRevision::MAX_SUPPORTED);
    halt()
}
```

- `#[unsafe(no_mangle)]` keeps the name `kmain` in the ELF file, so `ENTRY(kmain)` finds it.
- `extern "C"` uses the C calling convention, which Limine expects.
- `-> !` means "never returns": there is nothing to return to.

When the kernel is done, it **halts**: `cli` switches interrupts off, `hlt` stops the CPU until the next interrupt –
which never comes. The loop is there because some events (NMIs) can still wake the CPU.

The **panic handler** is required in `no_std`: Rust calls it on `panic!`, failed `assert!`s, out-of-range indexing.
Ours prints the message and halts.

## Step 6: a bootable CD image
`run-qemu.sh` packs the kernel and Limine's files into an ISO image with `xorriso` – one image that boots with BIOS
and with UEFI – and starts QEMU with it. `limine.conf` on the image tells Limine which file to boot and with which
protocol.

## Run it
```sh
cd course/01-boot
make run
```
The first `make` downloads Limine and builds the kernel. QEMU opens a window (empty – we do not draw anything yet)
and your terminal shows:

```text
Hello from KernelTry!
Limine base revision 6 is supported.
```

Close the QEMU window to stop it.

## The test
`make test` boots the same image without a window, writes the serial output to `build/serial.log` and passes when
every line of `expected.txt` appears in it within 30 seconds. Remove the `Hello` line from `kmain` and the test
fails with both the expected and the actual output – try it. This is the pattern for the whole course: every
chapter has a test that fails without the chapter's code.

## What can go wrong
- **Nothing on the terminal, QEMU just sits there:** usually the kernel was not found or not started. Check that
  the ISO contains `boot/kernel` and that `limine.conf` points to it (`path: boot():/boot/kernel`).
- **`error: linking with ld.bfd failed`:** GNU ld (package `binutils`) is missing.
- **"relocation truncated to fit":** code built for a different code model than the linker script expects; the
  `code-model=kernel` flag in `.cargo/config.toml` belongs to the higher-half address in `linker.ld`.
- **The kernel resets in a loop:** a *triple fault* – the CPU hit an error while handling an error. Add
  `-d int,cpu_reset -D qemu.log` to the QEMU command in `run-qemu.sh` and read the last exception in the log.
  (We will set up proper exception handlers in a later chapter.)

## Experiments
1. Request a revision Limine does not know: `BaseRevision::with_revision(99)`. What does the kernel print now?
2. Remove `#[used]` from `BASE_REVISION`. Does the kernel still say the revision is supported? Look at the ELF with
   `objdump -h kernel/target/x86_64-unknown-none/debug/kernel` before and after.
3. Panic on purpose: put `panic!("on purpose")` after the greeting. Where does the message come from?
