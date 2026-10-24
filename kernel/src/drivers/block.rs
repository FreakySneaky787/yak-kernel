//! Block device abstraction: anything that stores fixed-size 512-byte
//! sectors (the AHCI disk, a RAM disk for tests, ...). File systems only use
//! this trait.

pub const SECTOR_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    OutOfRange,
    /// Buffer length is not a multiple of the sector size.
    BadBuffer,
    /// The device reported an error (task file error, timeout, ...).
    Device,
}

pub trait BlockDevice: Send {
    /// Total number of sectors.
    fn sector_count(&self) -> u64;

    /// Reads `buf.len() / 512` sectors starting at `lba`.
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError>;

    /// Writes `buf.len() / 512` sectors starting at `lba`.
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError>;

    /// Makes sure written data reached the medium.
    fn flush(&mut self) -> Result<(), BlockError> {
        Ok(())
    }
}

/// Checks that `[lba, lba + len/512)` is a valid request for `dev`.
pub fn check_request(sector_count: u64, lba: u64, len: usize) -> Result<u64, BlockError> {
    if len == 0 || len % SECTOR_SIZE != 0 {
        return Err(BlockError::BadBuffer);
    }
    let sectors = (len / SECTOR_SIZE) as u64;
    if lba.checked_add(sectors).is_none_or(|end| end > sector_count) {
        return Err(BlockError::OutOfRange);
    }
    Ok(sectors)
}

/// A block device in RAM (used by file system tests).
pub struct RamDisk {
    data: alloc::vec::Vec<u8>,
}

impl RamDisk {
    pub fn new(sectors: usize) -> RamDisk {
        RamDisk {
            data: alloc::vec![0; sectors * SECTOR_SIZE],
        }
    }

    pub fn from_bytes(data: alloc::vec::Vec<u8>) -> RamDisk {
        assert!(data.len() % SECTOR_SIZE == 0);
        RamDisk { data }
    }
}

impl BlockDevice for RamDisk {
    fn sector_count(&self) -> u64 {
        (self.data.len() / SECTOR_SIZE) as u64
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        check_request(self.sector_count(), lba, buf.len())?;
        let start = lba as usize * SECTOR_SIZE;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        check_request(self.sector_count(), lba, buf.len())?;
        let start = lba as usize * SECTOR_SIZE;
        self.data[start..start + buf.len()].copy_from_slice(buf);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn request_checks() {
        assert_eq!(check_request(10, 0, 512), Ok(1));
        assert_eq!(check_request(10, 9, 512), Ok(1));
        assert_eq!(check_request(10, 9, 1024), Err(BlockError::OutOfRange));
        assert_eq!(check_request(10, 0, 100), Err(BlockError::BadBuffer));
        assert_eq!(check_request(10, u64::MAX, 512), Err(BlockError::OutOfRange));
    }

    #[test_case]
    fn ram_disk_roundtrip() {
        let mut d = RamDisk::new(4);
        let data = [0x5Au8; 1024];
        d.write(1, &data).unwrap();
        let mut back = [0u8; 1024];
        d.read(1, &mut back).unwrap();
        assert_eq!(back, data);
        assert_eq!(d.read(4, &mut [0; 512]), Err(BlockError::OutOfRange));
    }
}
