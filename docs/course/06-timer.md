# 6 – Hardware interrupts and the timer

**Goal:** the first interrupt that does not come from our own code – a timer, 100 times per second – and on top of
it a clock and `sleep_ms`.
**You end up with:** `sleep_ms(1000)` waits a second while the CPU sleeps in `hlt`; `make test` proves the ticks
arrive and that a sleep never ends early.
**Code:** [`course/06-timer/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/06-timer) (chapter 5
plus `port.rs`, `pic.rs`, `pit.rs`, `time.rs` and three IRQ handlers)
**What changed:** `diff -ru course/05-interrupts course/06-timer` (in the repository) shows every line this chapter adds.

## Background: IRQs and the PIC
In chapter 5 the CPU interrupted itself. Devices need the same thing: the keyboard has a key, the disk has data, the
timer's time is up – and the CPU should not have to ask them every microsecond. So a device raises an
**interrupt request (IRQ)** line, and the CPU calls a handler through the IDT, exactly as for an exception.

Between the devices and the CPU sits an **interrupt controller**. The classic one, still emulated by every PC and by
QEMU, is the **8259 PIC**: two chips of 8 lines each, IRQ 0–15, the second chained to line 2 of the first. Line 0 is
the timer, line 1 the keyboard (next chapter). The PIC turns a line into a vector – its *offset* plus the line number
– and it has two habits we must handle:

- **Its offset starts at 8.** IRQ 0, the timer, would arrive as vector 8 – the double fault. That was a design
  mistake of the original PC; every x86 kernel starts by moving the PIC elsewhere. We move it to 32–47.
- **It waits for an answer.** After delivering an IRQ the PIC holds back further IRQs of that priority until the
  handler sends an **end of interrupt (EOI)**.

The timer itself is the **PIT**, a counter running at 1.193182 MHz that counts down from a number we choose and
raises IRQ 0 each time it reaches zero.

## Step 1: I/O ports, shared
PIC and PIT live in the *I/O port* address space, like the serial port of chapter 1. The two little functions
`inb`/`outb` that were private to `serial.rs` move into `port.rs`, and `serial.rs` imports them:

```rust,file=kernel/src/serial.rs
use crate::port::{inb, outb};
```

## Step 2: moving the PIC
The PIC is programmed with four *initialization command words* in a fixed order. The second one is the offset:

```rust,file=kernel/src/pic.rs
    let commands = [
        (MASTER_COMMAND, 0x11),       // ICW1 master: start initialization, ICW4 follows
        (SLAVE_COMMAND, 0x11),        // ICW1 slave
        (MASTER_DATA, MASTER_OFFSET), // ICW2: the vector of IRQ 0
        (SLAVE_DATA, SLAVE_OFFSET),   //       the vector of IRQ 8
        (MASTER_DATA, 1 << 2),        // ICW3 master: a slave hangs on line 2
        (SLAVE_DATA, 2),              // ICW3 slave: you hang on line 2
        (MASTER_DATA, 0x01),          // ICW4: 8086 mode
        (SLAVE_DATA, 0x01),
    ];
```

After that every line is masked (a set bit in the mask register blocks it), and `unmask` opens single lines. The
answer to an IRQ is one byte, `0x20`, to the command port – to both chips if the IRQ came through the second:

```rust,file=kernel/src/pic.rs
pub fn end_of_interrupt(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(SLAVE_COMMAND, END_OF_INTERRUPT);
        }
        outb(MASTER_COMMAND, END_OF_INTERRUPT);
    }
}
```

## Step 3: the local APIC is in the way
Modern CPUs have a newer interrupt controller in every core, the **local APIC**, and the PIC's signal enters it on
a pin called LINT0. Limine leaves that pin masked. With the APIC on, our PIC could ring forever – nothing would
arrive. The simplest way out for now is to switch the local APIC off, so the PIC talks to the CPU directly, as on a
PC of 1990. That is bit 11 of the *model-specific register* `IA32_APIC_BASE`, written with `wrmsr`:

```rust,file=kernel/src/pic.rs
pub fn disable_local_apic() {
    let base = read_msr(IA32_APIC_BASE);
    unsafe { write_msr(IA32_APIC_BASE, base & !APIC_GLOBAL_ENABLE) };
}
```

The reference kernel found this the hard way ([bug #2](../BUGS.md): the timer never arrived, and QEMU's
`info lapic` showed LINT0 masked). It later switched to the APIC and masked the PIC instead – a topic for part IV.

## Step 4: the timer
For 100 interrupts per second the PIT counts down from 1193182 / 100 = 11931. One command byte selects channel 0
and the mode, then the number goes in, low byte first:

```rust,file=kernel/src/pit.rs
pub fn init(hz: u32) {
    let divisor = divisor(hz);
    unsafe {
        // channel 0, low byte then high byte, mode 3 (square wave: reload and repeat), binary counting
        outb(COMMAND, 0b0011_0110);
        outb(CHANNEL0, divisor as u8);
        outb(CHANNEL0, (divisor >> 8) as u8);
    }
}
```

The handler on vector 32 counts the tick and answers the PIC:

```rust,file=kernel/src/interrupts.rs
extern "x86-interrupt" fn timer(_frame: InterruptStackFrame) {
    time::tick();
    // without it, the PIC would never send the next tick
    pic::end_of_interrupt(TIMER_IRQ);
}
```

The counter is an `AtomicU64`: the handler writes it in the middle of whatever the kernel is doing, and the rest of
the kernel reads it – an atomic guarantees nobody ever sees half a value. Two more small handlers sit on vectors 39
and 47 for *spurious* IRQs – the PIC reports line 7 or 15 when a signal vanished before it could deliver it, and
then it must not get an EOI (`interrupts.rs` explains the rule).

Everything is started in one place, in an order that matters – the gates first (chapter 5's `interrupts::init`),
then the hardware, and only at the end `sti`, which lets interrupts in:

```rust,file=kernel/src/interrupts.rs
pub fn start_timer() {
    pic::disable_local_apic();
    pic::init();
    pit::init(time::TICKS_PER_SECOND);
    pic::unmask(TIMER_IRQ);
    unsafe { asm!("sti", options(nomem, nostack)) };
}
```

Both entry points in `main.rs` call `interrupts::start_timer()` after `interrupts::init()`: the normal `kmain`
after the breakpoint test, the test `kmain` before `test_main()`.

## Step 5: time and sleep
With a tick every 10 ms, uptime is `ticks × 10`. Sleeping means: compute the tick to wake at, then `hlt` until it
has come – `hlt` stops the CPU until the next interrupt, so the kernel waits without doing any work:

```rust,file=kernel/src/time.rs
pub fn ticks_for(ms: u64) -> u64 {
    ms.div_ceil(MS_PER_TICK) + 1
}
```

Why the `+ 1`? The tick counter says *which* tick it is, not how far into it we are. A sleep that starts just
before a tick ends would count that tick fully and wake up almost 10 ms early. The reference kernel had exactly
this bug ([bug #18](../BUGS.md)); with one tick more a sleep lasts at least `ms` and at most `ms + 10`.

```rust,file=kernel/src/time.rs
pub fn sleep_ms(ms: u64) {
    let deadline = ticks() + ticks_for(ms);
    while ticks() < deadline {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}
```

`kmain` now starts the timer and sleeps one second:

```rust,file=kernel/src/main.rs
    let before = time::uptime_ms();
    time::sleep_ms(1000);
    let slept = time::uptime_ms() - before;
    let enough = if slept >= 1000 { "yes" } else { "NO" };
```

## The tests
`make test` now runs 29 tests in the kernel; seven are new:
- `the_local_apic_is_off` – bit 11 of `IA32_APIC_BASE` is clear;
- `only_the_timer_is_let_through` – the masks are `0b1111_1110` and `0b1111_1111`;
- `irqs_start_after_the_exceptions` – IRQ 0 is vector 32, IRQ 15 vector 47;
- `the_divisor_for_100_hz` – 11931, and the slowest the 16-bit counter can do;
- `ticks_advance_while_the_cpu_halts` – five ticks arrive while the test sits in `hlt`;
- `a_deadline_rounds_up_and_adds_one` – `ticks_for` for 0, 1, 10, 11 and 1000 ms;
- `sleep_never_ends_early` – five sleeps of 30 ms, each at least 4 ticks.

(`only_the_vectors_we_handle_are_present` from chapter 5 now expects the gates 32, 39 and 47 too.) Then the three
boot checks of chapter 5 run again; the normal one now needs "at least 1000: yes". The tests measure only lower
bounds for time: how late an emulated machine wakes up depends on the host, and the reference kernel learned that
an upper bound makes a test flaky ([bug #24](../BUGS.md)).

Without `disable_local_apic()` the kernel stops after the "Timer" line – the tick never comes and `sleep_ms` waits
forever; without the EOI exactly one tick arrives and the tick test runs into the time limit.

## Run it
```sh
cd course/06-timer
make run
```
After the lines of chapter 5, and a second of nothing:

```text
Timer: IRQ 0 on vector 32, 100 Hz
sleep_ms(1000) took 1010 ms (at least 1000: yes), the CPU waited in hlt
```
1010, not 1000: the one extra tick from step 5.

## What can go wrong
- **The kernel hangs in the first `sleep_ms`:** no tick arrives. In order of likelihood: interrupts are off (no
  `sti`), the line is masked, the local APIC is still on, or the PIT was not programmed. The QEMU monitor shows the
  PIC's view: `info pic` lists `irr` (requested), `imr` (masked) and `irq_base` (the offset).
- **Exactly one tick, then silence:** the handler does not send the EOI.
- **A "double fault" with a nonsense frame as soon as interrupts are on:** the PIC was not moved, and IRQ 0 arrived
  on vector 8. A real double fault pushes an error code, an IRQ does not – so the handler reads everything one place
  off. Removing `pic::init()` from `start_timer` gives exactly this (with BIOS):

  ```text
  EXCEPTION: DOUBLE FAULT (code 0xffffffff80009a65), last page fault at 0x0
  rip=0x0000000000000008 cs=0x293 rflags=0xffff800007f8ece0 rsp=0x0000000000000010 ss=0x0
  ```
  The "error code" is the interrupted RIP, "rip" is the code segment `0x8`, and so on.
- **A general protection fault with error code `0x102` right after `sti`:** bit 1 says "an IDT entry",
  `0x102 >> 3 = 32` says which – the timer's line is open but its gate is missing (remove the `idt.set` for
  `TIMER_IRQ` and you get exactly this). The same holds for any IRQ: unmask only the lines you have a handler for.
- **The kernel hangs in `println!` from a handler:** the interrupted code held the console's lock (chapter 5). The
  timer handler prints nothing for this reason.

## Experiments
1. Set `TICKS_PER_SECOND` to 1000. Does `sleep_ms(1000)` still take about a second? What does `ticks_for` compute
   now – and is `MS_PER_TICK` still right?
2. Remove the `+ 1` in `ticks_for`. Which test fails – always, or only sometimes? Why?
3. Count how often the loop in `sleep_ms` runs during `sleep_ms(1000)`. What does that say about `hlt`?
4. Replace `hlt` in `sleep_ms` by `core::hint::spin_loop()` and watch your host's CPU load while `make run` sleeps.
