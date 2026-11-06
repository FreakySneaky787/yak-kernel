//! FAT32, read only: files and directories on a disk, in the format of Microsoft's "FAT: General Overview of On-Disk
//! Format" (version 1.03) - which every operating system reads, and which Linux tools write for our tests.
//!
//! A FAT32 volume, from its first sector on:
//! - the *boot sector*: the BIOS parameter block (BPB) says how big everything is;
//! - *reserved sectors*, then the *FAT* (usually twice, the second a copy);
//! - the *data area*, cut into *clusters* numbered from 2. A cluster is a few sectors (here 1).
//!
//! The FAT has one 32-bit entry per cluster (the top 4 bits are reserved): 0 = free, 0x0FFFFFF8 and up = the file
//! ends here, anything else = the next cluster of the same file. A file is a linked list of clusters; its
//! directory entry holds the first one. A directory is a file too, made of 32-byte entries.

use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const FAT_MASK: u32 = 0x0FFF_FFFF;
/// This value and everything above it in the FAT means "end of the chain".
const END_OF_CHAIN: u32 = 0x0FFF_FFF8;

const ENTRY_SIZE: usize = 32;
const ENTRY_END: u8 = 0x00; // first name byte 0: no entry here, and none after it
const ENTRY_DELETED: u8 = 0xE5;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
/// read only + hidden + system + volume ID: no real entry has all four - a long name piece has exactly these.
const ATTR_LONG_NAME: u8 = 0x0F;
/// Byte 12 of an 8.3 entry, set by Windows NT and mtools: base name and extension are lower case.
const LOWER_CASE_BASE: u8 = 0x08;
const LOWER_CASE_EXT: u8 = 0x10;
/// A long name piece holds 13 UTF-16 characters, scattered over three places of the entry.
const LONG_NAME_CHARS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
const LAST_LONG_NAME_PIECE: u8 = 0x40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    Disk(BlockError),
    /// Sector 0 is no FAT32 boot sector.
    NotFat32,
    /// Numbers on the disk that cannot be right.
    Corrupt(&'static str),
    NotFound,
    NotADirectory,
    IsADirectory,
}

impl From<BlockError> for FsError {
    fn from(error: BlockError) -> FsError {
        FsError::Disk(error)
    }
}

pub type Result<T> = core::result::Result<T, FsError>;

/// A file or directory, as its directory entry describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// The first cluster; 0 for an empty file.
    pub cluster: u32,
    pub size: u32,
}

/// A mounted FAT32 volume on the block device `D`.
pub struct FatFs<D: BlockDevice> {
    disk: D,
    sectors_per_cluster: u64,
    /// The first sector of the (first) FAT, and of the data area.
    fat_start: u64,
    data_start: u64,
    /// Clusters in the data area: valid numbers are 2 to clusters + 1.
    clusters: u32,
    root: u32,
    label: String,
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

/// "HELLO   TXT" -> "HELLO.TXT", or "hello.txt" with the lower-case flags of byte 12.
fn short_name(raw: &[u8], case: u8) -> String {
    let part = |bytes: &[u8], lower: bool| {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_end();
        if lower {
            text.to_ascii_lowercase()
        } else {
            String::from(text)
        }
    };
    let mut name = part(&raw[..8], case & LOWER_CASE_BASE != 0);
    let extension = part(&raw[8..11], case & LOWER_CASE_EXT != 0);
    if !extension.is_empty() {
        name.push('.');
        name.push_str(&extension);
    }
    name
}

/// The checksum of the 8.3 name that every piece of its long name carries.
fn checksum(raw: &[u8]) -> u8 {
    raw[..11]
        .iter()
        .fold(0u8, |sum, &byte| sum.rotate_right(1).wrapping_add(byte))
}

/// FAT names do not care about case: "HELLO.TXT" is "hello.txt".
fn same_name(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

impl<D: BlockDevice> FatFs<D> {
    /// Reads the boot sector and checks that its numbers fit together and fit the disk.
    pub fn mount(mut disk: D) -> Result<FatFs<D>> {
        let mut boot = [0u8; SECTOR_SIZE];
        disk.read(0, &mut boot)?;
        if boot[510..512] != [0x55, 0xAA] || &boot[82..90] != b"FAT32   " {
            return Err(FsError::NotFat32);
        }
        if u16_at(&boot, 11) as usize != SECTOR_SIZE {
            return Err(FsError::Corrupt("sectors that are not 512 bytes"));
        }
        let sectors_per_cluster = boot[13] as u64;
        let reserved = u16_at(&boot, 14) as u64;
        let fats = boot[16] as u64;
        let total = u32_at(&boot, 32) as u64;
        let fat_size = u32_at(&boot, 36) as u64;
        if sectors_per_cluster == 0 || fats == 0 || fat_size == 0 {
            return Err(FsError::Corrupt("a boot sector with zeros where sizes belong"));
        }
        let data_start = reserved + fats * fat_size;
        if data_start >= total || total > disk.sector_count() {
            return Err(FsError::Corrupt("a volume bigger than its disk"));
        }
        let clusters = ((total - data_start) / sectors_per_cluster) as u32;
        // the FAT must have an entry for every cluster (and the two unused ones, 0 and 1)
        if (clusters as u64 + 2) * 4 > fat_size * SECTOR_SIZE as u64 {
            return Err(FsError::Corrupt("a FAT too small for its clusters"));
        }
        let label = String::from_utf8_lossy(&boot[71..82]).trim_end().into();
        let fs = FatFs {
            disk,
            sectors_per_cluster,
            fat_start: reserved,
            data_start,
            clusters,
            root: u32_at(&boot, 44),
            label,
        };
        if !fs.valid(fs.root) {
            return Err(FsError::Corrupt("a root directory outside the volume"));
        }
        Ok(fs)
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn cluster_size(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR_SIZE
    }

    pub fn cluster_count(&self) -> u32 {
        self.clusters
    }

    fn valid(&self, cluster: u32) -> bool {
        (2..self.clusters + 2).contains(&cluster)
    }

    /// The FAT entry of `cluster`: the next cluster of its file, or "end of chain".
    fn next(&mut self, cluster: u32) -> Result<u32> {
        let offset = cluster as u64 * 4;
        let mut sector = [0u8; SECTOR_SIZE];
        self.disk
            .read(self.fat_start + offset / SECTOR_SIZE as u64, &mut sector)?;
        Ok(u32_at(&sector, (offset % SECTOR_SIZE as u64) as usize) & FAT_MASK)
    }

    /// Every cluster of the file that starts at `first`, in order. A chain read from the disk is not trusted: it
    /// may point outside the volume, or back to itself - and a loop would never end.
    fn chain(&mut self, first: u32) -> Result<Vec<u32>> {
        let mut chain = Vec::new();
        if first == 0 {
            return Ok(chain); // an empty file has no cluster at all
        }
        let mut cluster = first;
        loop {
            if !self.valid(cluster) {
                return Err(FsError::Corrupt("a cluster chain that leaves the volume"));
            }
            if chain.len() == self.clusters as usize {
                return Err(FsError::Corrupt("a cluster chain that runs in a circle"));
            }
            chain.push(cluster);
            match self.next(cluster)? {
                next if next >= END_OF_CHAIN => return Ok(chain),
                next => cluster = next,
            }
        }
    }

    fn read_cluster(&mut self, cluster: u32, buf: &mut [u8]) -> Result<()> {
        let sector = self.data_start + (cluster as u64 - 2) * self.sectors_per_cluster;
        Ok(self.disk.read(sector, buf)?)
    }

    /// The entries of the directory that starts at `first` - without "." and "..".
    fn entries(&mut self, first: u32) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut buf = vec![0u8; self.cluster_size()];
        // the pieces of a long name seen so far, by their number (1 = the first 13 characters), and their checksum
        let mut pieces: Vec<[u16; 13]> = Vec::new();
        let mut expected = 0;
        'clusters: for cluster in self.chain(first)? {
            self.read_cluster(cluster, &mut buf)?;
            for raw in buf.chunks(ENTRY_SIZE) {
                let attributes = raw[11];
                if raw[0] == ENTRY_END {
                    break 'clusters;
                }
                if raw[0] == ENTRY_DELETED {
                    pieces.clear();
                    continue;
                }
                if attributes & ATTR_LONG_NAME == ATTR_LONG_NAME {
                    // the pieces come last one first: the first piece on the disk says how many there are
                    let number = (raw[0] & 0x1F) as usize;
                    if raw[0] & LAST_LONG_NAME_PIECE != 0 {
                        pieces = vec![[0xFFFF; 13]; number];
                        expected = raw[13];
                    }
                    if number == 0 || number > pieces.len() || raw[13] != expected {
                        pieces.clear(); // a piece without the others: ignore the long name
                        continue;
                    }
                    for (i, &offset) in LONG_NAME_CHARS.iter().enumerate() {
                        pieces[number - 1][i] = u16_at(raw, offset);
                    }
                    continue;
                }
                let long = core::mem::take(&mut pieces);
                if attributes & ATTR_VOLUME_ID != 0 || raw[0] == b'.' {
                    continue; // the volume label, ".", ".."
                }
                // the long name counts only if its checksum matches this 8.3 name - else it belonged to a deleted one
                let name = if !long.is_empty() && checksum(raw) == expected {
                    let units = long.iter().flatten().copied().take_while(|&u| u != 0 && u != 0xFFFF);
                    char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect()
                } else {
                    short_name(&raw[..11], raw[12])
                };
                entries.push(Entry {
                    name,
                    is_dir: attributes & ATTR_DIRECTORY != 0,
                    cluster: (u16_at(raw, 20) as u32) << 16 | u16_at(raw, 26) as u32,
                    size: u32_at(raw, 28),
                });
            }
        }
        Ok(entries)
    }

    /// The entry of an absolute path such as "/docs/notes.txt"; "/" is the root directory.
    pub fn lookup(&mut self, path: &str) -> Result<Entry> {
        let mut entry = Entry {
            name: String::from("/"),
            is_dir: true,
            cluster: self.root,
            size: 0,
        };
        for part in path.split('/').filter(|part| !part.is_empty()) {
            if !entry.is_dir {
                return Err(FsError::NotADirectory);
            }
            entry = self
                .entries(entry.cluster)?
                .into_iter()
                .find(|e| same_name(&e.name, part))
                .ok_or(FsError::NotFound)?;
        }
        Ok(entry)
    }

    pub fn read_dir(&mut self, path: &str) -> Result<Vec<Entry>> {
        let dir = self.lookup(path)?;
        if !dir.is_dir {
            return Err(FsError::NotADirectory);
        }
        self.entries(dir.cluster)
    }

    /// The whole content of a file.
    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let file = self.lookup(path)?;
        if file.is_dir {
            return Err(FsError::IsADirectory);
        }
        let size = file.size as usize;
        let mut data = Vec::with_capacity(size);
        let mut buf = vec![0u8; self.cluster_size()];
        for cluster in self.chain(file.cluster)? {
            if data.len() == size {
                break; // a chain longer than the file: the rest is not ours to read
            }
            self.read_cluster(cluster, &mut buf)?;
            let take = (size - data.len()).min(buf.len());
            data.extend_from_slice(&buf[..take]);
        }
        if data.len() < size {
            return Err(FsError::Corrupt("a file whose chain is shorter than its size"));
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::SystemDisk;
    use alloc::collections::BTreeMap;

    /// The test disk with some sectors replaced - the disk itself is not touched.
    struct Patched(BTreeMap<u64, [u8; SECTOR_SIZE]>);

    impl BlockDevice for Patched {
        fn sector_count(&self) -> u64 {
            SystemDisk.sector_count()
        }

        fn read(&mut self, lba: u64, buf: &mut [u8]) -> core::result::Result<(), BlockError> {
            for (i, sector) in buf.chunks_mut(SECTOR_SIZE).enumerate() {
                match self.0.get(&(lba + i as u64)) {
                    Some(patch) => sector.copy_from_slice(patch),
                    None => SystemDisk.read(lba + i as u64, sector)?,
                }
            }
            Ok(())
        }
    }

    fn mounted() -> FatFs<SystemDisk> {
        FatFs::mount(SystemDisk).expect("the test disk is FAT32")
    }

    #[test_case]
    fn the_boot_sector_is_read() {
        let fs = mounted();
        assert_eq!(fs.label(), "YAKDISK");
        assert_eq!(fs.cluster_size(), 512);
        assert_eq!(fs.cluster_count(), 129_022); // what fsck.fat says about the test disk
        assert_eq!(fs.root, 2);
    }

    #[test_case]
    fn the_root_directory_lists_short_long_and_lower_case_names() {
        let names: Vec<String> = mounted().read_dir("/").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            [
                "hello.txt",
                "a long file name.txt",
                "empty.txt",
                "big.bin",
                "docs",
                "many"
            ]
        );
    }

    #[test_case]
    fn names_are_found_in_any_case() {
        let mut fs = mounted();
        assert_eq!(fs.lookup("/HELLO.TXT").unwrap().size, 21);
        assert!(fs.lookup("/Docs").unwrap().is_dir);
        assert_eq!(fs.lookup("/a LONG file name.txt").unwrap().size, 38);
    }

    #[test_case]
    fn files_are_read() {
        let mut fs = mounted();
        assert_eq!(fs.read_file("/hello.txt").unwrap(), b"Hello from the disk!\n");
        assert_eq!(fs.read_file("/docs/deeper/end.txt").unwrap(), b"The end of the path.\n");
        assert_eq!(fs.read_file("/empty.txt").unwrap(), b"");
    }

    #[test_case]
    fn a_big_file_follows_its_chain() {
        let data = mounted().read_file("/big.bin").unwrap();
        assert_eq!(data.len(), 100_000);
        assert!(data.iter().enumerate().all(|(i, &b)| b == (i % 253) as u8));
    }

    #[test_case]
    fn a_directory_can_span_clusters() {
        let mut fs = mounted();
        let many = fs.lookup("/many").unwrap();
        assert!(
            fs.chain(many.cluster).unwrap().len() > 1,
            "40 long names need more than 512 bytes"
        );
        let entries = fs.read_dir("/many").unwrap();
        assert_eq!(entries.len(), 40);
        for i in 1..=40 {
            let name = alloc::format!("file number {}.txt", i);
            assert!(entries.iter().any(|e| e.name == name), "{} is missing", name);
        }
        assert_eq!(fs.read_file("/many/file number 40.txt").unwrap(), b"file 40\n");
    }

    #[test_case]
    fn wrong_paths_are_errors() {
        let mut fs = mounted();
        assert_eq!(fs.lookup("/nope.txt"), Err(FsError::NotFound));
        assert_eq!(fs.lookup("/hello.txt/x"), Err(FsError::NotADirectory));
        assert_eq!(fs.read_file("/docs"), Err(FsError::IsADirectory));
        assert_eq!(fs.read_dir("/hello.txt").err(), Some(FsError::NotADirectory));
    }

    #[test_case]
    fn a_disk_without_fat32_is_refused() {
        let zeros = Patched(BTreeMap::from([(0, [0u8; SECTOR_SIZE])]));
        assert_eq!(FatFs::mount(zeros).err(), Some(FsError::NotFat32));
    }

    #[test_case]
    fn a_chain_in_a_circle_is_an_error_not_a_hang() {
        let mut fs = mounted();
        let first = fs.lookup("/big.bin").unwrap().cluster;
        let third = fs.chain(first).unwrap()[2];
        // let the third cluster's FAT entry point back to the first
        let offset = third as u64 * 4;
        let fat_sector = fs.fat_start + offset / SECTOR_SIZE as u64;
        let mut sector = [0u8; SECTOR_SIZE];
        SystemDisk.read(fat_sector, &mut sector).unwrap();
        let at = (offset % SECTOR_SIZE as u64) as usize;
        sector[at..at + 4].copy_from_slice(&first.to_le_bytes());
        let mut broken = FatFs::mount(Patched(BTreeMap::from([(fat_sector, sector)]))).unwrap();
        assert_eq!(
            broken.read_file("/big.bin"),
            Err(FsError::Corrupt("a cluster chain that runs in a circle"))
        );
    }

    #[test_case]
    fn the_long_name_checksum() {
        // the checksum mtools wrote into the long name of "ALONGF~1TXT" is the one `checksum` computes
        let mut fs = mounted();
        let root = fs.root;
        let mut sector = [0u8; SECTOR_SIZE];
        fs.read_cluster(root, &mut sector).unwrap();
        let short = sector
            .chunks(ENTRY_SIZE)
            .find(|e| e.starts_with(b"ALONGF~1TXT"))
            .unwrap();
        let piece = sector.chunks(ENTRY_SIZE).find(|e| e[11] == ATTR_LONG_NAME).unwrap();
        assert_eq!(checksum(short), piece[13]);
    }
}
