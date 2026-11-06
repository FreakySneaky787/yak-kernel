//! A block device: something that stores sectors of 512 bytes, numbered from 0 (the *logical block address*, LBA).
//! The SATA disk of this chapter is one; the file system of the next chapters will only know this trait.

pub const SECTOR_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// Sectors past the end of the device.
    OutOfRange,
    /// A buffer that is empty or not a whole number of sectors.
    BadBuffer,
    /// The device reported an error.
    Device,
    /// The device did not finish in time.
    Timeout,
}

pub trait BlockDevice: Send {
    fn sector_count(&self) -> u64;

    /// Reads `buf.len() / 512` sectors, starting at sector `lba`.
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError>;
}

/// Checks a request for `len` bytes from sector `lba` against a device of `sector_count` sectors.
pub fn check_request(sector_count: u64, lba: u64, len: usize) -> Result<(), BlockError> {
    if len == 0 || len % SECTOR_SIZE != 0 {
        return Err(BlockError::BadBuffer);
    }
    // `checked_add`: a huge `lba` must not wrap around to a small number and pass
    match lba.checked_add((len / SECTOR_SIZE) as u64) {
        Some(end) if end <= sector_count => Ok(()),
        _ => Err(BlockError::OutOfRange),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn requests_are_checked() {
        assert_eq!(check_request(100, 0, 512), Ok(()));
        assert_eq!(check_request(100, 99, 512), Ok(()));
        assert_eq!(check_request(100, 99, 1024), Err(BlockError::OutOfRange));
        assert_eq!(check_request(100, u64::MAX, 512), Err(BlockError::OutOfRange));
        assert_eq!(check_request(100, 0, 0), Err(BlockError::BadBuffer));
        assert_eq!(check_request(100, 0, 511), Err(BlockError::BadBuffer));
    }
}
