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

/// A write-through cache of single sectors in front of a block device.
///
/// Reads are served from the cache where possible; runs of missing sectors
/// are fetched with one device read each. Writes go to the device first and
/// then update the cache, so the disk is always up to date (a power-off loses
/// nothing). When full, the least recently used eighth is dropped at once
/// (one scan instead of one per miss).
pub struct BlockCache<D: BlockDevice> {
    dev: D,
    capacity: usize,
    entries: alloc::collections::BTreeMap<u64, CacheEntry>,
    clock: u64,
    pub hits: u64,
    pub misses: u64,
}

struct CacheEntry {
    data: alloc::boxed::Box<[u8; SECTOR_SIZE]>,
    used: u64,
}

impl<D: BlockDevice> BlockCache<D> {
    /// A cache holding up to `capacity` sectors.
    pub fn new(dev: D, capacity: usize) -> BlockCache<D> {
        BlockCache {
            dev,
            capacity: capacity.max(8),
            entries: alloc::collections::BTreeMap::new(),
            clock: 0,
            hits: 0,
            misses: 0,
        }
    }

    pub fn cached_sectors(&self) -> usize {
        self.entries.len()
    }

    fn insert(&mut self, lba: u64, data: &[u8]) {
        self.clock += 1;
        if let Some(e) = self.entries.get_mut(&lba) {
            e.data.copy_from_slice(data);
            e.used = self.clock;
            return;
        }
        if self.entries.len() >= self.capacity {
            // drop the oldest eighth
            let mut ages: alloc::vec::Vec<u64> = self.entries.values().map(|e| e.used).collect();
            ages.sort_unstable();
            let cutoff = ages[self.capacity / 8];
            self.entries.retain(|_, e| e.used > cutoff);
        }
        let mut sector = alloc::boxed::Box::new([0u8; SECTOR_SIZE]);
        sector.copy_from_slice(data);
        self.entries.insert(
            lba,
            CacheEntry {
                data: sector,
                used: self.clock,
            },
        );
    }
}

impl<D: BlockDevice> BlockDevice for BlockCache<D> {
    fn sector_count(&self) -> u64 {
        self.dev.sector_count()
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let n = check_request(self.sector_count(), lba, buf.len())? as usize;
        let mut i = 0;
        while i < n {
            self.clock += 1;
            if let Some(e) = self.entries.get_mut(&(lba + i as u64)) {
                e.used = self.clock;
                buf[i * SECTOR_SIZE..(i + 1) * SECTOR_SIZE].copy_from_slice(&e.data[..]);
                self.hits += 1;
                i += 1;
                continue;
            }
            // a run of missing sectors: one device read
            let mut run = 1;
            while i + run < n && !self.entries.contains_key(&(lba + (i + run) as u64)) {
                run += 1;
            }
            let part = &mut buf[i * SECTOR_SIZE..(i + run) * SECTOR_SIZE];
            self.dev.read(lba + i as u64, part)?;
            for j in 0..run {
                let sector: [u8; SECTOR_SIZE] = part[j * SECTOR_SIZE..(j + 1) * SECTOR_SIZE].try_into().unwrap();
                self.insert(lba + (i + j) as u64, &sector);
            }
            self.misses += run as u64;
            i += run;
        }
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        check_request(self.sector_count(), lba, buf.len())?;
        self.dev.write(lba, buf)?;
        for (i, sector) in buf.chunks(SECTOR_SIZE).enumerate() {
            self.insert(lba + i as u64, sector);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.dev.flush()
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

    /// Counts the reads that reach the device.
    struct Counting {
        disk: RamDisk,
        reads: usize,
        sectors_read: usize,
    }

    impl BlockDevice for Counting {
        fn sector_count(&self) -> u64 {
            self.disk.sector_count()
        }
        fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
            self.reads += 1;
            self.sectors_read += buf.len() / SECTOR_SIZE;
            self.disk.read(lba, buf)
        }
        fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
            self.disk.write(lba, buf)
        }
    }

    #[test_case]
    fn cache_serves_repeated_reads_and_writes_through() {
        let mut data = RamDisk::new(64);
        for lba in 0..64u64 {
            data.write(lba, &[lba as u8; 512]).unwrap();
        }
        let mut c = BlockCache::new(
            Counting {
                disk: data,
                reads: 0,
                sectors_read: 0,
            },
            16,
        );
        let mut buf = [0u8; 4 * 512];
        c.read(10, &mut buf).unwrap();
        assert_eq!(
            (c.dev.reads, c.dev.sectors_read),
            (1, 4),
            "one device read for a run of misses"
        );
        c.read(10, &mut buf).unwrap();
        assert_eq!(c.dev.reads, 1, "second read must come from the cache");
        assert_eq!((c.hits, c.misses), (4, 4));
        assert!(
            buf.chunks(512)
                .enumerate()
                .all(|(i, s)| s.iter().all(|&b| b == 10 + i as u8))
        );
        // partly cached: sectors 12..16 -> 12, 13 cached, 14, 15 fetched in one read
        c.read(12, &mut buf).unwrap();
        assert_eq!((c.dev.reads, c.dev.sectors_read), (2, 6));
        // writes reach the disk and the cache
        c.write(11, &[0xEE; 512]).unwrap();
        let mut one = [0u8; 512];
        c.dev.disk.read(11, &mut one).unwrap();
        assert_eq!(one, [0xEE; 512], "write-through");
        c.read(11, &mut one).unwrap();
        assert_eq!((one, c.dev.reads), ([0xEE; 512], 2));
        // filling it beyond its capacity evicts, but data stays correct
        for lba in 20..60u64 {
            c.read(lba, &mut one).unwrap();
            assert_eq!(one, [lba as u8; 512]);
        }
        assert!(c.cached_sectors() <= 16);
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
