//! AHCI: the standard way to talk to a SATA disk (Serial ATA AHCI 1.3.1 Specification).
//!
//! The controller - the *HBA*, host bus adapter - is a PCI device; its registers are memory at the address in
//! BAR5. It has up to 32 *ports*, one disk each. We do not hand it data byte by byte: we write a command into RAM,
//! tell the port where it is, and the controller fetches the command and moves the sectors between disk and RAM
//! itself - DMA. Every port needs, in physical memory:
//! - a *command list*: 32 command headers of 32 bytes (we use slot 0 only),
//! - a *received FIS* area of 256 bytes, where the disk posts its answers,
//! - a *command table*: the ATA command as a "frame information structure" (FIS), and a list of the buffers
//!   (physical region descriptors, PRDs) the data goes to.
//!
//! All three fit into one frame. The data goes through a second frame, the *bounce buffer*: the controller needs
//! physical addresses, and a buffer on the heap is only contiguous within a page. So one command moves at most
//! 8 sectors. This chapter waits for a command by polling; chapter 19 lets the disk send an interrupt.

use crate::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
use crate::mem::{PAGE_SIZE, PhysAddr};
use crate::{paging, pci, pmm, time};
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};
use spin::Mutex;

// Registers of the HBA (section 3.1)
const HBA_GHC: u64 = 0x04;
const HBA_PI: u64 = 0x0C;
const HBA_VS: u64 = 0x10;
/// GHC.AE: "AHCI enable" - without it the controller may behave like an old IDE controller.
const GHC_AHCI_ENABLE: u32 = 1 << 31;

// Registers of a port, port n at 0x100 + n * 0x80 (section 3.3)
const PORT_CLB: u64 = 0x00; // command list base, low and high half
const PORT_FB: u64 = 0x08; // received FIS base
const PORT_IS: u64 = 0x10; // interrupt status: what happened
const PORT_CMD: u64 = 0x18;
const PORT_TFD: u64 = 0x20; // task file data: the disk's status and error bits
const PORT_SIG: u64 = 0x24; // signature: what kind of device is there
const PORT_SSTS: u64 = 0x28; // SATA status: is a device there at all
const PORT_SERR: u64 = 0x30;
const PORT_CI: u64 = 0x38; // command issue: one bit per slot, cleared by the HBA when the command is done

const CMD_START: u32 = 1 << 0; // ST: process the command list
const CMD_FIS_RECEIVE: u32 = 1 << 4; // FRE: accept FISes from the disk
const CMD_FIS_RUNNING: u32 = 1 << 14; // FR
const CMD_LIST_RUNNING: u32 = 1 << 15; // CR

const TFD_ERROR: u32 = 1 << 0;
const TFD_DRQ: u32 = 1 << 3;
const TFD_BUSY: u32 = 1 << 7;
const IS_TASK_FILE_ERROR: u32 = 1 << 30;

const SIG_SATA_DISK: u32 = 0x0000_0101;
const SIG_ATAPI: u32 = 0xEB14_0101; // a CD drive - QEMU's -cdrom, our boot disk

// ATA commands (ATA/ATAPI Command Set, ACS-3)
const ATA_IDENTIFY: u8 = 0xEC;
const ATA_READ_DMA_EXT: u8 = 0x25;
const FIS_TYPE_REGISTER_H2D: u8 = 0x27; // "register, host to device": a command

// Where things are in the frame of a port
const COMMAND_LIST: u64 = 0; // 1 KiB, must be 1 KiB aligned
const RECEIVED_FIS: u64 = 1024; // 256 bytes, 256 aligned
const COMMAND_TABLE: u64 = 2048; // 128 aligned: the FIS at 0x00, the PRDs at 0x80
const PRD_OFFSET: u64 = 0x80;

const SECTORS_PER_COMMAND: usize = PAGE_SIZE as usize / SECTOR_SIZE;
/// How long a command may take. QEMU needs microseconds; a real disk that spins up, seconds.
const TIMEOUT_MS: u64 = 1000;

fn read32(address: u64) -> u32 {
    unsafe { read_volatile(address as *const u32) }
}

fn write32(address: u64, value: u32) {
    unsafe { write_volatile(address as *mut u32, value) }
}

/// Polls `done` until it is true, or `TIMEOUT_MS` passed (the timer must run: interrupts on).
fn wait_until(mut done: impl FnMut() -> bool) -> Result<(), BlockError> {
    let deadline = time::uptime_ms() + TIMEOUT_MS;
    while !done() {
        if time::uptime_ms() > deadline {
            return Err(BlockError::Timeout);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    SataDisk,
    Atapi,
    Other(u32),
}

/// What sits on a port.
#[derive(Debug, Clone, Copy)]
pub struct Port {
    pub number: u8,
    pub kind: PortKind,
}

/// A SATA disk on one port.
pub struct Disk {
    registers: u64,
    port: u8,
    frame: PhysAddr,
    bounce: PhysAddr,
    sectors: u64,
    model: String,
}

/// The controller as `init` found it.
pub struct Controller {
    pub pci: pci::Device,
    pub version: (u16, u16),
    pub ports: Vec<Port>,
}

/// The first SATA disk, found by `init`.
pub static DISK: Mutex<Option<Disk>> = Mutex::new(None);
static CONTROLLER: Mutex<Option<Controller>> = Mutex::new(None);

/// Finds the AHCI controller and its disks; the first disk becomes `DISK`. Once only: a second run would give the
/// ports new command lists behind the back of the `Disk` that uses the old ones.
pub fn init() -> Result<(), &'static str> {
    let mut controller = CONTROLLER.lock();
    if controller.is_some() {
        return Err("the AHCI controller is set up already");
    }
    let device = pci::find(0x01, 0x06, 0x01).ok_or("no AHCI controller")?; // mass storage, SATA, AHCI
    device.enable_dma();
    let bar = device.memory_bar(5).ok_or("AHCI's BAR5 is not a memory BAR")?;
    // 0x100 bytes for the HBA, 0x80 for each of up to 32 ports: two pages
    let hba = paging::map_device(PhysAddr(bar)).map_err(|_| "cannot map the AHCI registers")?;
    paging::map_device(PhysAddr(bar + PAGE_SIZE)).map_err(|_| "cannot map the AHCI registers")?;
    write32(hba + HBA_GHC, read32(hba + HBA_GHC) | GHC_AHCI_ENABLE);

    let implemented = read32(hba + HBA_PI);
    let mut ports = Vec::new();
    let mut disks = Vec::new();
    for number in (0..32).filter(|n| implemented & (1 << n) != 0) {
        let registers = hba + 0x100 + number as u64 * 0x80;
        // SSTS.DET = 3: a device is there and talks to the controller
        if read32(registers + PORT_SSTS) & 0xF != 3 {
            continue;
        }
        let kind = match read32(registers + PORT_SIG) {
            SIG_SATA_DISK => PortKind::SataDisk,
            SIG_ATAPI => PortKind::Atapi,
            other => PortKind::Other(other),
        };
        ports.push(Port { number, kind });
        if kind == PortKind::SataDisk {
            disks.push(Disk::start(registers, number).map_err(|_| "a SATA port did not start")?);
        }
    }
    let version = read32(hba + HBA_VS);
    *controller = Some(Controller {
        pci: device,
        version: ((version >> 16) as u16, version as u16),
        ports,
    });
    *DISK.lock() = disks.into_iter().next();
    Ok(())
}

/// Runs `f` with the controller `init` found.
pub fn with_controller<R>(f: impl FnOnce(&Controller) -> R) -> Option<R> {
    CONTROLLER.lock().as_ref().map(f)
}

impl Disk {
    fn reg(&self, offset: u64) -> u32 {
        read32(self.registers + offset)
    }

    fn set_reg(&self, offset: u64, value: u32) {
        write32(self.registers + offset, value)
    }

    /// Gives the port our command list and FIS area, starts it and asks the disk who it is.
    fn start(registers: u64, port: u8) -> Result<Disk, BlockError> {
        let frame = pmm::alloc_frame_zeroed().ok_or(BlockError::Device)?;
        let bounce = pmm::alloc_frame_zeroed().ok_or(BlockError::Device)?;
        let mut disk = Disk {
            registers,
            port,
            frame,
            bounce,
            sectors: 0,
            model: String::new(),
        };
        // The firmware may have left the port running with its own command list: stop it first (section 10.1.2)
        disk.set_reg(PORT_CMD, disk.reg(PORT_CMD) & !(CMD_START | CMD_FIS_RECEIVE));
        wait_until(|| disk.reg(PORT_CMD) & (CMD_LIST_RUNNING | CMD_FIS_RUNNING) == 0)?;
        let list = frame.0 + COMMAND_LIST;
        let fis = frame.0 + RECEIVED_FIS;
        disk.set_reg(PORT_CLB, list as u32);
        disk.set_reg(PORT_CLB + 4, (list >> 32) as u32);
        disk.set_reg(PORT_FB, fis as u32);
        disk.set_reg(PORT_FB + 4, (fis >> 32) as u32);
        disk.set_reg(PORT_SERR, u32::MAX); // these bits are cleared by writing 1
        disk.set_reg(PORT_IS, u32::MAX);
        disk.set_reg(PORT_CMD, disk.reg(PORT_CMD) | CMD_FIS_RECEIVE);
        disk.set_reg(PORT_CMD, disk.reg(PORT_CMD) | CMD_START);
        disk.identify()?;
        Ok(disk)
    }

    /// Sends one ATA command in slot 0 and waits until the disk is done. `bytes` (at most a page) go through the
    /// bounce buffer; 0 means the command moves no data.
    fn command(&self, ata_command: u8, lba: u64, sectors: u16, bytes: usize) -> Result<(), BlockError> {
        assert!(bytes <= PAGE_SIZE as usize);
        wait_until(|| self.reg(PORT_TFD) & (TFD_BUSY | TFD_DRQ) == 0)?;
        let base = self.frame.to_virt();
        let table = self.frame.0 + COMMAND_TABLE;
        let prds = if bytes > 0 { 1 } else { 0 };
        unsafe {
            // The command header in slot 0: FIS length (5 double words), number of PRDs, the table's address
            let header = (base + COMMAND_LIST) as *mut u32;
            write_volatile(header, 5 | prds << 16);
            write_volatile(header.add(1), 0); // bytes transferred - the HBA counts here
            write_volatile(header.add(2), table as u32);
            write_volatile(header.add(3), (table >> 32) as u32);
            // The command FIS (Serial ATA Revision 3.0, section 10.3.4): a 48-bit LBA in six bytes
            let fis: [u8; 16] = [
                FIS_TYPE_REGISTER_H2D,
                0x80, // C: this FIS carries a command
                ata_command,
                0, // features
                lba as u8,
                (lba >> 8) as u8,
                (lba >> 16) as u8,
                1 << 6, // device register: LBA, not cylinder/head/sector
                (lba >> 24) as u8,
                (lba >> 32) as u8,
                (lba >> 40) as u8,
                0,
                sectors as u8,
                (sectors >> 8) as u8,
                0,
                0,
            ];
            let command_fis = (base + COMMAND_TABLE) as *mut [u8; 16];
            write_volatile(command_fis, fis);
            // One PRD: the bounce buffer, and how many bytes (minus one)
            let prd = (base + COMMAND_TABLE + PRD_OFFSET) as *mut u32;
            write_volatile(prd, self.bounce.0 as u32);
            write_volatile(prd.add(1), (self.bounce.0 >> 32) as u32);
            write_volatile(prd.add(2), 0);
            write_volatile(prd.add(3), (bytes as u32).saturating_sub(1));
        }
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

    fn bounce(&self, bytes: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.bounce.as_mut_ptr::<u8>(), bytes) }
    }

    /// IDENTIFY DEVICE: 256 words about the disk (ACS-3, section 7.12.7).
    fn identify(&mut self) -> Result<(), BlockError> {
        self.command(ATA_IDENTIFY, 0, 0, SECTOR_SIZE)?;
        let data = self.bounce(SECTOR_SIZE);
        let word = |i: usize| u16::from_le_bytes([data[2 * i], data[2 * i + 1]]);
        // words 27-46: the model, 40 ASCII characters - the first of each pair in the high byte
        let model: String = (27..47)
            .flat_map(|i| [(word(i) >> 8) as u8 as char, word(i) as u8 as char])
            .collect();
        // words 100-103: the number of sectors for 48-bit LBA
        self.sectors = (0..4).fold(0, |sum, i| sum | (word(100 + i) as u64) << (16 * i));
        self.model = String::from(model.trim());
        Ok(())
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn port(&self) -> u8 {
        self.port
    }
}

impl BlockDevice for Disk {
    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        block::check_request(self.sectors, lba, buf.len())?;
        for (i, chunk) in buf.chunks_mut(SECTORS_PER_COMMAND * SECTOR_SIZE).enumerate() {
            let first = lba + (i * SECTORS_PER_COMMAND) as u64;
            self.command(ATA_READ_DMA_EXT, first, (chunk.len() / SECTOR_SIZE) as u16, chunk.len())?;
            chunk.copy_from_slice(self.bounce(chunk.len()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `f` with the disk `init` found at boot.
    fn with_disk<R>(f: impl FnOnce(&mut Disk) -> R) -> R {
        f(DISK.lock().as_mut().expect("a SATA disk"))
    }

    #[test_case]
    fn the_disk_says_who_it_is() {
        with_disk(|disk| {
            assert_eq!(disk.model(), "QEMU HARDDISK");
            assert_eq!(disk.sector_count(), 131_072); // mkdisk.sh makes 64 MiB
        });
        let kinds: Vec<PortKind> = with_controller(|c| c.ports.iter().map(|p| p.kind).collect()).unwrap();
        assert!(
            kinds.contains(&PortKind::SataDisk) && kinds.contains(&PortKind::Atapi),
            "{:?}",
            kinds
        );
    }

    #[test_case]
    fn one_sector_is_read() {
        // sector 0 is the FAT32 boot sector (chapter 17): its type at byte 82, the signature 55 AA at the end
        let mut sector = [0u8; SECTOR_SIZE];
        with_disk(|disk| disk.read(0, &mut sector)).unwrap();
        assert_eq!(&sector[82..90], b"FAT32   ");
        assert_eq!(sector[510..], [0x55, 0xAA]);
        with_disk(|disk| disk.read(131_071, &mut sector)).unwrap();
    }

    #[test_case]
    fn a_long_read_takes_several_commands() {
        // 20 sectors (the FAT): 8 + 8 + 4, three commands through the bounce buffer - the same as 20 reads of one
        let mut data = alloc::vec![0u8; 20 * SECTOR_SIZE];
        with_disk(|disk| disk.read(32, &mut data)).unwrap();
        for (i, expected) in data.chunks(SECTOR_SIZE).enumerate() {
            let mut sector = [0u8; SECTOR_SIZE];
            with_disk(|disk| disk.read(32 + i as u64, &mut sector)).unwrap();
            assert!(sector == expected, "sector {}", 32 + i);
        }
        assert!(data.iter().any(|&b| b != 0), "the FAT is not empty");
    }

    #[test_case]
    fn bad_requests_are_refused() {
        let mut sector = [0u8; SECTOR_SIZE];
        with_disk(|disk| {
            assert_eq!(disk.read(131_072, &mut sector), Err(BlockError::OutOfRange));
            assert_eq!(disk.read(0, &mut sector[..100]), Err(BlockError::BadBuffer));
        });
    }

    #[test_case]
    fn init_runs_once() {
        assert!(init().is_err());
        assert!(DISK.lock().is_some(), "the disk is still there");
    }
}
