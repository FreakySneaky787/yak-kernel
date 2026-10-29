# 5 – Interrupts and exceptions

**Goal:** an **Interrupt Descriptor Table** with handlers for the CPU's exceptions – so that a bug in the kernel
reports what happened instead of resetting the machine.
**You end up with:** a breakpoint (`int3`) that is handled and returns; a page fault that names the address and the
cause; and a double fault that survives a broken stack, on the stack we prepared in chapter 4.
**Code:** [`course/05-interrupts/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/05-interrupts)
(chapter 4 plus `idt.rs`, `interrupts.rs`, a kernel command line and two more checks in `make test`)
**What changed:** `diff -ru course/04-gdt course/05-interrupts` (in the repository) shows every line this chapter adds.

## Background: when the CPU interrupts itself
So far, any mistake in our kernel ended the same way: the machine reset, and QEMU (started with `-no-reboot`)
closed. That is because the CPU has a fixed reaction to errors – it raises an **exception** – and we never told it
what to do then.

An exception is the CPU stopping the current instruction and calling a function instead: division by zero
(vector 0), an unknown instruction (6), access to memory no page table maps – a **page fault** (14), a broken
segment or privilege rule – a **general protection fault** (13). Each kind has a number, the **vector**. Vectors 0–31
belong to the CPU; 32–255 are free for hardware interrupts (keyboard, timer – the next chapters) and software.

The **IDT** says, for each vector, where that function is. Before calling it the CPU pushes five values onto the
stack – where the interrupted code was (RIP, CS), its flags (RFLAGS), its stack (RSP, SS) – and for some exceptions
an **error code** on top. A handler that returns must pop all of it with `iretq` instead of `ret`.

If the CPU fails *while calling a handler* – the gate is missing, or pushing the five values faults – it raises a
**double fault** (vector 8). If calling the double fault handler fails too, there is nothing left to try: a
**triple fault**, and the CPU resets. That is what we have seen until now.

## Step 1: a gate
Each IDT entry – a **gate** – is 16 bytes. Like the TSS descriptor of chapter 4, it scatters the handler's address
over three fields:

```rust,file=kernel/src/idt.rs
pub struct Entry {
    offset_low: u16,
    selector: u16,
    /// Bits 0..3: which IST stack to switch to, 1..=7 (0 = stay on the current stack).
    ist: u8,
    /// P | DPL | 0 | type; 0xE = interrupt gate: the CPU turns interrupts off while the handler runs.
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}
```

`selector` is the code segment the handler runs in – our `0x08` from chapter 4. Filling a gate means cutting the
address into pieces and marking it present:

```rust,file=kernel/src/idt.rs
    fn set_address(&mut self, address: u64) -> &mut Entry {
        self.offset_low = address as u16;
        self.offset_mid = (address >> 16) as u16;
        self.offset_high = (address >> 32) as u32;
        self.selector = KERNEL_CODE_SELECTOR;
        self.type_attr = PRESENT_INTERRUPT_GATE;
        self
    }
```

The table is 256 of them, starting out all *missing* (all zeros). It is loaded with `lidt` – the twin of `lgdt`,
with the same 10-byte pointer of size and address.

## Step 2: a function the CPU can call
A handler is not a normal function. The CPU jumps into it in the middle of other code, so it must save every
register it touches, find the pushed values on the stack and end with `iretq`. Rust's nightly compiler can generate
all of that: the **`x86-interrupt` calling convention** (feature `abi_x86_interrupt` in `main.rs`). The stack frame
becomes the first parameter:

```rust,file=kernel/src/idt.rs
pub struct InterruptStackFrame {
    /// Where the interrupted code was - for most faults the instruction that failed, so returning retries it.
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}
```

and the error code, for the exceptions that have one, the second. There are three shapes of handler, and the IDT
only accepts the right one for each kind:

```rust,file=kernel/src/idt.rs
pub type Handler = extern "x86-interrupt" fn(InterruptStackFrame);
pub type HandlerWithErrorCode = extern "x86-interrupt" fn(InterruptStackFrame, u64);
pub type DivergingHandlerWithErrorCode = extern "x86-interrupt" fn(InterruptStackFrame, u64) -> !;
```

## Step 3: the handlers
The **breakpoint** is the friendly exception: `int3` raises it on purpose (debuggers place it in code). Our handler
counts it, prints, and returns – the CPU continues after the `int3`, because for a breakpoint the pushed RIP already
points to the next instruction:

```rust,file=kernel/src/interrupts.rs
extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    BREAKPOINT_COUNT.fetch_add(1, Ordering::Relaxed);
    serial_println!("EXCEPTION: BREAKPOINT at {:#x}", frame.rip);
}
```

Every other exception in our kernel is a bug, so the handlers report and panic. The **page fault** has the most to
say: control register **CR2** holds the address the code tried to use, and the error code tells why it failed – bit 0
"the page exists but forbids this", bit 1 "it was a write", bit 2 "user mode", bit 4 "fetching an instruction":

```rust,file=kernel/src/interrupts.rs
extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, error_code: u64) {
    panic!(
        "EXCEPTION: PAGE FAULT at address {:#018x}\n  cause: {}\n{:?}",
        read_cr2(),
        PageFaultError(error_code),
        frame
    );
}
```

(`PageFaultError` turns those bits into words.) The handlers print to the serial port only, not with `println!`:
an exception can hit code that holds the console's lock, and a handler waiting for that lock would wait forever.

## Step 4: the double fault, on its own stack
Now the reason for chapter 4's TSS. Imagine the stack pointer points to memory that is not there – a stack overflow
does exactly that. The next `push` page-faults. To call the page fault handler the CPU must push the five values –
onto the same broken stack – and faults again: a double fault. If its handler used that stack too, a third failure
would reset the machine.

So the double fault gate gets an **IST index**: before pushing anything, the CPU loads RSP from the TSS's
`interrupt_stacks[0]` – the 20 KiB stack we prepared:

```rust,file=kernel/src/interrupts.rs
        // the one handler that must not use the current stack: it may be what broke
        idt.set_diverging_with_error_code(DOUBLE_FAULT, double_fault)
            .set_ist(DOUBLE_FAULT_IST);
```

The handler is *diverging* (`-> !`): a double fault cannot be returned from, because the CPU does not say how far
the first exception got. Ours checks its own stack pointer – is it really on the double fault stack? – and panics
with what it knows:

```rust,file=kernel/src/interrupts.rs
extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, error_code: u64) -> ! {
    let rsp: u64;
    unsafe { asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags)) };
    let on_ist = if gdt::on_double_fault_stack(rsp) { "yes" } else { "no" };
    serial_println!("running on the double fault stack: {} (rsp={:#x})", on_ist, rsp);
```

`interrupts::init` builds the table once (`spin::Once`, like the GDT) with six handlers – vectors 0, 3, 6, 8, 13 and
14 – and loads it. Both entry points in `main.rs` call it right after `gdt::init()`; it needs our code segment and
our TSS:

```rust,file=kernel/src/main.rs
    console::init();
    gdt::init();
    interrupts::init();
    println!("Hello from YAK!");
```

## Step 5: crashing on purpose
A handler for a crash is only proven by a crash. The normal `kmain` now ends with a breakpoint – it must come back –
and then, if asked, a real fault:

```rust,file=kernel/src/main.rs
    unsafe { core::arch::asm!("int3") };
    println!("Back from the breakpoint handler - the kernel carries on.");
    match requested_fault() {
        Some("page") => interrupts::provoke_page_fault(),
        Some("stack") => interrupts::provoke_double_fault(),
```

Who asks? The **kernel command line**: a `cmdline:` line in `limine.conf`, which Limine hands to the kernel when we
ask with an `ExecutableCmdlineRequest` (one more request next to the framebuffer). `run-qemu.sh` adds that line when
the variable `KERNEL_CMDLINE` is set. `fault=page` writes to address `0xdeadbeef`, which no page table maps;
`fault=stack` points the stack there and pushes:

```rust,file=kernel/src/interrupts.rs
pub fn provoke_double_fault() -> ! {
    unsafe { asm!("mov rsp, {}", "push rax", in(reg) UNMAPPED & !0xF, options(noreturn)) }
}
```

## The tests
`make test` now runs 22 tests in the kernel; six are new:
- `a_gate_scatters_the_handler_address` – the bit puzzle of step 1 with a known address;
- `idtr_points_to_our_table` – `sidt` returns our table, 256 × 16 bytes;
- `breakpoints_return` – two `int3`, the counter goes up by two, and the test goes on;
- `the_double_fault_gate_switches_to_ist1` – present, IST 1, and pointing at our handler;
- `only_the_exceptions_we_handle_are_present` – all 256 gates, so a forgotten or extra one shows up;
- `the_page_fault_error_code_in_words` – error code `0b10` reads "page not present, write, kernel mode".

Then it boots the normal kernel three times: without a command line (`expected.txt`, which includes the breakpoint
and the line after it), with `fault=page` (`expected-page-fault.txt`) and with `fault=stack`
(`expected-double-fault.txt`, which demands "running on the double fault stack: yes"). Remove `.set_ist(…)` and
the last check fails within a second: QEMU ends by itself – a triple fault – and `run-qemu.sh` says so. Without
`interrupts::init()` in the test `kmain`, `idtr_points_to_our_table` fails.

## Run it
```sh
cd course/05-interrupts
make run
```
```text
GDT loaded: code segment 0x8, TSS 0x18, double fault stack top 0xffffffff8001a150
IDT loaded: exceptions 0, 3, 6, 8 (on IST1), 13, 14
EXCEPTION: BREAKPOINT at 0xffffffff800097ae
Back from the breakpoint handler - the kernel carries on.
```
(after the lines of chapter 4; the addresses depend on your build.) Then crash it:

```sh
KERNEL_CMDLINE=fault=page make run
```
```text
KERNEL PANIC: panicked at src/interrupts.rs:76:5:
EXCEPTION: PAGE FAULT at address 0x00000000deadbeef
  cause: page not present, write, kernel mode
rip=0xffffffff800055be cs=0x8 rflags=0x10086 rsp=0xffff800007f8edc0 ss=0x10
```
```sh
KERNEL_CMDLINE=fault=stack make run
```
```text
running on the double fault stack: yes (rsp=0xffffffff80019fd0)
KERNEL PANIC: panicked at src/interrupts.rs:90:5:
EXCEPTION: DOUBLE FAULT (code 0x0), last page fault at 0xdeadbed8
rip=0xffffffff80005629 cs=0x8 rflags=0x2 rsp=0x00000000deadbee0 ss=0x10
```
Read the double fault's frame: RSP was `0xdeadbee0` – the broken stack – and the page fault was at `0xdeadbed8`, the
8 bytes the `push` wanted to write. The handler itself ran 20 KiB higher, on its own stack.

## What can go wrong
- **A double fault still resets the machine:** the gate has no IST index, or the TSS holds the *bottom* of the
  stack instead of the top (chapter 4). `-d int,cpu_reset -D qemu.log` on the QEMU command line shows the chain:
  `v=0e e=0002` (page fault on a write), `v=08` (double fault), then `Triple fault` and a CPU reset.
- **A general protection fault with error code `0x102`:** bit 1 says "an IDT entry", `0x102 >> 3 = 0x20` says which
  – vector 32, which has no gate. The reference kernel hit exactly this when a test loaded its own IDT without the
  timer's vector while interrupts were on ([bug #10](../BUGS.md)). In this chapter interrupts stay off; the next
  chapter turns them on.
- **Garbage in the frame, then a crash on return:** a handler declared without the error code for an exception
  that pushes one (8, 10–14, 17, 21, 29, 30), or the other way round. Every field is read one place off – RIP shows the error
  code – and `iretq` jumps to it. The three handler types in `idt.rs` exist so the compiler catches the obvious case.
- **The kernel hangs inside a handler:** the handler printed with `println!` while the interrupted code held the
  console's lock. Use the serial port in handlers.
- **`error[E0658]: the extern "x86-interrupt" ABI is experimental and subject to change`:** the
  `#![feature(abi_x86_interrupt)]` line in `main.rs` is missing, or you are not on the pinned nightly compiler (`rust-toolchain.toml`).

## Experiments
1. Remove `.set_ist(DOUBLE_FAULT_IST)` and run with `fault=stack` – once plain, once with
   `-d int,cpu_reset -D qemu.log` added to the QEMU command in `run-qemu.sh`. Which vectors does the log show, in
   which order?
2. Divide by zero: `asm!("xor edx, edx", "xor eax, eax", "div eax")`. Then remove the divide error gate and try
   again – which exception do you get now, with which error code?
3. Write to `0xdeadbeef` once more, but *read* first (`read_volatile`). Which bit of the error code changes? Then
   try executing it: `asm!("call {}", in(reg) 0xdeadbeefu64)`.
