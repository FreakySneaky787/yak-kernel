# 16 – PCI and a SATA disk

**Goal:** find the devices of the machine on the PCI bus, and read sectors from a SATA disk through its AHCI
controller – the first device that moves data into RAM by itself.
**You end up with:** a list of every PCI device at boot, a driver that asks the disk who it is and reads any
sector by DMA, and two shell commands, `disk` and `read`. The start of part V, storage.
**Code:** [`course/16-ahci/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/16-ahci)
(chapter 15 plus `pci.rs`, `block.rs`, `ahci.rs`, 32-bit ports in `port.rs`, a test disk from `mkdisk.py`)
**What changed:** `diff -ru course/15-scheduler course/16-ahci` (in the repository) shows every line this chapter adds.

## Background: devices that find themselves, and DMA
The serial port, the PIC and the keyboard sit at fixed I/O ports that every PC has had since the 1980s. Newer
devices do not: a machine has whatever cards are plugged in, so the kernel has to *ask*. That is what **PCI** is
for. Every device has a 256-byte *configuration space* saying what it is (vendor, device, class) and where its
registers are: the **BARs**, base address registers, hold physical addresses at which the device answers like
memory.

A disk is an array of **sectors** of 512 bytes, numbered from 0 – the *LBA*, logical block address. The SATA
controller of nearly every PC since 2005 speaks **AHCI**. It does not take data byte by byte through a port, as the
serial port does. We write a command into RAM and set a bit; the controller reads the command, fetches the sectors
from the disk and writes them into RAM itself – **DMA**, direct memory access. The CPU only waits until the
controller says "done".

That has two consequences. The controller knows nothing about our page tables: every address we give it is
*physical*. And it writes into RAM behind the back of the compiler, which must not assume that memory stays as it
left it.

## Step 1: the configuration space
Two I/O ports reach the configuration space: an address goes to 0xCF8, then the data comes from 0xCFC. Both are 32
bits wide, so `port.rs` gets `outl` and `inl` next to `outb` and `inb`:

```rust,file=kernel/src/port.rs
/// Reads 32 bits from an I/O port.
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags))
    };
    value
}
```

A device's place is bus, device and function; together with the offset of a double word they make the address:

```rust,file=kernel/src/pci.rs
    fn address(self, offset: u8) -> u32 {
        1 << 31
            | (self.bus as u32) << 16
            | (self.device as u32) << 11
            | (self.function as u32) << 8
            | (offset & 0xFC) as u32
    }
```

Reading is two port accesses – and with four CPUs since chapter 14, two CPUs could interleave theirs: CPU 1 writes
its address, CPU 2 writes its own, CPU 1 reads CPU 2's data. So a lock holds the pair together:

```rust,file=kernel/src/pci.rs
    pub fn read(self, offset: u8) -> u32 {
        let _lock = CONFIG.lock();
        unsafe {
            outl(CONFIG_ADDRESS, self.address(offset));
            inl(CONFIG_DATA)
        }
    }
```

## Step 2: finding every device
Where nothing is, nobody answers, and the read returns all ones – vendor 0xFFFF. So `enumerate` tries every place.
A device whose header type has bit 7 set has more than one function (QEMU's 00:1f is one: ISA bridge, SATA
controller and SMBus controller in one chip):

```rust,file=kernel/src/pci.rs
pub fn enumerate() -> Vec<Device> {
    let mut devices = Vec::new();
    for bus in 0..=255 {
        for device in 0..32 {
            let Some(first) = probe(Location {
                bus,
                device,
                function: 0,
            }) else {
                continue;
            };
            devices.push(first);
            let header_type = (first.location.read(HEADER_TYPE) >> 16) as u8;
            if header_type & 0x80 != 0 {
                devices.extend((1..8).filter_map(|function| probe(Location { bus, device, function })));
            }
        }
    }
    devices
}
```

8192 places, most of them empty – crude, but in QEMU it took 10 ms in three runs (one tick of our clock, which
cannot measure finer). (Real kernels follow the
bridges from bus 0 instead, or read the PCI Express area that the ACPI table MCFG describes.)

The AHCI controller is the device with class 0x01 (mass storage), subclass 0x06 (SATA), programming interface 0x01
(AHCI). Its registers are at the physical address in BAR5. Before it may use them – and before it may access RAM
itself – the command register needs two bits, *memory space* and *bus master*; `enable_dma` sets them.

## Step 3: the controller and its ports
`ahci::init` finds the controller, maps its registers – uncached, like the APIC's in chapter 12 – and switches it to
AHCI mode:

```rust,file=kernel/src/ahci.rs
    let device = pci::find(0x01, 0x06, 0x01).ok_or("no AHCI controller")?; // mass storage, SATA, AHCI
    device.enable_dma();
    let bar = device.memory_bar(5).ok_or("AHCI's BAR5 is not a memory BAR")?;
    // 0x100 bytes for the HBA, 0x80 for each of up to 32 ports: two pages
    let hba = paging::map_device(PhysAddr(bar)).map_err(|_| "cannot map the AHCI registers")?;
    paging::map_device(PhysAddr(bar + PAGE_SIZE)).map_err(|_| "cannot map the AHCI registers")?;
    write32(hba + HBA_GHC, read32(hba + HBA_GHC) | GHC_AHCI_ENABLE);
```

Then it looks at every port the controller has (register PI, "ports implemented"). A port is in use when its SATA
status says a device is there and talking (DET = 3); its *signature* says what kind: 0x101 is a disk, 0xEB140101 an
ATAPI device – a CD drive. QEMU's `-cdrom`, our boot disk, is one.

A disk port gets one zeroed frame for its three structures – the command list at offset 0, the area where the disk
posts its answers at 1024, the command table at 2048 – and a second frame for the data. `Disk::start` stops the
port (the firmware may have left it running with its own command list), writes the *physical* addresses of our
structures into its registers, starts it again and sends the first command: IDENTIFY DEVICE, which answers with 512
bytes about the disk – among them its model name (words 27–46) and its size in sectors (words 100–103).

## Step 4: one command
A command is three things in RAM: a *header* in slot 0 of the command list (how long the command is, how many
buffers, where the table is), the *command FIS* in the table (the ATA command, the LBA in six bytes, the number of
sectors) and one *PRD*, which says "the data goes to this physical address, this many bytes". Then:

```rust,file=kernel/src/ahci.rs
        // Everything above must be in memory before the controller is told to fetch it
        fence(Ordering::SeqCst);
        self.set_reg(PORT_IS, u32::MAX);
        self.set_reg(PORT_CI, 1);
        let failed = || self.reg(PORT_IS) & IS_TASK_FILE_ERROR != 0;
        wait_until(|| self.reg(PORT_CI) & 1 == 0 || failed())?;
        // ... and what the controller wrote must not be read before it said "done"
        fence(Ordering::SeqCst);
        if failed() || self.reg(PORT_TFD) & TFD_ERROR != 0 {
            self.set_reg(PORT_IS, u32::MAX);
            return Err(BlockError::Device);
        }
        Ok(())
    }
```

Setting bit 0 of CI ("command issue") hands slot 0 to the controller; it clears the bit when the command is done.
The two fences are for the compiler: it knows nothing about the controller, and without them it may move a
write to the command table after the write to CI, or a read of the data before the wait. x86 itself keeps the
order of these accesses, but the fence costs nothing next to a disk access.

`wait_until` polls – the CPU spins until the bit clears, for at most a second (`time::uptime_ms`, so the timer
must run). In QEMU a command takes microseconds; chapter 19 lets the controller send an interrupt instead, so that
the CPU can run other threads meanwhile.

## Step 5: reading sectors
The data frame is the *bounce buffer*: the controller needs a physically contiguous buffer, and a buffer on the
heap is contiguous only within a page. So one command moves at most 8 sectors, and a longer read is several
commands, each copied out of the bounce buffer:

```rust,file=kernel/src/ahci.rs
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        block::check_request(self.sectors, lba, buf.len())?;
        for (i, chunk) in buf.chunks_mut(SECTORS_PER_COMMAND * SECTOR_SIZE).enumerate() {
            let first = lba + (i * SECTORS_PER_COMMAND) as u64;
            self.command(ATA_READ_DMA_EXT, first, (chunk.len() / SECTOR_SIZE) as u16, chunk.len())?;
            chunk.copy_from_slice(self.bounce(chunk.len()));
        }
        Ok(())
    }
```

`read` belongs to the trait `BlockDevice` in `block.rs`: "something with sectors". The file system of the next
chapter will only know the trait, so its tests can use a disk in RAM. `check_request` refuses a buffer that is not
whole sectors, and sectors past the end – with `checked_add`, so that a huge LBA cannot wrap around to a small one.

## Step 6: a disk for QEMU, and wiring it in
`mkdisk.py` makes the test disk: 1 MiB, 2048 sectors, and every sector starts with its own number – "YAK test
disk, sector 5" – and is filled up with the byte `n % 251`, so a test can tell every sector from its neighbours at
both ends. `run-qemu.sh` makes it fresh before every start and gives it to QEMU as a SATA disk on port 0:

```sh,file=run-qemu.sh
python3 -I mkdisk.py build/disk.img
```

```sh,file=run-qemu.sh
      -drive file=build/disk.img,format=raw,if=none,id=disk -device ide-hd,drive=disk,bus=ide.0)
```

(`ide-hd` and `ide.0` are QEMU's names for "a hard disk on the first port of the q35's controller" – which is an
AHCI controller, whatever the name says.)

`main.rs` declares the new modules (`mod ahci;`, `mod block;`, `mod pci;`), and `kmain` calls `show_disk()` after
`show_scheduler()`: it prints every PCI device, calls `ahci::init()` and reads sector 0. The test kernel calls
`ahci::init()` before the tests, so that they find the disk in `ahci::DISK`:

```rust,file=kernel/src/main.rs
    ahci::init().expect("the AHCI controller and its disk");
```

The shell gets two commands: `disk` (model and size) and `read <sector>`, which prints the first 64 bytes in hex and
as text:

```rust,file=kernel/src/shell.rs
fn read(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let [lba] = args else {
        return Err(String::from("usage: read <sector>"));
    };
    let lba: u64 = lba.parse().map_err(|_| alloc::format!("not a number: {}", lba))?;
    let mut sector = [0u8; SECTOR_SIZE];
    let mut guard = ahci::DISK.lock();
    let disk = guard.as_mut().ok_or("no SATA disk")?;
    disk.read(lba, &mut sector)
        .map_err(|e| alloc::format!("sector {}: {:?}", lba, e))?;
    drop(guard);
    hex_dump(&sector[..64], out);
    Ok(())
}
```

## The tests
`make test` now runs 111 tests in the kernel; these are new:
- `the_q35_host_bridge_is_device_zero`, `the_sata_controller_is_found` (vendor 8086, device 2922 at 00:1f.2, BAR5 a
  page-aligned memory address), `nothing_answers_where_there_is_no_device`, `dma_is_on_after_init`;
- `the_disk_says_who_it_is` – IDENTIFY reports "QEMU HARDDISK" and 2048 sectors, and the CD drive is on a port too;
- `one_sector_is_read` (the first and the last sector), `a_long_read_takes_several_commands` (20 sectors: 8 + 8 + 4,
  each sector checked at its start and its end), `bad_requests_are_refused`, `init_runs_once`;
- `requests_are_checked` (`block.rs`) and `read_shows_a_sector_in_hex_and_as_text` (the shell).

The boot check types `disk` and `read 5` and looks for their output.

## Run it
```sh
cd course/16-ahci
make run
```
After the scheduler line:

```text
PCI 00:00.0  8086:29c0  host bridge
PCI 00:01.0  1234:1111  display controller
PCI 00:02.0  8086:10d3  network controller
PCI 00:1f.0  8086:2918  ISA bridge
PCI 00:1f.2  8086:2922  SATA controller
PCI 00:1f.3  8086:2930  SMBus controller
AHCI 1.0 at 00:1f.2, ports in use: 0 SataDisk, 2 Atapi
Disk: QEMU HARDDISK on port 0, 2048 sectors (1024 KiB); sector 0 says: YAK test disk, sector 0
```

and in the shell:

```text
yak> read 5
0000  59 41 4b 20 74 65 73 74 20 64 69 73 6b 2c 20 73  |YAK test disk, s|
0010  65 63 74 6f 72 20 35 0a 05 05 05 05 05 05 05 05  |ector 5.........|
0020  05 05 05 05 05 05 05 05 05 05 05 05 05 05 05 05  |................|
0030  05 05 05 05 05 05 05 05 05 05 05 05 05 05 05 05  |................|
```

The network card at 00:02.0 waits for part VII.

## What can go wrong
- **No bus mastering:** the controller may not touch RAM, and the first command never finishes. I cleared the bit on
  purpose: IDENTIFY timed out and the test kernel stopped at `ahci::init`. Leaving the bit out of `enable_dma`, on
  the other hand, changed nothing here – the firmware had set it already (the command register read 0x107 before
  `enable_dma`), because SeaBIOS reads the boot CD through this very controller. On another machine, or with
  another firmware, it may not.
- **A virtual address in a register or a PRD:** the controller reads from that number as a *physical* address –
  somewhere else entirely. Every address handed over is `frame.0`, never `to_virt()`.
- **Initializing a port twice:** the port gets a new command list while the old `Disk` still fills in the old one,
  which the controller no longer reads – it runs stale commands, and the data lands in the wrong buffer. In the
  reference kernel a test did exactly this, and reading sector 0 returned the disk's IDENTIFY data instead
  ([bug #3](../BUGS.md)). `init` refuses a second run, and `init_runs_once` checks it.
- **Holding `ahci::DISK` while waiting:** the lock is a spin lock; another thread that wants the disk spins until the
  command is done. Fine for microseconds in QEMU – chapter 19 replaces the polling.
- **The timer is not running:** `wait_until` measures its timeout with `time::uptime_ms`. With interrupts off, a
  disk that never answers would make it spin for ever.

## Experiments
1. Print `d.vendor` and `d.device` of every device and look them up in the PCI ID database
   ([pci-ids.ucw.cz](https://pci-ids.ucw.cz/)). What is 1234:1111?
2. Attach a second disk (another `-drive` with `bus=ide.1`). On which port does it appear? Make `init` keep all
   disks.
3. Read sector 0 of the boot CD on port 2. Why does it fail? (ATAPI devices take SCSI commands wrapped in an ATA
   PACKET command, not READ DMA EXT.)
4. Time `read` of 2048 sectors with `time::uptime_ms`. How much faster is it with 16 sectors per command (a bounce
   buffer of two contiguous frames)?

Next, chapter 17 turns these numbered sectors into files: FAT32.
