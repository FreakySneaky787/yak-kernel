# 19 – A block cache, and the disk's interrupt

**Goal:** stop doing work twice and stop waiting actively. The file system reads the same FAT sectors again and
again, and the CPU spins while the disk works. A cache in RAM answers the repeats; an interrupt tells the kernel
when a command is done, so the waiting thread can sleep.
**You end up with:** a write-through sector cache with least-recently-used replacement under the file system;
message signaled interrupts (MSI) from the SATA controller; disk commands that wait in `task::wait_for` – and a
count of what it saves. The end of part V.
**Code:** [`course/19-disk-irq/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/19-disk-irq)
(chapter 18 plus `BlockCache` in `block.rs`, MSI in `pci.rs`, the interrupt in `ahci.rs` and `interrupts.rs`)
**What changed:** `diff -ru course/18-fat32-write course/19-disk-irq` (in the repository) shows every line this
chapter adds.

## Background: measure first
"The disk is slow, add a cache" is the obvious idea, and the reference kernel found out it was only half true
(`docs/LEARN.md`, M32): QEMU's disk answers in microseconds, and most of the time went into asking it the same
things again. So this chapter counts first. Reading `/big.bin` (196 clusters) through chapter 18's file system costs
**394** disk commands:

- 2 for the root directory (its FAT entry and its one cluster),
- 196 for the FAT – one sector read per FAT entry, although one FAT sector holds 128 entries,
- 196 for the data.

The data has to come from the disk once. The FAT reads are repeats: the cache's job.

The other cost is invisible in that number: during each command the CPU spins in `wait_until`. With four CPUs and a
scheduler (chapter 15), that is a CPU that could run another thread. The controller can say when it is done – by
interrupt.

## Step 1: a cache of sectors
`BlockCache<D>` is a `BlockDevice` in front of another one. It keeps sectors in a `BTreeMap` from sector number to
data, each with the time it was last used (a counter, `clock`). A read takes what the cache has and fetches each
*run* of missing sectors with one read of the device. A write goes to the device first:

```rust,file=kernel/src/block.rs
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        check_request(self.sector_count(), lba, buf.len())?;
        self.device.write(lba, buf)?; // the disk first: if this fails, the cache still has the old data
        for (i, sector) in buf.chunks(SECTOR_SIZE).enumerate() {
            self.keep(lba + i as u64, sector);
        }
        Ok(())
    }
```

That is *write-through*: the disk is never behind the cache, so everything chapter 18 said about the order of
writes and about `fsck.fat` still holds. (A *write-back* cache would collect writes and save disk commands – and lose
them, and break that order, when the machine stops.)

When the cache is full, `keep` drops the sector whose `used` is smallest – the least recently used. It looks at every
sector for that; with 2048 sectors that is cheap next to one disk command.

The file system now sits on `fs::Disk`, the cache in front of `SystemDisk`:

```rust,file=kernel/src/fs.rs
pub type Disk = BlockCache<SystemDisk>;
pub const CACHE_SECTORS: usize = 2048;
```

One rule comes with it: everything that *writes* to the disk must go through the cache, or the cache holds old
data. The shell's `read` and the tests' own mounts only read, so they may go around it.

## Step 2: MSI – an interrupt without a wire
Chapter 12 routed the keyboard's interrupt line through the I/O APIC, and the MADT said which input it arrives at.
For a PCI device the wiring is described in ACPI's AML bytecode, which this kernel does not interpret. PCI has a
simpler way: **message signaled interrupts**. The device does not raise a line; it *writes* a value to a memory
address – an address that belongs to the local APICs (0xFEE0_0000, with the APIC ID in bits 12–19). The APIC turns
the write into an interrupt with the vector in the value.

MSI is a *capability*, one of the extra register blocks in a linked list that starts at offset 0x34 of the
configuration space. `pci::Device::capability` walks the list (at most 48 steps – a broken list could be a loop);
`enable_msi` writes the address and the vector, switches MSI on and the old interrupt pin off:

```rust,file=kernel/src/pci.rs
    pub fn enable_msi(&self, vector: u8, apic_id: u8) -> bool {
        let Some(msi) = self.capability(CAPABILITY_MSI) else {
            return false;
        };
        let control = self.location.read(msi) >> 16;
        let is_64bit = control & (1 << 7) != 0;
        self.location.write(msi + 4, 0xFEE0_0000 | (apic_id as u32) << 12); // the address: which APIC
        let data = if is_64bit {
            self.location.write(msi + 8, 0); // the high half of the address
            msi + 12
        } else {
            msi + 8
        };
        self.location.write(data, vector as u32); // the data: fixed delivery, edge, this vector
        // control: enable (bit 0), one message only (bits 4-6 = 0); the low half (id, next) cannot be written
        self.location.write(msi, ((control & !(0b111 << 4)) | 1) << 16);
        let command = self.location.read(COMMAND) & 0xFFFF;
        self.location.write(COMMAND, command | COMMAND_NO_PIN_INTERRUPT);
        true
    }
```

`ahci::init` calls it with vector 0x50 and the APIC ID of the CPU it runs on, then sets GHC.IE (the controller may
interrupt) and, per port, PxIE: "the disk answered" (the register FIS that ends every command) and "task file
error". `interrupts.rs` puts a handler on vector 0x50 – and the old test that only the vectors we handle are present
in the IDT noticed the new one at once.

## Step 3: the handler, and sleeping until it comes
The handler finds the ports that report something, notes errors, clears the status bits (by writing 1 – first in the
port, then in the HBA) and wakes whoever waits:

```rust,file=kernel/src/ahci.rs
pub fn handle_interrupt() {
    let hba = HBA.load(Ordering::Relaxed);
    if hba == 0 {
        return;
    }
    let pending = read32(hba + HBA_IS);
    for port in (0..32).filter(|port| pending & (1 << port) != 0) {
        let registers = hba + 0x100 + port as u64 * 0x80;
        let status = read32(registers + PORT_IS);
        if status & IS_TASK_FILE_ERROR != 0 {
            FAILED_PORTS.fetch_or(1 << port, Ordering::Relaxed);
        }
        write32(registers + PORT_IS, status);
    }
    write32(hba + HBA_IS, pending);
    INTERRUPTS.fetch_add(1, Ordering::Relaxed);
    task::wake(EVENT);
}
```

The errors go into `FAILED_PORTS` because the handler has just cleared PxIS – the waiting thread would not find the
error bit there any more. And the command waits with the `wait_for` of chapter 15, on `Wait::Event(0x50)` – the
variant that chapter added for exactly this:

```rust,file=kernel/src/ahci.rs
        if USE_INTERRUPTS.load(Ordering::Relaxed) && interrupts::enabled() {
            // sleep until the interrupt handler wakes us. `wait_for` announces the wait before it asks `done`
            // (chapter 15): an interrupt that comes in between is not lost.
            task::wait_for(EVENT, || done().then_some(()));
        } else {
```

Here chapter 15 pays off. The interrupt can come at any moment – even before the thread has gone to sleep, on another
CPU than the one the thread runs on (the MSI goes to one APIC; the thread may be anywhere). `wait_for` marks the
thread blocked *first* and asks `done` second: an interrupt before the question is seen by the question; one after it
finds the thread blocked and wakes it. Without MSI, or with interrupts off on this CPU, the driver polls as before.

## The tests
`make test` runs 137 tests in the kernel; new:
- `the_cache_reads_a_sector_once` (runs of missing sectors become one read; hits and misses counted),
  `writes_go_through_to_the_device`, `the_least_recently_used_sector_goes_first` – on a counting disk in RAM;
- `the_sata_controller_has_msi` (the capability list, MSI switched on), `commands_end_by_interrupt` (16 commands,
  and interrupts came);
- the shell's `disk` now reports commands, interrupts and the cache, and the test checks "done by interrupt".

Everything of chapter 18 runs on top of the cache unchanged – including `check-disk.sh`, so `fsck.fat` and mtools
still check what the kernel wrote.

Counter-check: with the `task::wake` in the handler commented out, the test kernel hung (timeout after 40 s, twice)
– already in `ahci::init`, at IDENTIFY. So the commands really wait for the interrupt; nothing polls behind its back.

## Run it
```sh
cd course/19-disk-irq
make run
```
After the FAT32 lines:

```text
Cache: reading /big.bin took 394 disk commands without the cache, 196 through it, 0 the second time
AHCI: commands end by interrupt: yes, 847 interrupts so far
```

196 through the cache, not 198: the FAT is already in it – `mount` read the whole FAT to count free clusters, through
the cache. The second read needs no disk at all. In the shell, `disk` shows the counters:

```text
yak> disk
QEMU HARDDISK on port 0: 131072 sectors of 512 bytes
commands: 847 sent, done by interrupt (847 interrupts)
cache: 1207 of 2048 sectors, 596 hits, 1207 misses
```

(the numbers of one run, straight after boot; they grow with every command, and there may be fewer interrupts than
commands – see below).

## What can go wrong
- **Writing around the cache:** a write that goes straight to the disk leaves an old copy in the cache, and the next
  read returns it. Everything that writes uses `fs::FS`.
- **Clearing the status before reading it:** the handler must look at PxIS before it clears it, and keep what the
  thread needs (`FAILED_PORTS`).
- **Two interrupts, one delivery:** an MSI that arrives while the same vector is still pending in the local APIC is
  merged with it. So there may be fewer interrupts than commands – harmless, because `done` asks the controller, not a
  counter. The reference kernel's tests once expected one interrupt per command ([bug #8](../BUGS.md)).
- **No timeout any more:** `wait_until` gave up after a second; `wait_for` sleeps until it is woken. A disk that never
  answers now hangs the thread for good – the price of sleeping, and the reason real drivers add a timer.
- **A spin lock held while sleeping:** the thread sleeps with `ahci::DISK` locked. Another thread that wants the disk
  spins on the lock until the command is done – correct, but wasteful; a lock that puts the waiter to sleep (the
  reference kernel's `sync::Mutex`) would be better.

## Experiments
1. Make the cache smaller than the FAT (`CACHE_SECTORS = 512`) and read `/big.bin` twice. How many commands now, and
   why does least-recently-used do badly when a file is read from start to end?
2. Count how long `wait_for` sleeps against how long `wait_until` spun: read 2048 sectors with `time::uptime_ms`
   around it, with and without MSI. Is the interrupt slower for a single command?
3. Let `FatFs::read_file` read runs of neighbouring clusters with one call instead of one per cluster. What does
   `/big.bin` cost then?
4. Send the MSI to CPU 3 instead of the boot CPU (its APIC ID from `smp::apic_id`). Does anything change?

That ends part V: the kernel finds its disk, reads and writes files on it, and waits for it the way it waits for
everything else. Chapter 20 starts part VI: code that runs in ring 3, and the first system call.
