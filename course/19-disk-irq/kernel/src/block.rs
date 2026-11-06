//! A block device: something that stores sectors of 512 bytes, numbered from 0 (the *logical block address*, LBA).
//! The SATA disk of this chapter is one; the file system of the next chapters will only know this trait.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;

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

    /// Writes `buf.len() / 512` sectors, starting at sector `lba` (chapter 18).
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError>;

    /// Returns when everything written so far is on the medium, not only in a cache of the device.
    fn flush(&mut self) -> Result<(), BlockError>;
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

/// A write-through cache of single sectors in front of a block device (chapter 19).
///
/// A read takes what the cache has and fetches each run of missing sectors with one read of the device. A write
/// goes to the device *first* and then into the cache - so the disk is never behind the cache, and nothing is lost
/// when the machine stops. When the cache is full, the sector that was used longest ago makes room ("least
/// recently used").
pub struct BlockCache<D: BlockDevice> {
    device: D,
    capacity: usize,
    sectors: BTreeMap<u64, Cached>,
    /// Counts up with every use: the `used` of a sector says when it was used last.
    clock: u64,
    pub hits: u64,
    pub misses: u64,
}

struct Cached {
    data: Box<[u8; SECTOR_SIZE]>,
    used: u64,
}

impl<D: BlockDevice> BlockCache<D> {
    pub fn new(device: D, capacity: usize) -> BlockCache<D> {
        BlockCache {
            device,
            capacity,
            sectors: BTreeMap::new(),
            clock: 0,
            hits: 0,
            misses: 0,
        }
    }

    pub fn cached(&self) -> usize {
        self.sectors.len()
    }

    fn keep(&mut self, lba: u64, data: &[u8]) {
        self.clock += 1;
        if let Some(sector) = self.sectors.get_mut(&lba) {
            sector.data.copy_from_slice(data);
            sector.used = self.clock;
            return;
        }
        if self.sectors.len() == self.capacity {
            // one look at every sector: simple, and cheap next to a disk command
            let oldest = *self.sectors.iter().min_by_key(|(_, s)| s.used).expect("full").0;
            self.sectors.remove(&oldest);
        }
        let mut copy = Box::new([0u8; SECTOR_SIZE]);
        copy.copy_from_slice(data);
        self.sectors.insert(
            lba,
            Cached {
                data: copy,
                used: self.clock,
            },
        );
    }
}

impl<D: BlockDevice> BlockDevice for BlockCache<D> {
    fn sector_count(&self) -> u64 {
        self.device.sector_count()
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        check_request(self.sector_count(), lba, buf.len())?;
        let count = buf.len() / SECTOR_SIZE;
        let mut i = 0;
        while i < count {
            if let Some(sector) = self.sectors.get_mut(&(lba + i as u64)) {
                self.clock += 1;
                sector.used = self.clock;
                buf[i * SECTOR_SIZE..(i + 1) * SECTOR_SIZE].copy_from_slice(&sector.data[..]);
                self.hits += 1;
                i += 1;
                continue;
            }
            // a run of sectors the cache does not have: one read of the device
            let mut run = 1;
            while i + run < count && !self.sectors.contains_key(&(lba + (i + run) as u64)) {
                run += 1;
            }
            let part = &mut buf[i * SECTOR_SIZE..(i + run) * SECTOR_SIZE];
            self.device.read(lba + i as u64, part)?;
            for (j, sector) in part.chunks(SECTOR_SIZE).enumerate() {
                let sector: [u8; SECTOR_SIZE] = sector.try_into().unwrap();
                self.keep(lba + (i + j) as u64, &sector);
            }
            self.misses += run as u64;
            i += run;
        }
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        check_request(self.sector_count(), lba, buf.len())?;
        self.device.write(lba, buf)?; // the disk first: if this fails, the cache still has the old data
        for (i, sector) in buf.chunks(SECTOR_SIZE).enumerate() {
            self.keep(lba + i as u64, sector);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.device.flush()
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

    /// Sectors in RAM that count how often the cache asks for them.
    struct Counting {
        data: alloc::vec::Vec<u8>,
        reads: usize,
        writes: usize,
    }

    impl Counting {
        fn new(sectors: usize) -> Counting {
            let data = (0..sectors * SECTOR_SIZE).map(|i| (i / SECTOR_SIZE) as u8).collect();
            Counting {
                data,
                reads: 0,
                writes: 0,
            }
        }
    }

    impl BlockDevice for Counting {
        fn sector_count(&self) -> u64 {
            (self.data.len() / SECTOR_SIZE) as u64
        }

        fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
            self.reads += 1;
            let start = lba as usize * SECTOR_SIZE;
            buf.copy_from_slice(&self.data[start..start + buf.len()]);
            Ok(())
        }

        fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
            self.writes += 1;
            let start = lba as usize * SECTOR_SIZE;
            self.data[start..start + buf.len()].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> Result<(), BlockError> {
            Ok(())
        }
    }

    #[test_case]
    fn the_cache_reads_a_sector_once() {
        let mut cache = BlockCache::new(Counting::new(16), 8);
        let mut buf = [0u8; 2 * SECTOR_SIZE];
        cache.read(3, &mut buf).unwrap();
        assert_eq!((buf[0], buf[SECTOR_SIZE]), (3, 4));
        cache.read(3, &mut buf).unwrap();
        cache.read(4, &mut buf[..SECTOR_SIZE]).unwrap();
        assert_eq!(cache.device.reads, 1, "one run of two sectors, then only hits");
        assert_eq!((cache.hits, cache.misses), (3, 2));
        // sectors 2..6: 2 and 5 are missing, 3 and 4 are there - two runs, two reads
        let mut four = [0u8; 4 * SECTOR_SIZE];
        cache.read(2, &mut four).unwrap();
        assert_eq!(cache.device.reads, 3);
        assert_eq!(
            four.chunks(SECTOR_SIZE).map(|s| s[0]).collect::<alloc::vec::Vec<_>>(),
            [2, 3, 4, 5]
        );
    }

    #[test_case]
    fn writes_go_through_to_the_device() {
        let mut cache = BlockCache::new(Counting::new(16), 8);
        cache.write(7, &[0xAB; SECTOR_SIZE]).unwrap();
        assert_eq!(cache.device.data[7 * SECTOR_SIZE], 0xAB, "on the device at once");
        let mut buf = [0u8; SECTOR_SIZE];
        cache.read(7, &mut buf).unwrap();
        assert_eq!((buf[0], cache.device.reads), (0xAB, 0), "and in the cache");
    }

    #[test_case]
    fn the_least_recently_used_sector_goes_first() {
        let mut cache = BlockCache::new(Counting::new(16), 4);
        let mut buf = [0u8; SECTOR_SIZE];
        for lba in 0..4 {
            cache.read(lba, &mut buf).unwrap();
        }
        cache.read(0, &mut buf).unwrap(); // 0 is used again: now 1 is the oldest
        cache.read(9, &mut buf).unwrap(); // the cache is full: 1 goes
        assert_eq!(cache.cached(), 4);
        let reads = cache.device.reads;
        cache.read(0, &mut buf).unwrap();
        assert_eq!(cache.device.reads, reads, "0 is still there");
        cache.read(1, &mut buf).unwrap();
        assert_eq!(cache.device.reads, reads + 1, "1 was dropped");
    }
}
