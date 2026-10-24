//! AHCI (Advanced Host Controller Interface) driver for SATA disks.
//!
//! The controller ("HBA") is a PCI device whose registers live in BAR5. It
//! has up to 32 ports; each port with a device gets three DMA structures in
//! physical memory that we allocate:
//! - command list: 32 command headers (we only use slot 0),
//! - received-FIS area: where the device posts its status,
//! - command table: the ATA command (as a "FIS") plus a list of data buffers
//!   (physical region descriptor table, PRDT).
//!
//! Issuing a command: fill the table, set bit 0 in the port's CI register,
//! poll until the bit clears, check the task file for errors. Data goes
//! through a 4 KiB bounce frame, so each command moves at most 8 sectors.

use super::block::{BlockDevice, BlockError, SECTOR_SIZE, check_request};
use super::pci;
use crate::mem::paging::{self, Flags};
use crate::mem::pmm::alloc_frame_zeroed;
use crate::mem::{PAGE_SIZE, PhysAddr};
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

// HBA (global) registers
const HBA_CAP: u64 = 0x00;
const HBA_GHC: u64 = 0x04;
const HBA_PI: u64 = 0x0C;
const HBA_VS: u64 = 0x10;
const GHC_AHCI_ENABLE: u32 = 1 << 31;

// Port registers (port n at 0x100 + n * 0x80)
const PORT_CLB: u64 = 0x00;
const PORT_FB: u64 = 0x08;
const PORT_IS: u64 = 0x10;
const PORT_IE: u64 = 0x14;
const PORT_CMD: u64 = 0x18;
const PORT_TFD: u64 = 0x20;
const PORT_SIG: u64 = 0x24;
const PORT_SSTS: u64 = 0x28;
const PORT_SERR: u64 = 0x30;
const PORT_CI: u64 = 0x38;

const CMD_START: u32 = 1 << 0;
const CMD_FIS_RECEIVE: u32 = 1 << 4;
const CMD_FIS_RUNNING: u32 = 1 << 14;
const CMD_LIST_RUNNING: u32 = 1 << 15;

const TFD_ERR: u32 = 1 << 0;
const TFD_DRQ: u32 = 1 << 3;
const TFD_BSY: u32 = 1 << 7;
const IS_TASK_FILE_ERROR: u32 = 1 << 30;

const SIG_SATA_DISK: u32 = 0x0000_0101;
const SIG_ATAPI: u32 = 0xEB14_0101;

// ATA commands
const ATA_IDENTIFY: u8 = 0xEC;
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH_CACHE_EXT: u8 = 0xEA;

const FIS_TYPE_REG_H2D: u8 = 0x27;
const MAX_SECTORS_PER_COMMAND: usize = PAGE_SIZE as usize / SECTOR_SIZE;
const TIMEOUT_SPINS: u32 = 10_000_000;

/// One port with an attached SATA disk.
pub struct AhciDisk {
    port_base: u64,
    port: u8,
    command_list: PhysAddr,
    command_table: PhysAddr,
    bounce: PhysAddr,
    sectors: u64,
    model: String,
}

// The raw addresses are only used by the owner of the `AhciDisk`.
unsafe impl Send for AhciDisk {}

fn mmio_read(addr: u64) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

fn mmio_write(addr: u64, value: u32) {
    unsafe { write_volatile(addr as *mut u32, value) }
}

fn wait_until(mut condition: impl FnMut() -> bool) -> Result<(), BlockError> {
    for _ in 0..TIMEOUT_SPINS {
        if condition() {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(BlockError::Device)
}

/// Port types found during probing (for `disk` command output).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    SataDisk,
    Atapi,
    Other(u32),
}

#[derive(Debug, Clone)]
pub struct PortInfo {
    pub port: u8,
    pub kind: PortKind,
}

static PROBED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Finds the AHCI controller, enables it and returns the disks it has, plus
/// a list of all occupied ports.
///
/// May only run **once**: initializing a port again would point its command
/// list at new memory behind the back of the existing `AhciDisk` (bug #3).
pub fn probe() -> Result<(Vec<AhciDisk>, Vec<PortInfo>), &'static str> {
    if PROBED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return Err("AHCI controller was already probed");
    }
    let dev = pci::find(0x01, 0x06, 0x01).ok_or("no AHCI controller found")?;
    dev.enable_mmio_and_dma();
    let abar = dev.memory_bar(5).ok_or("AHCI BAR5 is not a memory BAR")?;
    let hba = paging::ensure_hhdm_mapped(PhysAddr(abar), 0x1100, Flags::MMIO)
        .map_err(|_| "cannot map AHCI registers")?
        .as_u64();

    mmio_write(hba + HBA_GHC, mmio_read(hba + HBA_GHC) | GHC_AHCI_ENABLE);
    let implemented = mmio_read(hba + HBA_PI);

    let mut disks = Vec::new();
    let mut ports = Vec::new();
    for port in 0..32u8 {
        if implemented & (1 << port) == 0 {
            continue;
        }
        let port_base = hba + 0x100 + port as u64 * 0x80;
        let ssts = mmio_read(port_base + PORT_SSTS);
        // DET = 3: device present and communication established
        if ssts & 0xF != 3 {
            continue;
        }
        let kind = match mmio_read(port_base + PORT_SIG) {
            SIG_SATA_DISK => PortKind::SataDisk,
            SIG_ATAPI => PortKind::Atapi,
            other => PortKind::Other(other),
        };
        ports.push(PortInfo { port, kind });
        if kind == PortKind::SataDisk {
            disks.push(AhciDisk::init(port_base, port).map_err(|_| "AHCI port initialization failed")?);
        }
    }
    Ok((disks, ports))
}

/// Controller version and capabilities (for diagnostics).
pub fn controller_version(hba: u64) -> (u32, u32) {
    (mmio_read(hba + HBA_VS), mmio_read(hba + HBA_CAP))
}

impl AhciDisk {
    fn reg(&self, offset: u64) -> u32 {
        mmio_read(self.port_base + offset)
    }

    fn set_reg(&self, offset: u64, value: u32) {
        mmio_write(self.port_base + offset, value)
    }

    fn init(port_base: u64, port: u8) -> Result<AhciDisk, BlockError> {
        let alloc = || alloc_frame_zeroed().ok_or(BlockError::Device);
        // Command list (1 KiB) and received FIS (256 bytes) share one frame
        let structures = alloc()?;
        let command_table = alloc()?;
        let bounce = alloc()?;
        let mut disk = AhciDisk {
            port_base,
            port,
            command_list: structures,
            command_table,
            bounce,
            sectors: 0,
            model: String::new(),
        };

        disk.stop()?;
        let clb = structures.as_u64();
        let fb = clb + 1024;
        disk.set_reg(PORT_CLB, clb as u32);
        disk.set_reg(PORT_CLB + 4, (clb >> 32) as u32);
        disk.set_reg(PORT_FB, fb as u32);
        disk.set_reg(PORT_FB + 4, (fb >> 32) as u32);
        disk.set_reg(PORT_SERR, u32::MAX); // clear errors (write 1 to clear)
        disk.set_reg(PORT_IS, u32::MAX);
        disk.set_reg(PORT_IE, 0); // we poll, no interrupts
        disk.start()?;

        disk.identify()?;
        Ok(disk)
    }

    fn stop(&self) -> Result<(), BlockError> {
        self.set_reg(PORT_CMD, self.reg(PORT_CMD) & !(CMD_START | CMD_FIS_RECEIVE));
        wait_until(|| self.reg(PORT_CMD) & (CMD_LIST_RUNNING | CMD_FIS_RUNNING) == 0)
    }

    fn start(&self) -> Result<(), BlockError> {
        wait_until(|| self.reg(PORT_CMD) & CMD_LIST_RUNNING == 0)?;
        self.set_reg(PORT_CMD, self.reg(PORT_CMD) | CMD_FIS_RECEIVE);
        self.set_reg(PORT_CMD, self.reg(PORT_CMD) | CMD_START);
        Ok(())
    }

    /// Issues one ATA command in slot 0. `data_len` bytes are transferred
    /// through the bounce frame (0 = no data).
    fn command(
        &self,
        ata_command: u8,
        lba: u64,
        sector_count: u16,
        data_len: usize,
        write: bool,
    ) -> Result<(), BlockError> {
        assert!(data_len <= PAGE_SIZE as usize);
        wait_until(|| self.reg(PORT_TFD) & (TFD_BSY | TFD_DRQ) == 0)?;

        // --- command header 0 (32 bytes at the start of the command list) ---
        let header = self.command_list.to_virt().as_mut_ptr::<u32>();
        let prdt_entries = if data_len > 0 { 1u32 } else { 0 };
        let flags = 5 /* FIS length in dwords */ | if write { 1 << 6 } else { 0 };
        let table = self.command_table.as_u64();
        unsafe {
            write_volatile(header, flags | prdt_entries << 16);
            write_volatile(header.add(1), 0); // bytes transferred (set by the HBA)
            write_volatile(header.add(2), table as u32);
            write_volatile(header.add(3), (table >> 32) as u32);
        }

        // --- command table: FIS at 0x00, PRDT at 0x80 ---
        let t = self.command_table.to_virt().as_mut_ptr::<u8>();
        unsafe {
            core::ptr::write_bytes(t, 0, 0x80 + 16);
            let fis = [
                FIS_TYPE_REG_H2D,
                0x80, // "command" bit: this FIS carries a new command
                ata_command,
                0, // features (low)
                lba as u8,
                (lba >> 8) as u8,
                (lba >> 16) as u8,
                1 << 6, // device: LBA mode
                (lba >> 24) as u8,
                (lba >> 32) as u8,
                (lba >> 40) as u8,
                0, // features (high)
                sector_count as u8,
                (sector_count >> 8) as u8,
                0, // isochronous command completion
                0, // control
            ];
            core::ptr::copy_nonoverlapping(fis.as_ptr(), t, fis.len());
            if data_len > 0 {
                let prd = t.add(0x80) as *mut u32;
                let buf = self.bounce.as_u64();
                write_volatile(prd, buf as u32);
                write_volatile(prd.add(1), (buf >> 32) as u32);
                write_volatile(prd.add(3), (data_len as u32 - 1) & 0x3F_FFFF); // byte count - 1
            }
        }

        // --- issue and wait ---
        self.set_reg(PORT_IS, u32::MAX);
        self.set_reg(PORT_CI, 1);
        wait_until(|| self.reg(PORT_CI) & 1 == 0 || self.reg(PORT_IS) & IS_TASK_FILE_ERROR != 0)?;
        if self.reg(PORT_IS) & IS_TASK_FILE_ERROR != 0 || self.reg(PORT_TFD) & TFD_ERR != 0 {
            self.set_reg(PORT_IS, u32::MAX);
            return Err(BlockError::Device);
        }
        Ok(())
    }

    fn bounce_slice(&self, len: usize) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.bounce.to_virt().as_mut_ptr::<u8>(), len) }
    }

    /// IDENTIFY DEVICE: model name and capacity.
    fn identify(&mut self) -> Result<(), BlockError> {
        self.command(ATA_IDENTIFY, 0, 0, SECTOR_SIZE, false)?;
        let data = self.bounce_slice(SECTOR_SIZE);
        let word = |i: usize| u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]);
        // Model: words 27-46, two ASCII characters per word, high byte first
        let mut model = String::new();
        for i in 27..47 {
            let w = word(i);
            model.push((w >> 8) as u8 as char);
            model.push(w as u8 as char);
        }
        // Words 100-103: number of sectors for 48-bit LBA
        let sectors = (0..4).fold(0u64, |acc, i| acc | (word(100 + i) as u64) << (16 * i));
        self.model = String::from(model.trim());
        self.sectors = sectors;
        Ok(())
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn port(&self) -> u8 {
        self.port
    }
}

impl BlockDevice for AhciDisk {
    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        check_request(self.sectors, lba, buf.len())?;
        for (i, chunk) in buf.chunks_mut(MAX_SECTORS_PER_COMMAND * SECTOR_SIZE).enumerate() {
            let chunk_lba = lba + (i * MAX_SECTORS_PER_COMMAND) as u64;
            let count = chunk.len() / SECTOR_SIZE;
            self.command(ATA_READ_DMA_EXT, chunk_lba, count as u16, chunk.len(), false)?;
            chunk.copy_from_slice(self.bounce_slice(chunk.len()));
        }
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        check_request(self.sectors, lba, buf.len())?;
        for (i, chunk) in buf.chunks(MAX_SECTORS_PER_COMMAND * SECTOR_SIZE).enumerate() {
            let chunk_lba = lba + (i * MAX_SECTORS_PER_COMMAND) as u64;
            let count = chunk.len() / SECTOR_SIZE;
            self.bounce_slice(chunk.len()).copy_from_slice(chunk);
            self.command(ATA_WRITE_DMA_EXT, chunk_lba, count as u16, chunk.len(), true)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.command(ATA_FLUSH_CACHE_EXT, 0, 0, 0, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::DISK;

    /// Runs `f` with the disk found at boot (tests must not probe again).
    fn with_disk<R>(f: impl FnOnce(&mut AhciDisk) -> R) -> R {
        f(DISK.lock().as_mut().expect("no SATA disk attached"))
    }

    #[test_case]
    fn identifies_qemu_disk() {
        let ports = crate::drivers::STORAGE_PORTS.lock();
        assert!(
            ports.iter().any(|p| p.kind == PortKind::Atapi),
            "the boot CD-ROM should show up as ATAPI"
        );
        with_disk(|d| {
            assert!(d.model().contains("QEMU"), "model: {}", d.model());
            assert_eq!(d.sector_count(), 64 * 1024 * 1024 / 512); // test disks are 64 MiB
        });
    }

    #[test_case]
    fn probing_twice_is_refused() {
        assert!(probe().is_err());
    }

    #[test_case]
    fn reads_fat32_boot_sector() {
        with_disk(|d| {
            let mut sector = [0u8; 512];
            d.read(0, &mut sector).unwrap();
            assert_eq!(&sector[510..512], &[0x55, 0xAA], "boot signature");
            assert_eq!(&sector[82..90], b"FAT32   ");
        });
    }

    #[test_case]
    fn write_read_roundtrip_multi_command() {
        with_disk(|d| {
            // 20 sectors = 3 commands (8 + 8 + 4) near the end of the (scratch) test disk
            let lba = d.sector_count() - 40;
            let mut data = alloc::vec![0u8; 20 * 512];
            for (i, b) in data.iter_mut().enumerate() {
                *b = (i * 7 + i / 512) as u8;
            }
            d.write(lba, &data).unwrap();
            d.flush().unwrap();
            let mut back = alloc::vec![0u8; 20 * 512];
            d.read(lba, &mut back).unwrap();
            assert!(back == data, "data read back differs");
        });
    }

    #[test_case]
    fn rejects_out_of_range() {
        with_disk(|d| {
            let n = d.sector_count();
            assert_eq!(d.read(n, &mut [0; 512]), Err(BlockError::OutOfRange));
            assert_eq!(d.write(n - 1, &[0; 1024]), Err(BlockError::OutOfRange));
        });
    }
}
