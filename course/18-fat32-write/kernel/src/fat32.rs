//! FAT32: files and directories on a disk, in the format of Microsoft's "FAT: General Overview of On-Disk
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
//!
//! Chapter 17 reads; chapter 18 writes: free clusters, chains, directory entries - always in the order that leaves
//! the volume usable if the machine stops between two writes.

use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const FAT_MASK: u32 = 0x0FFF_FFFF;
/// This value and everything above it in the FAT means "end of the chain".
const END_OF_CHAIN: u32 = 0x0FFF_FFF8;
/// What we write at the end of a chain (any value from `END_OF_CHAIN` up would do).
const END_MARK: u32 = 0x0FFF_FFFF;

const ENTRY_SIZE: usize = 32;
const ENTRY_END: u8 = 0x00; // first name byte 0: no entry here, and none after it
const ENTRY_DELETED: u8 = 0xE5;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
/// "Changed since the last backup" - set on every file we write, as other systems do.
const ATTR_ARCHIVE: u8 = 0x20;
/// read only + hidden + system + volume ID: no real entry has all four - a long name piece has exactly these.
const ATTR_LONG_NAME: u8 = 0x0F;
/// Byte 12 of an 8.3 entry, set by Windows NT and mtools: base name and extension are lower case.
const LOWER_CASE_BASE: u8 = 0x08;
const LOWER_CASE_EXT: u8 = 0x10;
/// A long name piece holds 13 UTF-16 characters, scattered over three places of the entry.
const LONG_NAME_CHARS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
const LAST_LONG_NAME_PIECE: u8 = 0x40;
/// The FSInfo sector (section 5): two signatures, the number of free clusters and where to look for one.
const FSINFO_LEAD: u32 = 0x4161_5252;
const FSINFO_STRUCT: u32 = 0x6141_7272;
/// 1980-01-01, the first day FAT can store: day 1, month 1, years since 1980 in the top 7 bits. Our kernel has no
/// clock that knows the date yet.
const FIRST_DAY: u16 = 1 | 1 << 5;

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
    /// A name this chapter cannot store: it needs a long name, or has characters FAT does not allow.
    BadName,
    Exists,
    /// A directory that still has entries cannot be removed.
    NotEmpty,
    NoSpace,
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
    /// Where the entry is on the disk: its long name pieces, then the 8.3 entry (empty for the root directory).
    slots: Vec<Slot>,
    /// The 8.3 name as it is on the disk - a file with a long name has this one too.
    short: [u8; 11],
}

/// The place of one 32-byte entry: a cluster of the directory, and the number of the entry in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    cluster: u32,
    index: usize,
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
    /// How many FATs, and the sectors of each: every change goes to all of them.
    fats: u64,
    fat_size: u64,
    fsinfo: u64,
    /// Free clusters, counted at mount - and where the search for the next one starts.
    free: u32,
    next_free: u32,
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
        let mut fs = FatFs {
            disk,
            sectors_per_cluster,
            fat_start: reserved,
            data_start,
            clusters,
            root: u32_at(&boot, 44),
            label,
            fats,
            fat_size,
            fsinfo: u16_at(&boot, 48) as u64,
            free: 0,
            next_free: 2,
        };
        if !fs.valid(fs.root) {
            return Err(FsError::Corrupt("a root directory outside the volume"));
        }
        fs.count_free()?;
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
        let mut piece_slots = Vec::new();
        'clusters: for cluster in self.chain(first)? {
            self.read_cluster(cluster, &mut buf)?;
            for (index, raw) in buf.chunks(ENTRY_SIZE).enumerate() {
                let slot = Slot { cluster, index };
                let attributes = raw[11];
                if raw[0] == ENTRY_END {
                    break 'clusters;
                }
                if raw[0] == ENTRY_DELETED {
                    pieces.clear();
                    piece_slots.clear();
                    continue;
                }
                if attributes & ATTR_LONG_NAME == ATTR_LONG_NAME {
                    // the pieces come last one first: the first piece on the disk says how many there are
                    let number = (raw[0] & 0x1F) as usize;
                    if raw[0] & LAST_LONG_NAME_PIECE != 0 {
                        pieces = vec![[0xFFFF; 13]; number];
                        expected = raw[13];
                        piece_slots.clear();
                    }
                    if number == 0 || number > pieces.len() || raw[13] != expected {
                        pieces.clear(); // a piece without the others: ignore the long name
                        piece_slots.clear();
                        continue;
                    }
                    for (i, &offset) in LONG_NAME_CHARS.iter().enumerate() {
                        pieces[number - 1][i] = u16_at(raw, offset);
                    }
                    piece_slots.push(slot);
                    continue;
                }
                let long = core::mem::take(&mut pieces);
                let mut slots = core::mem::take(&mut piece_slots);
                if attributes & ATTR_VOLUME_ID != 0 || raw[0] == b'.' {
                    continue; // the volume label, ".", ".."
                }
                // the long name counts only if its checksum matches this 8.3 name - else it belonged to a deleted one
                let name = if !long.is_empty() && checksum(raw) == expected {
                    let units = long.iter().flatten().copied().take_while(|&u| u != 0 && u != 0xFFFF);
                    char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect()
                } else {
                    slots.clear(); // pieces of another name are not this entry's
                    short_name(&raw[..11], raw[12])
                };
                slots.push(slot);
                entries.push(Entry {
                    name,
                    is_dir: attributes & ATTR_DIRECTORY != 0,
                    cluster: (u16_at(raw, 20) as u32) << 16 | u16_at(raw, 26) as u32,
                    size: u32_at(raw, 28),
                    slots,
                    short: raw[..11].try_into().unwrap(),
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
            slots: Vec::new(),
            short: [b' '; 11],
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

// ---- Writing (chapter 18) ----

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// The 8.3 form of `name` and the lower-case flags for byte 12: "notes.txt" -> "NOTES   TXT" + both flags. Only
/// names that fit: at most 8 + 3 characters, each part all upper or all lower case, no characters FAT forbids.
/// Anything else needs a long name, which this chapter does not write.
fn short_name_for(name: &str) -> Result<([u8; 11], u8)> {
    let (base, extension) = match name.rfind('.') {
        Some(dot) => (&name[..dot], &name[dot + 1..]),
        None => (name, ""),
    };
    let allowed = |c: u8| c.is_ascii_alphanumeric() || b"!#$%&'()-@^_`{}~".contains(&c);
    if base.is_empty() || base.len() > 8 || extension.len() > 3 || !(base.bytes().chain(extension.bytes())).all(allowed)
    {
        return Err(FsError::BadName);
    }
    let mut raw = [b' '; 11];
    let mut case = 0;
    for (part, start, flag) in [(base, 0, LOWER_CASE_BASE), (extension, 8, LOWER_CASE_EXT)] {
        let lower = part.bytes().any(|c| c.is_ascii_lowercase());
        if lower && part.bytes().any(|c| c.is_ascii_uppercase()) {
            return Err(FsError::BadName); // "ReadMe" needs a long name to keep its case
        }
        if lower {
            case |= flag;
        }
        for (i, c) in part.bytes().enumerate() {
            raw[start + i] = c.to_ascii_uppercase();
        }
    }
    Ok((raw, case))
}

/// A complete 8.3 directory entry.
fn entry_bytes(raw: &[u8; 11], case: u8, attributes: u8, cluster: u32, size: u32) -> [u8; ENTRY_SIZE] {
    let mut entry = [0u8; ENTRY_SIZE];
    entry[..11].copy_from_slice(raw);
    entry[11] = attributes;
    entry[12] = case;
    put_u16(&mut entry, 16, FIRST_DAY); // created
    put_u16(&mut entry, 18, FIRST_DAY); // last accessed
    put_u16(&mut entry, 20, (cluster >> 16) as u16);
    put_u16(&mut entry, 24, FIRST_DAY); // last written
    put_u16(&mut entry, 26, cluster as u16);
    put_u32(&mut entry, 28, size);
    entry
}

/// "/docs/new.txt" -> ("/docs", "new.txt").
fn split(path: &str) -> Result<(&str, &str)> {
    let path = path.trim_end_matches('/');
    match path.rfind('/') {
        Some(slash) if !path[slash + 1..].is_empty() => Ok((&path[..slash.max(1)], &path[slash + 1..])),
        _ => Err(FsError::BadName), // "/" itself, or no absolute path
    }
}

impl<D: BlockDevice> FatFs<D> {
    pub fn free_clusters(&self) -> u32 {
        self.free
    }

    /// Counts the free clusters by reading the whole FAT, 8 sectors at a time. The FSInfo sector has a count too,
    /// but the specification calls it a hint - after a crash it may be wrong.
    fn count_free(&mut self) -> Result<()> {
        let entries = self.clusters as u64 + 2;
        let sectors = (entries * 4).div_ceil(SECTOR_SIZE as u64);
        let mut buf = vec![0u8; 8 * SECTOR_SIZE];
        let (mut free, mut first_free) = (0, None);
        let mut sector = 0;
        while sector < sectors {
            let count = (sectors - sector).min(8);
            let bytes = count as usize * SECTOR_SIZE;
            self.disk.read(self.fat_start + sector, &mut buf[..bytes])?;
            for (i, value) in buf[..bytes].chunks(4).enumerate() {
                let cluster = (sector * SECTOR_SIZE as u64 / 4) as u32 + i as u32;
                if self.valid(cluster) && u32_at(value, 0) & FAT_MASK == 0 {
                    free += 1;
                    first_free.get_or_insert(cluster);
                }
            }
            sector += count;
        }
        self.free = free;
        self.next_free = first_free.unwrap_or(2);
        Ok(())
    }

    /// Sets the FAT entry of `cluster` - in every copy of the FAT, keeping the 4 reserved bits.
    fn set(&mut self, cluster: u32, value: u32) -> Result<()> {
        let offset = cluster as u64 * 4;
        let sector = offset / SECTOR_SIZE as u64;
        let at = (offset % SECTOR_SIZE as u64) as usize;
        let mut data = [0u8; SECTOR_SIZE];
        self.disk.read(self.fat_start + sector, &mut data)?;
        let old = u32_at(&data, at);
        put_u32(&mut data, at, (old & !FAT_MASK) | (value & FAT_MASK));
        for copy in 0..self.fats {
            self.disk.write(self.fat_start + copy * self.fat_size + sector, &data)?;
        }
        Ok(())
    }

    /// Takes `count` free clusters and links them into a chain - hung onto `after`, if given. The chain is complete
    /// in the FAT before `after` points to it: a crash in between loses clusters, but breaks no file.
    fn allocate(&mut self, count: usize, after: Option<u32>) -> Result<Vec<u32>> {
        if count > self.free as usize {
            return Err(FsError::NoSpace);
        }
        let mut found = Vec::with_capacity(count);
        let mut cluster = self.next_free;
        let mut looked_at = 0;
        while found.len() < count {
            if looked_at > self.clusters {
                return Err(FsError::Corrupt("fewer free clusters than counted"));
            }
            looked_at += 1;
            if !self.valid(cluster) {
                cluster = 2; // past the end: go on at the start
            }
            if self.next(cluster)? == 0 {
                found.push(cluster);
            }
            cluster += 1;
        }
        for (i, &c) in found.iter().enumerate().rev() {
            self.set(c, found.get(i + 1).copied().unwrap_or(END_MARK))?;
        }
        if let Some(last) = after {
            self.set(last, found[0])?;
        }
        self.free -= count as u32;
        self.next_free = cluster;
        Ok(found)
    }

    fn free_chain(&mut self, first: u32) -> Result<()> {
        for cluster in self.chain(first)? {
            self.set(cluster, 0)?;
            self.free += 1;
            self.next_free = self.next_free.min(cluster);
        }
        Ok(())
    }

    fn write_cluster(&mut self, cluster: u32, buf: &[u8]) -> Result<()> {
        let sector = self.data_start + (cluster as u64 - 2) * self.sectors_per_cluster;
        Ok(self.disk.write(sector, buf)?)
    }

    /// Writes `data` into new clusters and returns the first one (0 for no data).
    fn store(&mut self, data: &[u8], after: Option<u32>) -> Result<u32> {
        let size = self.cluster_size();
        let clusters = self.allocate(data.len().div_ceil(size), after)?;
        let mut buf = vec![0u8; size];
        for (&cluster, piece) in clusters.iter().zip(data.chunks(size)) {
            buf.fill(0);
            buf[..piece.len()].copy_from_slice(piece);
            self.write_cluster(cluster, &buf)?;
        }
        Ok(clusters.first().copied().unwrap_or(0))
    }

    /// Changes one 32-byte entry: read its cluster, change it, write it back.
    fn write_slot(&mut self, slot: Slot, change: impl FnOnce(&mut [u8])) -> Result<()> {
        let mut buf = vec![0u8; self.cluster_size()];
        self.read_cluster(slot.cluster, &mut buf)?;
        change(&mut buf[slot.index * ENTRY_SIZE..(slot.index + 1) * ENTRY_SIZE]);
        self.write_cluster(slot.cluster, &buf)
    }

    /// A free entry in the directory that starts at `first` - a deleted one, or one at the end. A full directory
    /// grows by a cluster of zeros: all of it "end of the directory".
    fn free_slot(&mut self, first: u32) -> Result<Slot> {
        let mut buf = vec![0u8; self.cluster_size()];
        let chain = self.chain(first)?;
        for &cluster in &chain {
            self.read_cluster(cluster, &mut buf)?;
            if let Some(index) = buf
                .chunks(ENTRY_SIZE)
                .position(|e| e[0] == ENTRY_END || e[0] == ENTRY_DELETED)
            {
                return Ok(Slot { cluster, index });
            }
        }
        let new = self.allocate(1, None)?[0];
        buf.fill(0);
        self.write_cluster(new, &buf)?;
        self.set(*chain.last().expect("a directory has a cluster"), new)?;
        Ok(Slot { cluster: new, index: 0 })
    }

    /// Checks that `path` can be created: a name that fits, a parent that is a directory, no entry of that name.
    fn prepare(&mut self, path: &str) -> Result<([u8; 11], u8, u32)> {
        let (parent, name) = split(path)?;
        let (raw, case) = short_name_for(name)?;
        let dir = self.lookup(parent)?;
        if !dir.is_dir {
            return Err(FsError::NotADirectory);
        }
        // a file with a long name has an 8.3 name too ("ALONGF~1.TXT"): two entries must not share one
        let entries = self.entries(dir.cluster)?;
        if entries.iter().any(|e| same_name(&e.name, name) || e.short == raw) {
            return Err(FsError::Exists);
        }
        Ok((raw, case, dir.cluster))
    }

    /// Writes the free count back to the FSInfo sector and makes the disk write its cache to the medium. Every
    /// public operation ends with it: when it returns, the change is on the disk.
    fn sync(&mut self) -> Result<()> {
        let mut info = [0u8; SECTOR_SIZE];
        self.disk.read(self.fsinfo, &mut info)?;
        if u32_at(&info, 0) == FSINFO_LEAD && u32_at(&info, 484) == FSINFO_STRUCT {
            put_u32(&mut info, 488, self.free);
            put_u32(&mut info, 492, self.next_free);
            self.disk.write(self.fsinfo, &info)?;
        }
        Ok(self.disk.flush()?)
    }

    /// Creates the file `path`, or replaces what it holds.
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let size = u32::try_from(data.len()).map_err(|_| FsError::NoSpace)?;
        match self.lookup(path) {
            Ok(old) if old.is_dir => return Err(FsError::IsADirectory),
            Ok(old) => {
                // new data first, then the entry points to it, then the old clusters are freed
                let first = self.store(data, None)?;
                let slot = *old.slots.last().expect("a file has an entry");
                self.write_slot(slot, |e| {
                    put_u16(e, 20, (first >> 16) as u16);
                    put_u16(e, 26, first as u16);
                    put_u32(e, 28, size);
                })?;
                self.free_chain(old.cluster)?;
            }
            Err(FsError::NotFound) => {
                let (raw, case, dir) = self.prepare(path)?;
                let first = self.store(data, None)?;
                let slot = self.free_slot(dir)?;
                self.write_slot(slot, |e| {
                    e.copy_from_slice(&entry_bytes(&raw, case, ATTR_ARCHIVE, first, size))
                })?;
            }
            Err(other) => return Err(other),
        }
        self.sync()
    }

    /// Adds `data` to the end of the file `path`: first into the free part of its last cluster, then into new ones.
    pub fn append(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let file = self.lookup(path)?;
        if file.is_dir {
            return Err(FsError::IsADirectory);
        }
        if file.cluster == 0 {
            return self.write_file(path, data); // an empty file has no cluster to add to
        }
        let size = self.cluster_size();
        let new_size = u32::try_from(file.size as usize + data.len()).map_err(|_| FsError::NoSpace)?;
        let chain = self.chain(file.cluster)?;
        let last = *chain.last().expect("cluster != 0");
        if chain.len() != (file.size as usize).div_ceil(size).max(1) {
            return Err(FsError::Corrupt("a chain that does not fit the size of its file"));
        }
        let used = file.size as usize - (chain.len() - 1) * size;
        let (fits, rest) = data.split_at(data.len().min(size - used));
        if !fits.is_empty() {
            let mut buf = vec![0u8; size];
            self.read_cluster(last, &mut buf)?;
            buf[used..used + fits.len()].copy_from_slice(fits);
            self.write_cluster(last, &buf)?;
        }
        if !rest.is_empty() {
            self.store(rest, Some(last))?;
        }
        let slot = *file.slots.last().expect("a file has an entry");
        self.write_slot(slot, |e| put_u32(e, 28, new_size))?;
        self.sync()
    }

    /// Makes an empty directory: one cluster with "." (itself) and ".." (its parent; 0 stands for the root).
    pub fn create_dir(&mut self, path: &str) -> Result<()> {
        let (raw, case, parent) = self.prepare(path)?;
        let cluster = self.allocate(1, None)?[0];
        let mut buf = vec![0u8; self.cluster_size()];
        buf[..ENTRY_SIZE].copy_from_slice(&entry_bytes(b".          ", 0, ATTR_DIRECTORY, cluster, 0));
        let up = if parent == self.root { 0 } else { parent };
        buf[ENTRY_SIZE..2 * ENTRY_SIZE].copy_from_slice(&entry_bytes(b"..         ", 0, ATTR_DIRECTORY, up, 0));
        self.write_cluster(cluster, &buf)?;
        let slot = self.free_slot(parent)?;
        self.write_slot(slot, |e| {
            e.copy_from_slice(&entry_bytes(&raw, case, ATTR_DIRECTORY, cluster, 0))
        })?;
        self.sync()
    }

    /// Removes a file or an empty directory: first the entry (with the pieces of its long name), then its clusters.
    pub fn remove(&mut self, path: &str) -> Result<()> {
        split(path)?; // not "/"
        let entry = self.lookup(path)?;
        if entry.is_dir && !self.entries(entry.cluster)?.is_empty() {
            return Err(FsError::NotEmpty);
        }
        for &slot in &entry.slots {
            self.write_slot(slot, |e| e[0] = ENTRY_DELETED)?;
        }
        self.free_chain(entry.cluster)?;
        self.sync()
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

        /// Writes stay in RAM: the disk under the patches does not change.
        fn write(&mut self, lba: u64, buf: &[u8]) -> core::result::Result<(), BlockError> {
            for (i, sector) in buf.chunks(SECTOR_SIZE).enumerate() {
                self.0.insert(lba + i as u64, sector.try_into().unwrap());
            }
            Ok(())
        }

        fn flush(&mut self) -> core::result::Result<(), BlockError> {
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
        // what mkdisk.sh made comes first; the tests run in the order of their names, and the writing ones may
        // already have added their files
        assert_eq!(
            names[..7],
            [
                "hello.txt",
                "a long file name.txt",
                "empty.txt",
                "big.bin",
                "docs",
                "many",
                "trash"
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

    // ---- writing (chapter 18) ----

    /// The volume the kernel mounted at boot. Tests that write to the real disk must use it: a second `FatFs` of
    /// the same disk counts free clusters on its own, and whichever writes the FSInfo sector last writes a wrong
    /// count - `fsck.fat` found exactly that.
    fn volume() -> spin::MutexGuard<'static, Option<FatFs<SystemDisk>>> {
        crate::fs::FS.lock()
    }

    /// A pattern that differs in every cluster: byte i is i % 251.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test_case]
    fn the_free_count_stays_exact() {
        // what `allocate` and `free_chain` keep up to date is what a new count of the FAT finds
        let mut fs = FatFs::mount(Patched(BTreeMap::new())).unwrap();
        fs.write_file("/count.bin", &pattern(5000)).unwrap();
        fs.write_file("/count.bin", &pattern(700)).unwrap();
        fs.remove("/hello.txt").unwrap();
        let kept = fs.free_clusters();
        fs.count_free().unwrap();
        assert_eq!(fs.free_clusters(), kept);
    }

    #[test_case]
    fn a_new_file_is_written_and_read_back() {
        let mut guard = volume();
        let fs = guard.as_mut().unwrap();
        let free = fs.free_clusters();
        fs.write_file("/new.txt", b"Written by YAK.\n").unwrap();
        assert_eq!(fs.read_file("/new.txt").unwrap(), b"Written by YAK.\n");
        assert_eq!(fs.free_clusters(), free - 1);
        // a second mount reads it from the disk, not from anything this one remembers
        let mut again = mounted();
        assert_eq!(again.read_file("/NEW.TXT").unwrap(), b"Written by YAK.\n");
        assert_eq!(again.free_clusters(), free - 1);
    }

    #[test_case]
    fn replacing_a_file_gives_its_clusters_back() {
        let mut guard = volume();
        let fs = guard.as_mut().unwrap();
        fs.write_file("/grow.bin", &pattern(3000)).unwrap(); // 6 clusters
        let free = fs.free_clusters();
        assert_eq!(fs.read_file("/grow.bin").unwrap(), pattern(3000));
        fs.write_file("/grow.bin", b"small").unwrap();
        assert_eq!(fs.read_file("/grow.bin").unwrap(), b"small");
        assert_eq!(fs.free_clusters(), free + 5);
        fs.write_file("/grow.bin", b"").unwrap();
        assert_eq!(fs.lookup("/grow.bin").unwrap().cluster, 0);
        assert_eq!(fs.free_clusters(), free + 6);
    }

    #[test_case]
    fn append_fills_the_last_cluster_then_grows() {
        let mut guard = volume();
        let fs = guard.as_mut().unwrap();
        fs.write_file("/log.txt", &[b'a'; 500]).unwrap();
        fs.append("/log.txt", &[b'b'; 30]).unwrap(); // 12 bytes into the first cluster, 18 into a new one
        fs.append("/log.txt", &[b'c'; 1000]).unwrap();
        let data = fs.read_file("/log.txt").unwrap();
        assert_eq!(data.len(), 1530);
        assert!(data[..500].iter().all(|&b| b == b'a') && data[500..530].iter().all(|&b| b == b'b'));
        assert!(data[530..].iter().all(|&b| b == b'c'));
        let file = fs.lookup("/log.txt").unwrap();
        assert_eq!(fs.chain(file.cluster).unwrap().len(), 3);
        fs.write_file("/empty2.txt", b"").unwrap();
        fs.append("/empty2.txt", b"first words").unwrap();
        assert_eq!(fs.read_file("/empty2.txt").unwrap(), b"first words");
    }

    #[test_case]
    fn directories_are_made_and_grow() {
        let mut guard = volume();
        let fs = guard.as_mut().unwrap();
        fs.create_dir("/made").unwrap();
        assert!(fs.lookup("/made").unwrap().is_dir);
        assert!(fs.read_dir("/made").unwrap().is_empty());
        // ".", ".." and 20 entries: 22 x 32 bytes, more than one cluster of 512
        for i in 0..20 {
            let path = alloc::format!("/made/f{:02}.txt", i);
            fs.write_file(&path, path.as_bytes()).unwrap();
        }
        let made = fs.lookup("/made").unwrap();
        assert_eq!(fs.chain(made.cluster).unwrap().len(), 2);
        assert_eq!(fs.read_dir("/made").unwrap().len(), 20);
        assert_eq!(fs.read_file("/made/f19.txt").unwrap(), b"/made/f19.txt");
        fs.create_dir("/made/sub").unwrap();
        fs.write_file("/made/sub/deep.txt", b"deep\n").unwrap();
        assert_eq!(fs.read_file("/made/sub/deep.txt").unwrap(), b"deep\n");
    }

    #[test_case]
    fn removing_frees_clusters_and_the_name() {
        let mut guard = volume();
        let fs = guard.as_mut().unwrap();
        fs.write_file("/gone.bin", &pattern(2000)).unwrap();
        // counted after writing: the entry may have needed a new cluster for the directory, which stays
        let free = fs.free_clusters();
        fs.remove("/gone.bin").unwrap();
        assert_eq!(fs.lookup("/gone.bin"), Err(FsError::NotFound));
        assert_eq!(fs.free_clusters(), free + 4);
        // a file with a long name: its pieces are deleted with it, or fsck.fat would find them alone
        fs.remove("/trash/to be removed.txt").unwrap();
        assert!(fs.read_dir("/trash").unwrap().is_empty());
        fs.create_dir("/full").unwrap();
        fs.write_file("/full/x", b"x").unwrap();
        assert_eq!(fs.remove("/full"), Err(FsError::NotEmpty));
        fs.remove("/full/x").unwrap();
        fs.remove("/full").unwrap();
    }

    #[test_case]
    fn names_and_paths_are_checked() {
        let mut guard = volume();
        let fs = guard.as_mut().unwrap();
        assert_eq!(fs.write_file("/a long name.txt", b""), Err(FsError::BadName));
        assert_eq!(fs.write_file("/ReadMe.txt", b""), Err(FsError::BadName));
        assert_eq!(fs.write_file("/toolongname.txt", b""), Err(FsError::BadName));
        assert_eq!(fs.write_file("/nodir/x.txt", b""), Err(FsError::NotFound));
        assert_eq!(fs.write_file("/hello.txt/x", b""), Err(FsError::NotADirectory));
        assert_eq!(fs.write_file("/docs", b""), Err(FsError::IsADirectory));
        assert_eq!(fs.create_dir("/docs"), Err(FsError::Exists));
        assert_eq!(fs.remove("/docs"), Err(FsError::NotEmpty));
        assert_eq!(fs.remove("/"), Err(FsError::BadName));
        // the 8.3 alias of a file in "many/" is taken, even though nobody sees it in `ls`
        let alias = String::from_utf8_lossy(&fs.lookup("/many/file number 1.txt").unwrap().short).into_owned();
        let alias = alloc::format!("/many/{}.{}", alias[..8].trim_end(), alias[8..].trim_end());
        assert_eq!(fs.write_file(&alias, b""), Err(FsError::Exists), "{}", alias);
    }

    #[test_case]
    fn a_full_volume_is_refused_before_anything_is_written() {
        let mut fs = FatFs::mount(Patched(BTreeMap::new())).unwrap();
        fs.free = 3; // pretend: only 3 clusters left
        assert_eq!(fs.write_file("/big2.bin", &pattern(4 * 512)), Err(FsError::NoSpace));
        assert_eq!(fs.free_clusters(), 3);
        assert_eq!(fs.lookup("/big2.bin"), Err(FsError::NotFound));
    }

    #[test_case]
    fn files_for_the_host_to_check() {
        // `make test` checks these with fsck.fat and mtools after QEMU has ended
        let mut guard = volume();
        let fs = guard.as_mut().unwrap();
        fs.write_file("/kept.txt", b"Written by YAK, read by mtools.\n")
            .unwrap();
        fs.append("/kept.txt", b"And appended.\n").unwrap();
        fs.create_dir("/keptdir").unwrap();
        fs.write_file("/keptdir/big.bin", &pattern(100_000)).unwrap();
    }
}
