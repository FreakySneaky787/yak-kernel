# 12 – ACPI and the APIC

**Goal:** replace the PIC of 1981 with the interrupt controllers of every PC since the late 1990s – and find them
the way a real kernel does, in the firmware's ACPI tables.
**You end up with:** a parser for the RSDP, the RSDT/XSDT and the MADT; the local APIC switched on, with its own
timer measured against the PIT; the I/O APIC routing the timer and the keyboard; and QEMU started with a local APIC
again. This chapter begins part IV.
**Code:** [`course/12-apic/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/12-apic) (chapter 11
plus `acpi.rs`, `lapic.rs`, `ioapic.rs`, an uncached mapping in `paging.rs`, and `interrupts::start` instead of
`start_timer`/`start_keyboard`)
**What changed:** `diff -ru course/11-shell course/12-apic` (in the repository) shows every line this chapter adds.

## Background: two APICs, and a map of the wiring
An x86 PC has two kinds of APIC (*advanced programmable interrupt controller*):

```text
 keyboard --+                                         +--> local APIC --> CPU 0
 PIT -------+--> I/O APIC: 24 inputs, each with   ----+    (EOI, timer)
 disk, ... -+    "vector v on the CPU with ID c"      +--> local APIC --> CPU 1 ...
```

- The **I/O APIC** is where the interrupt lines of the devices end. For each input a *redirection entry* says which
  vector to raise on which CPU – or that the input is masked. It replaces the PIC's fixed "line n is vector 32 + n".
- Every CPU core has a **local APIC**. Every interrupt reaches the core through it, and it wants an end of
  interrupt after each one, like the PIC. It also has a timer of its own – one per CPU, which a scheduler on
  several CPUs will need.

Chapter 6 already met the local APIC: Limine leaves its input for the PIC (LINT0) masked, and under QEMU 8.2 no
PIC interrupt arrived while it existed ([bug #25](../BUGS.md)). So chapters 6–11 ran on a CPU without one
(`-cpu qemu64,-apic`). This chapter switches the PIC off instead and starts QEMU with `-cpu qemu64`.

Where are these controllers? Their registers are memory at physical addresses, and not every machine uses the same
ones. The firmware says where, in its **ACPI tables**: Limine hands us the **RSDP**, which points to the **RSDT**
(or, since ACPI 2.0, the **XSDT** with 64-bit addresses), a list of all other tables. The one we need is the
**MADT** (signature `APIC`): the CPUs, the I/O APICs, the local APIC's address – and the *interrupt source
overrides*, the ISA lines that are not wired the obvious way. On almost every PC, and in QEMU, one of them is the
timer: ISA IRQ 0 arrives at I/O APIC input 2.

## Step 1: reading the ACPI tables
Every table starts with the same 36-byte header: a four-letter signature, the length, and a checksum – all bytes of
the table add up to 0:

```rust,file=kernel/src/acpi.rs
pub fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)) == 0
}
```

`acpi::read` checks the RSDP (`RSD PTR `, checksum), takes the RSDT or the XSDT depending on the RSDP's revision,
checks that table, and walks its entries. Limine gives the RSDP as an HHDM address, and Limine maps the tables into
the HHDM too (the memory map's "reserved, but in the HHDM" lines); a page it left out would be mapped by `bytes`.

The MADT's body is a list of records, each starting with a type and a length. We read four types and skip the
rest by their length:

```rust,file=kernel/src/acpi.rs
        match kind {
            // a CPU's local APIC; flag bit 0: enabled, bit 1: can be enabled later
            0 if u32_at(entry, 4) & 0b11 != 0 => cpus += 1,
            1 if io_apic.is_none() => io_apic = Some((PhysAddr(u32_at(entry, 4) as u64), u32_at(entry, 8))),
```

Type 2 is an override: ISA IRQ `entry[3]` arrives at *global system interrupt* (GSI) `u32_at(entry, 4)` – the
number of the I/O APIC input. `Madt::gsi(irq)` returns it, or `irq` itself if there is no override. An override
also says whether a line is active-low or level-triggered; QEMU's say so for IRQ 5, 9, 10 and 11 (flags `0xd`),
not for the timer and the keyboard, so this chapter does not read them. The parser is plain code over a byte slice,
so a test feeds it a hand-made MADT, too.

## Step 2: device registers, mapped uncached
The local APIC's registers are 4 KiB at `0xfee00000`, the I/O APIC's at `0xfec00000`. They are not RAM, so they are
not in Limine's HHDM – a read there page-faults. Chapter 9's `map` puts them there, with one more flag:

```rust,file=kernel/src/paging.rs
    /// Device registers: like data, but never cached - a cached read would return an old value, not the device's.
    pub const DEVICE: Flags = Flags(Self::DATA.0 | Self::NO_CACHE.0);
```

Every access to them is `read_volatile`/`write_volatile`: to the compiler, two reads of the same address with no
write between give the same value, and it may drop the second – for a register that changes on its own, wrong.

This is also why the interrupts now start *after* memory: the registers must be mapped (chapter 9, frames for new
tables from chapter 8), and the MADT's lists are `Vec`s (chapter 10). `kmain` calls `interrupts::start` after the
heap demonstration; until then the kernel runs without hardware interrupts, which nothing before needs.

## Step 3: the local APIC and the I/O APIC
The local APIC is switched on through its *spurious interrupt vector* register: bit 8 enables it, the low byte is
the vector it raises for an interrupt that vanished before it could deliver it (such a spurious interrupt gets no
end of interrupt). The end of interrupt itself is a write of 0 to register `0xB0` – the timer and keyboard handlers
call `lapic::end_of_interrupt` instead of the PIC's.

The I/O APIC has only two registers: write a register number to `SELECT`, then read or write it through `WINDOW`.
Input `n`'s redirection entry is registers `0x10 + 2n` and `0x11 + 2n`. `init` masks all 24 inputs; `route` writes
one entry – the vector in the low byte, the destination CPU's APIC ID in the top byte, everything else 0 (a normal
interrupt, edge-triggered, active high – what ISA lines are):

```rust,file=kernel/src/ioapic.rs
pub fn route(gsi: u32, vector: u8, cpu: u8) {
    set_entry(input(gsi), vector as u64 | (cpu as u64) << 56);
}
```

The PIC is still in the machine. `pic::init` moves it off the exception vectors and masks every line – and that is
all this chapter does with it.

## Step 4: a timer that has to be measured
The local APIC timer counts down from a number we write, raises its vector at zero, and in periodic mode starts
again. How fast it counts depends on the machine, so we measure it against a clock we know: the PIT, for a tenth of
a second. `start` puts the steps together:

```rust,file=kernel/src/interrupts.rs
    // The PIT is our clock for a moment: its IRQ 0 arrives at the I/O APIC input the MADT says (2, not 0)
    pit::init(time::TICKS_PER_SECOND);
    ioapic::route(madt.gsi(TIMER_IRQ), TIMER_VECTOR, cpu);
    unsafe { asm!("sti", options(nomem, nostack)) };
    // ... long enough to measure the local APIC timer against it; then the APIC timer takes over the same vector
    let counts_per_tick = lapic::measure_timer(CALIBRATION_TICKS);
    ioapic::mask(madt.gsi(TIMER_IRQ));
    lapic::start_timer(TIMER_VECTOR, counts_per_tick);
```

`measure_timer` waits for the next tick, starts the APIC timer at `u32::MAX` with its interrupt masked, waits ten
ticks, and reads how far it got. Both sources use vector 32 and the same handler, so `time` does not notice the
change – which is why the PIT's input is masked *before* the APIC timer starts.

The PIT now runs in mode 2 (one short pulse per period), not mode 3 (a square wave) as in chapters 6–11. In mode 3
the I/O APIC delivered two interrupts per period – 200 Hz – and the measurement came out at half the right value
([bug #27](../BUGS.md), found while writing this chapter).

What the reference kernel ([`kernel/src/arch/x86_64/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/kernel/src/arch/x86_64))
does besides: it measures the APIC timer with PIT channel 2 by polling, without any interrupt; it reads the
overrides' polarity and trigger mode; it uses the local APIC to start and wake the other CPUs (inter-processor
interrupts); and it routes more devices – the serial port, the disk, the network card.

## The tests
`make test` now runs 82 tests in the kernel. Twelve are new; two PIC tests of chapter 6 became one
(`every_line_is_masked`):
- `the_checksum_adds_up_to_zero`, `a_handmade_madt_is_parsed` (two CPUs of which one is enabled, an I/O APIC, an
  override, an unknown type), `a_madt_without_io_apic_is_refused`;
- `qemus_tables_describe_qemus_machine` – the real tables: `APIC` and `FACP` among them, one CPU, the addresses, and
  IRQ 0 at input 2;
- `the_cpu_has_a_local_apic` (CPUID), `the_registers_are_mapped_uncached`, `the_apic_is_on_and_built_into_the_cpu`,
  `its_id_is_the_one_cpuid_reports`, `the_timer_runs_periodically_on_vector_32`;
- `the_timer_was_measured_against_a_100_hz_clock` – QEMU's local APIC counts at 1 GHz; divided by 16 that is
  625 000 per tick, and the test wants 550 000 to 700 000 (a busy host delays the PIT's ticks a little, bug #29).
  It is the only test that checks time against a second clock – the one that catches bug #27, which halves it;
- `qemus_io_apic_has_24_inputs`, `only_the_keyboard_is_routed` – the PIT's input is masked again.

The boot checks of chapter 11 run unchanged: the shell is typed into through the I/O APIC now.

One test failed on its first run and taught something: `the_registers_are_mapped_uncached` compared the page's
flags with `==` and got `0x8000000000000073` – bits 5 and 6 more than `DEVICE`. The CPU sets them itself:
*accessed* when a page is read, *dirty* when it is written. The test now checks `contains`.

## Run it
```sh
cd course/12-apic
make run
```
After the heap lines:

```text
ACPI: RSDP revision 0, 5 tables: FACP APIC HPET MCFG WAET
MADT: 1 CPU, local APIC at 0xfee00000, I/O APIC at 0xfec00000 with 24 inputs
Timer: the PIT's IRQ 0 arrived at I/O APIC input 2; the local APIC timer counts 624964 per tick of 10 ms
Timer: now the local APIC timer of CPU 0 raises vector 32, 100 Hz; input 2 is masked: yes
sleep_ms(1000) took 1010 ms (at least 1000: yes), the CPU waited in hlt
Keyboard: IRQ 1 arrives at I/O APIC input 1, vector 33 (US layout)
Shell: type help and Enter
```
Revision 0 is ACPI 1.0: QEMU's BIOS (SeaBIOS) gives an RSDT. The count per tick differs a little from run to run –
it is measured. QEMU 8.2.2 printed `624407`.

## What can go wrong
Three of these mistakes look the same: the kernel prints the heap lines and then nothing, for ever. No tick
arrives, so `measure_timer` waits for one that never comes.
- **No end of interrupt:** take `lapic::end_of_interrupt()` out of the timer handler. The first tick is handled,
  the local APIC waits for its EOI, and delivers no second one.
- **The override ignored:** route IRQ 0 to input 0 instead of `madt.gsi(0)`. Input 2, where the PIT really is,
  stays masked.
- **The APIC not switched on:** leave bit 8 out of the spurious register.
- **The PIT not masked:** leave out `ioapic::mask`, and two clocks drive vector 32. Nothing hangs, every test is
  green, `sleep_ms(1000)` even says 1010 ms – but `sleep 5000` in the shell returns after 2.7 s on a watch. The
  kernel measures time in its own ticks, and those come twice as often.
- **The PIT in mode 3:** bug #27 – the same symptom, for the length of the measurement: the test says
  `311689 counts per tick`.

## Experiments
1. Start QEMU with `-smp 4` (in `run-qemu.sh`). How many CPUs does the MADT list now? Which APIC IDs do they have?
2. Print every override with its flags. Which lines are level-triggered, and what are they for (look up IRQ 9 in
   the FADT – `FACP` – as "SCI")?
3. Route the keyboard to vector 50 instead of 33. What else has to change for it to work?
4. Use the timer's *one-shot* mode instead of periodic: start it again in the handler. When would a kernel want
   that rather than a fixed beat?

The rest of part IV builds on this timer: threads, and a scheduler that its ticks switch between them.
