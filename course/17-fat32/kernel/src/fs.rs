//! The file system of the kernel: FAT32 on the SATA disk of chapter 16.

use crate::ahci;
use crate::block::{BlockDevice, BlockError};
use crate::fat32::{FatFs, FsError};
use spin::Mutex;

/// The SATA disk as a `BlockDevice` that `FatFs` can own: every access takes the lock of `ahci::DISK`, so the
/// shell's `read` still works next to the file system.
pub struct SystemDisk;

impl BlockDevice for SystemDisk {
    fn sector_count(&self) -> u64 {
        ahci::DISK.lock().as_ref().map_or(0, |disk| disk.sector_count())
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        ahci::DISK.lock().as_mut().ok_or(BlockError::Device)?.read(lba, buf)
    }
}

/// The mounted volume - `None` until `mount` succeeded.
pub static FS: Mutex<Option<FatFs<SystemDisk>>> = Mutex::new(None);

pub fn mount() -> Result<(), FsError> {
    let fs = FatFs::mount(SystemDisk)?;
    *FS.lock() = Some(fs);
    Ok(())
}
