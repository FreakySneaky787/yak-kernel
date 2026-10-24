//! FAT32 file system: read and write, with long file names (VFAT/LFN).
//!
//! On-disk layout (all offsets relative to the start of the volume):
//! - sector 0: boot sector with the BIOS parameter block (BPB)
//! - FSInfo sector: hint for free cluster count / next free cluster
//! - reserved sectors, then `num_fats` copies of the FAT
//! - data area, divided into clusters (numbered from 2)
//!
//! The FAT is an array of 32-bit entries (28 bits used), one per cluster:
//! 0 = free, >= 0x0FFFFFF8 = end of chain, otherwise the next cluster of the
//! same file. Directories are files made of 32-byte entries. A long name is
//! stored in extra "LFN" entries (13 UTF-16 characters each) placed right
//! before the classic 8.3 entry.

use crate::drivers::block::{BlockDevice, BlockError, SECTOR_SIZE};
use crate::drivers::rtc::DateTime;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const FAT_MASK: u32 = 0x0FFF_FFFF;
const FAT_EOC: u32 = 0x0FFF_FFFF;
const FAT_EOC_MIN: u32 = 0x0FFF_FFF8;
const FAT_BAD: u32 = 0x0FFF_FFF7;

const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
pub const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

const ENTRY_SIZE: usize = 32;
const ENTRY_FREE: u8 = 0xE5;
const ENTRY_END: u8 = 0x00;
const LFN_LAST: u8 = 0x40;
const LFN_CHARS: usize = 13;
/// Byte offsets of the 13 UTF-16 characters inside an LFN entry.
const LFN_OFFSETS: [usize; LFN_CHARS] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
/// NT reserved byte flags: base name / extension stored in lower case.
const CASE_LOWER_BASE: u8 = 0x08;
const CASE_LOWER_EXT: u8 = 0x10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    NotFat32,
    Corrupt(&'static str),
    Io(BlockError),
    NotFound,
    NotADirectory,
    IsADirectory,
    AlreadyExists,
    DirectoryNotEmpty,
    InvalidName,
    InvalidPath,
    NoSpace,
    FileTooLarge,
    NotMounted,
}

impl From<BlockError> for FsError {
    fn from(e: BlockError) -> FsError {
        FsError::Io(e)
    }
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            FsError::NotFat32 => write!(f, "not a FAT32 file system"),
            FsError::Corrupt(what) => write!(f, "file system corrupt: {}", what),
            FsError::Io(e) => write!(f, "disk error: {:?}", e),
            FsError::NotFound => write!(f, "no such file or directory"),
            FsError::NotADirectory => write!(f, "not a directory"),
            FsError::IsADirectory => write!(f, "is a directory"),
            FsError::AlreadyExists => write!(f, "already exists"),
            FsError::DirectoryNotEmpty => write!(f, "directory not empty"),
            FsError::InvalidName => write!(f, "invalid file name"),
            FsError::InvalidPath => write!(f, "invalid path"),
            FsError::NoSpace => write!(f, "no space left on disk"),
            FsError::FileTooLarge => write!(f, "file too large (FAT32 limit is 4 GiB)"),
            FsError::NotMounted => write!(f, "no file system mounted"),
        }
    }
}

pub type Result<T> = core::result::Result<T, FsError>;

/// Position of one 32-byte directory slot: (cluster of the directory, slot index inside it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SlotLoc {
    cluster: u32,
    index: usize,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Long name if present, otherwise the 8.3 name.
    pub name: String,
    pub attr: u8,
    pub cluster: u32,
    pub size: u32,
    pub modified: DateTime,
    short_name: [u8; 11],
    /// All slots of this entry (LFN slots followed by the 8.3 slot); empty for the root directory.
    slots: Vec<SlotLoc>,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.attr & ATTR_DIRECTORY != 0
    }

    pub fn short_name(&self) -> String {
        format_short_name(&self.short_name, 0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub cluster_size: usize,
    pub total_clusters: u32,
    pub free_clusters: u32,
}

pub struct FatFs<D: BlockDevice> {
    dev: D,
    /// LBA of the volume's first sector (0 for a disk without partition table).
    volume_start: u64,
    sectors_per_cluster: u64,
    bytes_per_cluster: usize,
    reserved_sectors: u64,
    num_fats: u64,
    fat_sectors: u64,
    data_start: u64,
    cluster_count: u32,
    root_cluster: u32,
    fsinfo_sector: u64,
    free_clusters: u32,
    next_free: u32,
    label: String,
    /// One cached sector of the first FAT: (sector relative to the volume, data).
    fat_cache: Option<(u64, [u8; SECTOR_SIZE])>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn put_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

// ---- dates ----------------------------------------------------------------------

fn decode_datetime(date: u16, time: u16) -> DateTime {
    DateTime {
        year: 1980 + (date >> 9),
        month: ((date >> 5) & 0xF) as u8,
        day: (date & 0x1F) as u8,
        hour: (time >> 11) as u8,
        minute: ((time >> 5) & 0x3F) as u8,
        second: ((time & 0x1F) * 2) as u8,
    }
}

fn encode_datetime(t: DateTime) -> (u16, u16) {
    let year = t.year.clamp(1980, 2107) - 1980;
    let date = year << 9 | (t.month as u16) << 5 | t.day as u16;
    let time = (t.hour as u16) << 11 | (t.minute as u16) << 5 | (t.second as u16) / 2;
    (date, time)
}

fn now() -> DateTime {
    crate::drivers::rtc::now()
}

// ---- names ------------------------------------------------------------------------

const SHORT_SPECIAL: &[u8] = b"!#$%&'()-@^_`{}~";

fn is_short_char(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || SHORT_SPECIAL.contains(&c) || c >= 0x80
}

/// "README  TXT" + case flags -> "README.TXT" / "readme.txt"
fn format_short_name(raw: &[u8; 11], case: u8) -> String {
    let lower = |s: &[u8], flag: u8| -> String {
        let s = core::str::from_utf8(s).unwrap_or("?").trim_end();
        if case & flag != 0 {
            s.to_ascii_lowercase()
        } else {
            String::from(s)
        }
    };
    let mut name = lower(&raw[..8], CASE_LOWER_BASE);
    let ext = lower(&raw[8..], CASE_LOWER_EXT);
    if name.starts_with('\u{5}') {
        name.replace_range(..1, "\u{E5}"); // 0x05 stands for a real 0xE5 first byte
    }
    if !ext.is_empty() {
        name.push('.');
        name.push_str(&ext);
    }
    name
}

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &c| (sum >> 1 | sum << 7).wrapping_add(c))
}

/// Checks a name for characters FAT does not allow.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.chars().count() > 255 {
        return Err(FsError::InvalidName);
    }
    if name.chars().any(|c| (c as u32) < 0x20 || "\"*/:<>?\\|".contains(c)) {
        return Err(FsError::InvalidName);
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(FsError::InvalidName);
    }
    Ok(())
}

/// If `name` can be stored as a plain 8.3 entry, returns the raw name and
/// the case flags (e.g. "readme.txt" -> "README  TXT" + lower-case flags).
fn as_short_name(name: &str) -> Option<([u8; 11], u8)> {
    if !name.is_ascii() {
        return None;
    }
    let (base, ext) = match name.rfind('.') {
        Some(0) => return None,
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 || base.contains('.') {
        return None;
    }
    let mut case = 0;
    for (part, flag) in [(base, CASE_LOWER_BASE), (ext, CASE_LOWER_EXT)] {
        let has_lower = part.bytes().any(|b| b.is_ascii_lowercase());
        let has_upper = part.bytes().any(|b| b.is_ascii_uppercase());
        if has_lower && has_upper {
            return None; // mixed case needs a long name
        }
        if has_lower {
            case |= flag;
        }
    }
    let mut raw = [b' '; 11];
    for (i, b) in base.bytes().enumerate() {
        raw[i] = b.to_ascii_uppercase();
    }
    for (i, b) in ext.bytes().enumerate() {
        raw[8 + i] = b.to_ascii_uppercase();
    }
    if !raw.iter().all(|&b| b == b' ' || is_short_char(b)) {
        return None;
    }
    Some((raw, case))
}

/// Builds a "BASIS~N.EXT" alias for a long name.
fn short_alias(name: &str, n: u32) -> [u8; 11] {
    let clean = |s: &str| -> Vec<u8> {
        s.chars()
            .filter(|&c| c != ' ' && c != '.')
            .map(|c| {
                let u = c.to_ascii_uppercase();
                if u.is_ascii() && is_short_char(u as u8) {
                    u as u8
                } else {
                    b'_'
                }
            })
            .collect()
    };
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let base = clean(base);
    let ext = clean(ext);
    let tail = alloc::format!("~{}", n);
    let keep = (8 - tail.len()).min(base.len().max(1));
    let mut raw = [b' '; 11];
    for (i, &b) in base.iter().take(keep).enumerate() {
        raw[i] = b;
    }
    if base.is_empty() {
        raw[0] = b'_';
    }
    for (i, b) in tail.bytes().enumerate() {
        raw[keep + i] = b;
    }
    for (i, &b) in ext.iter().take(3).enumerate() {
        raw[8 + i] = b;
    }
    raw
}

fn names_equal(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

// ---- formatting a new volume ------------------------------------------------------------

/// Writes a fresh, empty FAT32 file system onto `dev` (whole device, 1 sector per cluster).
/// Volumes below the spec minimum of 65525 clusters work with this driver
/// (handy for small RAM disks in tests), but other systems may refuse them.
pub fn format<D: BlockDevice>(dev: &mut D, label: &str) -> Result<()> {
    let total = dev.sector_count();
    if total < 2048 || total > u32::MAX as u64 {
        return Err(FsError::NoSpace);
    }
    let reserved: u64 = 32;
    let num_fats: u64 = 2;
    // FAT size: each data cluster needs 4 bytes in each FAT; iterate until stable
    let mut fat_sectors = 1u64;
    loop {
        let clusters = total - reserved - num_fats * fat_sectors;
        let needed = ((clusters + 2) * 4).div_ceil(SECTOR_SIZE as u64);
        if needed <= fat_sectors {
            break;
        }
        fat_sectors = needed;
    }
    let clusters = (total - reserved - num_fats * fat_sectors) as u32;

    let mut boot = [0u8; SECTOR_SIZE];
    boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]); // jmp short + nop
    boot[3..11].copy_from_slice(b"KERNTRY ");
    put_u16(&mut boot, 11, SECTOR_SIZE as u16);
    boot[13] = 1; // sectors per cluster
    put_u16(&mut boot, 14, reserved as u16);
    boot[16] = num_fats as u8;
    boot[21] = 0xF8; // media: fixed disk
    put_u16(&mut boot, 24, 63); // sectors per track (legacy)
    put_u16(&mut boot, 26, 255); // heads (legacy)
    put_u32(&mut boot, 32, total as u32);
    put_u32(&mut boot, 36, fat_sectors as u32);
    put_u32(&mut boot, 44, 2); // root directory cluster
    put_u16(&mut boot, 48, 1); // FSInfo sector
    put_u16(&mut boot, 50, 6); // backup boot sector
    boot[64] = 0x80; // drive number
    boot[66] = 0x29; // extended boot signature
    put_u32(&mut boot, 67, 0x4B54_5259); // volume serial
    let mut raw_label = [b' '; 11];
    for (i, b) in label.bytes().take(11).enumerate() {
        raw_label[i] = b.to_ascii_uppercase();
    }
    boot[71..82].copy_from_slice(&raw_label);
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510] = 0x55;
    boot[511] = 0xAA;

    let mut fsinfo = [0u8; SECTOR_SIZE];
    put_u32(&mut fsinfo, 0, 0x4161_5252);
    put_u32(&mut fsinfo, 484, 0x6141_7272);
    put_u32(&mut fsinfo, 488, clusters - 1); // root directory uses one cluster
    put_u32(&mut fsinfo, 492, 3);
    put_u32(&mut fsinfo, 508, 0xAA55_0000);

    dev.write(0, &boot)?;
    dev.write(1, &fsinfo)?;
    dev.write(6, &boot)?;
    dev.write(7, &fsinfo)?;

    // Zero both FATs, the root cluster and everything in between (chunked)
    let zero = vec![0u8; 8 * SECTOR_SIZE];
    let end = reserved + num_fats * fat_sectors + 1;
    let mut lba = reserved;
    while lba < end {
        let n = (end - lba).min(8);
        dev.write(lba, &zero[..n as usize * SECTOR_SIZE])?;
        lba += n;
    }
    let mut first = [0u8; SECTOR_SIZE];
    put_u32(&mut first, 0, 0x0FFF_FFF8); // FAT[0]: media byte
    put_u32(&mut first, 4, FAT_EOC); // FAT[1]
    put_u32(&mut first, 8, FAT_EOC); // FAT[2]: root directory, one cluster
    for i in 0..num_fats {
        dev.write(reserved + i * fat_sectors, &first)?;
    }
    dev.flush()?;
    Ok(())
}

// ---- the file system ------------------------------------------------------------------

impl<D: BlockDevice> FatFs<D> {
    /// Mounts a FAT32 volume: either directly at sector 0 or in the first
    /// FAT32 partition of an MBR partition table.
    pub fn mount(mut dev: D) -> Result<FatFs<D>> {
        let mut sector = [0u8; SECTOR_SIZE];
        dev.read(0, &mut sector)?;
        let mut volume_start = 0;
        if &sector[82..90] != b"FAT32   " {
            // Look for a FAT32 partition (type 0x0B or 0x0C) in the MBR
            if sector[510..512] != [0x55, 0xAA] {
                return Err(FsError::NotFat32);
            }
            let part = (0..4)
                .map(|i| &sector[446 + i * 16..446 + (i + 1) * 16])
                .find(|p| p[4] == 0x0B || p[4] == 0x0C)
                .ok_or(FsError::NotFat32)?;
            volume_start = u32_at(part, 8) as u64;
            dev.read(volume_start, &mut sector)?;
            if &sector[82..90] != b"FAT32   " {
                return Err(FsError::NotFat32);
            }
        }
        if u16_at(&sector, 11) as usize != SECTOR_SIZE {
            return Err(FsError::Corrupt("only 512-byte sectors are supported"));
        }
        let sectors_per_cluster = sector[13] as u64;
        let reserved_sectors = u16_at(&sector, 14) as u64;
        let num_fats = sector[16] as u64;
        let total = u32_at(&sector, 32) as u64;
        let fat_sectors = u32_at(&sector, 36) as u64;
        let root_cluster = u32_at(&sector, 44);
        let fsinfo_sector = u16_at(&sector, 48) as u64;
        if sectors_per_cluster == 0 || num_fats == 0 || fat_sectors == 0 || root_cluster < 2 {
            return Err(FsError::Corrupt("invalid BPB"));
        }
        let data_start = reserved_sectors + num_fats * fat_sectors;
        let cluster_count = ((total - data_start) / sectors_per_cluster) as u32;
        let label = String::from(core::str::from_utf8(&sector[71..82]).unwrap_or("").trim_end());

        let mut fs = FatFs {
            dev,
            volume_start,
            sectors_per_cluster,
            bytes_per_cluster: sectors_per_cluster as usize * SECTOR_SIZE,
            reserved_sectors,
            num_fats,
            fat_sectors,
            data_start,
            cluster_count,
            root_cluster,
            fsinfo_sector,
            free_clusters: 0,
            next_free: 2,
            label,
            fat_cache: None,
        };
        fs.count_free_clusters()?;
        Ok(fs)
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn stats(&self) -> Stats {
        Stats {
            cluster_size: self.bytes_per_cluster,
            total_clusters: self.cluster_count,
            free_clusters: self.free_clusters,
        }
    }

    /// Gives the block device back (e.g. to inspect a RAM disk in tests).
    pub fn into_device(self) -> D {
        self.dev
    }

    // ---- low level: sectors, FAT, clusters ----

    fn read_sectors(&mut self, sector: u64, buf: &mut [u8]) -> Result<()> {
        Ok(self.dev.read(self.volume_start + sector, buf)?)
    }

    fn write_sectors(&mut self, sector: u64, buf: &[u8]) -> Result<()> {
        Ok(self.dev.write(self.volume_start + sector, buf)?)
    }

    fn valid_cluster(&self, c: u32) -> bool {
        c >= 2 && c < self.cluster_count + 2
    }

    /// Scans the whole FAT once (at mount) for an exact free count.
    fn count_free_clusters(&mut self) -> Result<()> {
        let mut free = 0;
        let mut first_free = None;
        let entries_per_sector = (SECTOR_SIZE / 4) as u32;
        let mut buf = vec![0u8; 8 * SECTOR_SIZE];
        let last = self.cluster_count + 2;
        let mut sector = 0u64;
        while (sector as u32) * entries_per_sector < last {
            let n = (self.fat_sectors - sector).min(8);
            let len = n as usize * SECTOR_SIZE;
            self.read_sectors(self.reserved_sectors + sector, &mut buf[..len])?;
            for i in 0..len / 4 {
                let cluster = sector as u32 * entries_per_sector + i as u32;
                if cluster >= 2 && cluster < last && u32_at(&buf, i * 4) & FAT_MASK == 0 {
                    free += 1;
                    first_free.get_or_insert(cluster);
                }
            }
            sector += n;
        }
        self.free_clusters = free;
        self.next_free = first_free.unwrap_or(2);
        Ok(())
    }

    fn fat_position(&self, cluster: u32) -> (u64, usize) {
        let offset = cluster as u64 * 4;
        (
            self.reserved_sectors + offset / SECTOR_SIZE as u64,
            (offset % SECTOR_SIZE as u64) as usize,
        )
    }

    fn load_fat_sector(&mut self, sector: u64) -> Result<&mut [u8; SECTOR_SIZE]> {
        if self.fat_cache.map(|(s, _)| s) != Some(sector) {
            let mut data = [0u8; SECTOR_SIZE];
            self.read_sectors(sector, &mut data)?;
            self.fat_cache = Some((sector, data));
        }
        Ok(&mut self.fat_cache.as_mut().unwrap().1)
    }

    fn fat_get(&mut self, cluster: u32) -> Result<u32> {
        let (sector, offset) = self.fat_position(cluster);
        Ok(u32_at(self.load_fat_sector(sector)?, offset) & FAT_MASK)
    }

    /// Sets a FAT entry in every FAT copy (keeping the 4 reserved top bits).
    fn fat_set(&mut self, cluster: u32, value: u32) -> Result<()> {
        let (sector, offset) = self.fat_position(cluster);
        let data = self.load_fat_sector(sector)?;
        let old = u32_at(data, offset);
        put_u32(data, offset, (old & !FAT_MASK) | (value & FAT_MASK));
        let copy = *data;
        for i in 0..self.num_fats {
            self.write_sectors(sector + i * self.fat_sectors, &copy)?;
        }
        Ok(())
    }

    fn cluster_sector(&self, cluster: u32) -> u64 {
        self.data_start + (cluster as u64 - 2) * self.sectors_per_cluster
    }

    fn read_cluster(&mut self, cluster: u32, buf: &mut [u8]) -> Result<()> {
        if !self.valid_cluster(cluster) {
            return Err(FsError::Corrupt("cluster number out of range"));
        }
        self.read_sectors(self.cluster_sector(cluster), buf)
    }

    fn write_cluster(&mut self, cluster: u32, buf: &[u8]) -> Result<()> {
        if !self.valid_cluster(cluster) {
            return Err(FsError::Corrupt("cluster number out of range"));
        }
        self.write_sectors(self.cluster_sector(cluster), buf)
    }

    /// All clusters of the chain starting at `first` (empty for cluster 0).
    fn chain(&mut self, first: u32) -> Result<Vec<u32>> {
        let mut clusters = Vec::new();
        let mut c = first;
        while c != 0 && c < FAT_EOC_MIN {
            if !self.valid_cluster(c) || c == FAT_BAD || clusters.len() > self.cluster_count as usize {
                return Err(FsError::Corrupt("broken cluster chain"));
            }
            clusters.push(c);
            c = self.fat_get(c)?;
        }
        Ok(clusters)
    }

    /// Allocates `count` clusters, links them into a chain (appended to
    /// `after`, if given) and returns them. Nothing is allocated on failure.
    fn alloc_clusters(&mut self, count: usize, after: Option<u32>) -> Result<Vec<u32>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if count > self.free_clusters as usize {
            return Err(FsError::NoSpace);
        }
        let mut found = Vec::with_capacity(count);
        let mut c = self.next_free;
        let mut scanned = 0;
        while found.len() < count && scanned < self.cluster_count {
            if !self.valid_cluster(c) {
                c = 2;
            }
            if self.fat_get(c)? == 0 {
                found.push(c);
            }
            c += 1;
            scanned += 1;
        }
        if found.len() < count {
            return Err(FsError::NoSpace);
        }
        for (i, &cluster) in found.iter().enumerate() {
            let next = found.get(i + 1).copied().unwrap_or(FAT_EOC);
            self.fat_set(cluster, next)?;
        }
        if let Some(prev) = after {
            self.fat_set(prev, found[0])?;
        }
        self.free_clusters -= count as u32;
        self.next_free = if self.valid_cluster(c) { c } else { 2 };
        Ok(found)
    }

    fn free_chain(&mut self, first: u32) -> Result<()> {
        for c in self.chain(first)? {
            self.fat_set(c, 0)?;
            self.free_clusters += 1;
            self.next_free = self.next_free.min(c);
        }
        Ok(())
    }

    /// Writes the FSInfo hints and flushes the disk.
    pub fn sync(&mut self) -> Result<()> {
        let mut info = [0u8; SECTOR_SIZE];
        self.read_sectors(self.fsinfo_sector, &mut info)?;
        if u32_at(&info, 0) == 0x4161_5252 && u32_at(&info, 484) == 0x6141_7272 {
            put_u32(&mut info, 488, self.free_clusters);
            put_u32(&mut info, 492, self.next_free);
            self.write_sectors(self.fsinfo_sector, &info)?;
        }
        Ok(self.dev.flush()?)
    }

    // ---- directories ----

    fn root_entry(&self) -> DirEntry {
        DirEntry {
            name: String::from("/"),
            attr: ATTR_DIRECTORY,
            cluster: self.root_cluster,
            size: 0,
            modified: DateTime {
                year: 1980,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            },
            short_name: [b' '; 11],
            slots: Vec::new(),
        }
    }

    /// Reads all entries of the directory starting at `dir_cluster`
    /// (without "." and ".."), plus the location of the end marker.
    fn read_dir_cluster(&mut self, dir_cluster: u32) -> Result<Vec<DirEntry>> {
        let mut entries = Vec::new();
        let mut buf = vec![0u8; self.bytes_per_cluster];
        // LFN state: collected UTF-16 parts, expected checksum, slots seen so far
        let mut lfn_parts: Vec<[u16; LFN_CHARS]> = Vec::new();
        let mut lfn_checksum_expected = 0u8;
        let mut lfn_slots: Vec<SlotLoc> = Vec::new();

        'clusters: for cluster in self.chain(dir_cluster)? {
            self.read_cluster(cluster, &mut buf)?;
            for index in 0..self.bytes_per_cluster / ENTRY_SIZE {
                let e = &buf[index * ENTRY_SIZE..(index + 1) * ENTRY_SIZE];
                let loc = SlotLoc { cluster, index };
                if e[0] == ENTRY_END {
                    break 'clusters;
                }
                if e[0] == ENTRY_FREE {
                    lfn_parts.clear();
                    lfn_slots.clear();
                    continue;
                }
                let attr = e[11];
                if attr & ATTR_LFN == ATTR_LFN {
                    let seq = e[0];
                    if seq & LFN_LAST != 0 {
                        let count = (seq & 0x1F) as usize;
                        lfn_parts = vec![[0xFFFF; LFN_CHARS]; count];
                        lfn_checksum_expected = e[13];
                        lfn_slots.clear();
                    }
                    let ord = (seq & 0x1F) as usize;
                    if ord == 0 || ord > lfn_parts.len() || e[13] != lfn_checksum_expected {
                        lfn_parts.clear(); // orphaned LFN entry: ignore it
                        lfn_slots.clear();
                        continue;
                    }
                    for (i, &off) in LFN_OFFSETS.iter().enumerate() {
                        lfn_parts[ord - 1][i] = u16_at(e, off);
                    }
                    lfn_slots.push(loc);
                    continue;
                }
                if attr & ATTR_VOLUME_ID != 0 {
                    lfn_parts.clear();
                    lfn_slots.clear();
                    continue;
                }
                let short_name: [u8; 11] = e[0..11].try_into().unwrap();
                if &short_name == b".          " || &short_name == b"..         " {
                    lfn_parts.clear();
                    lfn_slots.clear();
                    continue;
                }
                let long = if !lfn_parts.is_empty() && lfn_checksum(&short_name) == lfn_checksum_expected {
                    let units: Vec<u16> = lfn_parts
                        .iter()
                        .flatten()
                        .copied()
                        .take_while(|&u| u != 0x0000 && u != 0xFFFF)
                        .collect();
                    Some(char::decode_utf16(units).map(|r| r.unwrap_or('?')).collect::<String>())
                } else {
                    None
                };
                let mut slots = core::mem::take(&mut lfn_slots);
                if long.is_none() {
                    slots.clear();
                }
                slots.push(loc);
                lfn_parts.clear();
                let cluster_value = (u16_at(e, 20) as u32) << 16 | u16_at(e, 26) as u32;
                entries.push(DirEntry {
                    name: long.unwrap_or_else(|| format_short_name(&short_name, e[12])),
                    attr,
                    cluster: cluster_value,
                    size: u32_at(e, 28),
                    modified: decode_datetime(u16_at(e, 24), u16_at(e, 22)),
                    short_name,
                    slots,
                });
            }
        }
        Ok(entries)
    }

    fn find_in_dir(&mut self, dir_cluster: u32, name: &str) -> Result<Option<DirEntry>> {
        Ok(self
            .read_dir_cluster(dir_cluster)?
            .into_iter()
            .find(|e| names_equal(&e.name, name)))
    }

    /// Resolves an absolute, normalized path ("/", "/docs/a.txt").
    pub fn lookup(&mut self, path: &str) -> Result<DirEntry> {
        if !path.starts_with('/') {
            return Err(FsError::InvalidPath);
        }
        let mut entry = self.root_entry();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !entry.is_dir() {
                return Err(FsError::NotADirectory);
            }
            let dir = if entry.cluster == 0 {
                self.root_cluster
            } else {
                entry.cluster
            };
            entry = self.find_in_dir(dir, part)?.ok_or(FsError::NotFound)?;
        }
        Ok(entry)
    }

    pub fn read_dir(&mut self, path: &str) -> Result<Vec<DirEntry>> {
        let dir = self.lookup(path)?;
        if !dir.is_dir() {
            return Err(FsError::NotADirectory);
        }
        self.read_dir_cluster(dir.cluster)
    }

    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let entry = self.lookup(path)?;
        if entry.is_dir() {
            return Err(FsError::IsADirectory);
        }
        let mut data = Vec::with_capacity(entry.size as usize);
        let mut buf = vec![0u8; self.bytes_per_cluster];
        for cluster in self.chain(entry.cluster)? {
            if data.len() >= entry.size as usize {
                break;
            }
            self.read_cluster(cluster, &mut buf)?;
            let take = (entry.size as usize - data.len()).min(buf.len());
            data.extend_from_slice(&buf[..take]);
        }
        if data.len() != entry.size as usize {
            return Err(FsError::Corrupt("file shorter than its size"));
        }
        Ok(data)
    }

    /// Reads up to `buf.len()` bytes of a file starting at `offset`. Returns
    /// the number of bytes read (0 at or past the end of the file).
    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let entry = self.lookup(path)?;
        if entry.is_dir() {
            return Err(FsError::IsADirectory);
        }
        let size = entry.size as u64;
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let len = (size - offset).min(buf.len() as u64) as usize;
        let cluster_size = self.bytes_per_cluster as u64;
        let chain = self.chain(entry.cluster)?;
        let mut cluster_buf = vec![0u8; self.bytes_per_cluster];
        let mut done = 0;
        while done < len {
            let pos = offset + done as u64;
            let cluster = *chain
                .get((pos / cluster_size) as usize)
                .ok_or(FsError::Corrupt("file shorter than its size"))?;
            self.read_cluster(cluster, &mut cluster_buf)?;
            let within = (pos % cluster_size) as usize;
            let take = (self.bytes_per_cluster - within).min(len - done);
            buf[done..done + take].copy_from_slice(&cluster_buf[within..within + take]);
            done += take;
        }
        Ok(len)
    }

    // ---- writing directory entries ----

    fn write_slot(&mut self, loc: SlotLoc, bytes: &[u8; ENTRY_SIZE]) -> Result<()> {
        let mut buf = vec![0u8; self.bytes_per_cluster];
        self.read_cluster(loc.cluster, &mut buf)?;
        buf[loc.index * ENTRY_SIZE..(loc.index + 1) * ENTRY_SIZE].copy_from_slice(bytes);
        self.write_cluster(loc.cluster, &buf)
    }

    fn read_slot(&mut self, loc: SlotLoc) -> Result<[u8; ENTRY_SIZE]> {
        let mut buf = vec![0u8; self.bytes_per_cluster];
        self.read_cluster(loc.cluster, &mut buf)?;
        Ok(buf[loc.index * ENTRY_SIZE..(loc.index + 1) * ENTRY_SIZE]
            .try_into()
            .unwrap())
    }

    /// Finds `count` consecutive free slots in a directory, growing it by a
    /// zeroed cluster when needed.
    fn find_free_slots(&mut self, dir_cluster: u32, count: usize) -> Result<Vec<SlotLoc>> {
        let per_cluster = self.bytes_per_cluster / ENTRY_SIZE;
        let mut run: Vec<SlotLoc> = Vec::new();
        let mut buf = vec![0u8; self.bytes_per_cluster];
        let chain = self.chain(dir_cluster)?;
        for &cluster in &chain {
            self.read_cluster(cluster, &mut buf)?;
            for index in 0..per_cluster {
                let first = buf[index * ENTRY_SIZE];
                if first == ENTRY_FREE || first == ENTRY_END {
                    run.push(SlotLoc { cluster, index });
                    if run.len() == count {
                        return Ok(run);
                    }
                } else {
                    run.clear();
                }
            }
        }
        // Not enough room: append zeroed clusters (all their slots are free).
        // A non-empty `run` always ends at the last slot of the last cluster,
        // so it simply continues into the new cluster.
        let mut last = *chain.last().ok_or(FsError::Corrupt("directory without clusters"))?;
        while run.len() < count {
            let new = self.alloc_clusters(1, Some(last))?[0];
            self.write_cluster(new, &vec![0u8; self.bytes_per_cluster])?;
            for index in 0..per_cluster {
                run.push(SlotLoc { cluster: new, index });
                if run.len() == count {
                    break;
                }
            }
            last = new;
        }
        Ok(run)
    }

    fn short_entry_bytes(short: &[u8; 11], case: u8, attr: u8, cluster: u32, size: u32) -> [u8; ENTRY_SIZE] {
        let mut e = [0u8; ENTRY_SIZE];
        e[0..11].copy_from_slice(short);
        e[11] = attr;
        e[12] = case;
        let (date, time) = encode_datetime(now());
        put_u16(&mut e, 14, time); // created
        put_u16(&mut e, 16, date);
        put_u16(&mut e, 18, date); // accessed
        put_u16(&mut e, 20, (cluster >> 16) as u16);
        put_u16(&mut e, 22, time); // modified
        put_u16(&mut e, 24, date);
        put_u16(&mut e, 26, cluster as u16);
        put_u32(&mut e, 28, size);
        e
    }

    /// Creates a directory entry (with LFN slots if needed) in `dir_cluster`.
    fn create_entry(&mut self, dir_cluster: u32, name: &str, attr: u8, cluster: u32, size: u32) -> Result<()> {
        validate_name(name)?;
        let existing = self.read_dir_cluster(dir_cluster)?;
        if existing.iter().any(|e| names_equal(&e.name, name)) {
            return Err(FsError::AlreadyExists);
        }

        let (short, case, lfn) = match as_short_name(name) {
            Some((short, case)) if !existing.iter().any(|e| e.short_name == short) => (short, case, None),
            _ => {
                let mut n = 1;
                let alias = loop {
                    let alias = short_alias(name, n);
                    if !existing.iter().any(|e| e.short_name == alias) {
                        break alias;
                    }
                    n += 1;
                    if n > 999_999 {
                        return Err(FsError::NoSpace);
                    }
                };
                let units: Vec<u16> = name.encode_utf16().collect();
                (alias, 0, Some(units))
            }
        };

        let lfn_count = lfn.as_ref().map_or(0, |u| u.len().div_ceil(LFN_CHARS));
        let slots = self.find_free_slots(dir_cluster, lfn_count + 1)?;
        if let Some(units) = lfn {
            let checksum = lfn_checksum(&short);
            // LFN entries are stored in reverse order: the last part comes first
            for (slot_index, part) in (0..lfn_count).rev().enumerate() {
                let mut e = [0u8; ENTRY_SIZE];
                e[0] = (part + 1) as u8 | if part + 1 == lfn_count { LFN_LAST } else { 0 };
                e[11] = ATTR_LFN;
                e[13] = checksum;
                for (i, &off) in LFN_OFFSETS.iter().enumerate() {
                    let pos = part * LFN_CHARS + i;
                    let unit = match pos.cmp(&units.len()) {
                        core::cmp::Ordering::Less => units[pos],
                        core::cmp::Ordering::Equal => 0x0000,   // terminator
                        core::cmp::Ordering::Greater => 0xFFFF, // padding
                    };
                    put_u16(&mut e, off, unit);
                }
                self.write_slot(slots[slot_index], &e)?;
            }
        }
        let entry = Self::short_entry_bytes(&short, case, attr, cluster, size);
        self.write_slot(slots[lfn_count], &entry)
    }

    fn update_entry(&mut self, entry: &DirEntry, cluster: u32, size: u32) -> Result<()> {
        let loc = *entry.slots.last().ok_or(FsError::InvalidPath)?;
        let mut e = self.read_slot(loc)?;
        put_u16(&mut e, 20, (cluster >> 16) as u16);
        put_u16(&mut e, 26, cluster as u16);
        put_u32(&mut e, 28, size);
        let (date, time) = encode_datetime(now());
        put_u16(&mut e, 22, time);
        put_u16(&mut e, 24, date);
        put_u16(&mut e, 18, date);
        e[11] |= ATTR_ARCHIVE;
        self.write_slot(loc, &e)
    }

    fn split_parent(path: &str) -> Result<(&str, &str)> {
        let path = path.trim_end_matches('/');
        let idx = path.rfind('/').ok_or(FsError::InvalidPath)?;
        let parent = if idx == 0 { "/" } else { &path[..idx] };
        let name = &path[idx + 1..];
        if name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        Ok((parent, name))
    }

    fn parent_dir(&mut self, parent: &str) -> Result<u32> {
        let dir = self.lookup(parent)?;
        if !dir.is_dir() {
            return Err(FsError::NotADirectory);
        }
        Ok(if dir.cluster == 0 {
            self.root_cluster
        } else {
            dir.cluster
        })
    }

    /// Writes `data` into freshly allocated clusters, returns the first cluster (0 if empty).
    fn write_new_chain(&mut self, data: &[u8], after: Option<u32>) -> Result<u32> {
        let count = data.len().div_ceil(self.bytes_per_cluster);
        let clusters = self.alloc_clusters(count, after)?;
        let mut buf = vec![0u8; self.bytes_per_cluster];
        for (i, &cluster) in clusters.iter().enumerate() {
            let chunk = &data[i * self.bytes_per_cluster..((i + 1) * self.bytes_per_cluster).min(data.len())];
            buf[..chunk.len()].copy_from_slice(chunk);
            buf[chunk.len()..].fill(0);
            self.write_cluster(cluster, &buf)?;
        }
        Ok(clusters.first().copied().unwrap_or(0))
    }

    // ---- public write API ----

    /// Creates the file or replaces its contents.
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let size: u32 = data.len().try_into().map_err(|_| FsError::FileTooLarge)?;
        let (parent, name) = Self::split_parent(path)?;
        let dir = self.parent_dir(parent)?;
        match self.find_in_dir(dir, name)? {
            Some(entry) if entry.is_dir() => return Err(FsError::IsADirectory),
            Some(entry) => {
                let first = self.write_new_chain(data, None)?;
                self.update_entry(&entry, first, size)?;
                if entry.cluster != 0 {
                    self.free_chain(entry.cluster)?;
                }
            }
            None => {
                validate_name(name)?;
                let first = self.write_new_chain(data, None)?;
                if let Err(e) = self.create_entry(dir, name, ATTR_ARCHIVE, first, size) {
                    if first != 0 {
                        self.free_chain(first)?;
                    }
                    return Err(e);
                }
            }
        }
        self.sync()
    }

    /// Appends `data` to a file (creating it if necessary).
    pub fn append_file(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let entry = match self.lookup(path) {
            Ok(e) => e,
            Err(FsError::NotFound) => return self.write_file(path, data),
            Err(e) => return Err(e),
        };
        if entry.is_dir() {
            return Err(FsError::IsADirectory);
        }
        let new_size: u32 = (entry.size as usize + data.len())
            .try_into()
            .map_err(|_| FsError::FileTooLarge)?;
        let chain = self.chain(entry.cluster)?;
        let used_in_last = entry.size as usize % self.bytes_per_cluster;
        let mut rest = data;
        // Fill up the partially used last cluster first
        if let (Some(&last), true) = (chain.last(), used_in_last != 0) {
            let mut buf = vec![0u8; self.bytes_per_cluster];
            self.read_cluster(last, &mut buf)?;
            let take = (self.bytes_per_cluster - used_in_last).min(rest.len());
            buf[used_in_last..used_in_last + take].copy_from_slice(&rest[..take]);
            self.write_cluster(last, &buf)?;
            rest = &rest[take..];
        }
        let mut first = entry.cluster;
        if !rest.is_empty() {
            let new_first = self.write_new_chain(rest, chain.last().copied())?;
            if first == 0 {
                first = new_first;
            }
        }
        self.update_entry(&entry, first, new_size)?;
        self.sync()
    }

    /// Writes `data` into an existing file at `offset`: overwrites what is
    /// there and extends the file if needed. A gap between the end of the
    /// file and `offset` is filled with zeros.
    pub fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<()> {
        let entry = self.lookup(path)?;
        if entry.is_dir() {
            return Err(FsError::IsADirectory);
        }
        let size = entry.size as u64;
        if offset
            .checked_add(data.len() as u64)
            .is_none_or(|end| end > u32::MAX as u64)
        {
            return Err(FsError::FileTooLarge);
        }
        let mut filled = size;
        while filled < offset {
            let zeros = vec![0u8; (offset - filled).min(64 * 1024) as usize];
            self.append_file(path, &zeros)?;
            filled += zeros.len() as u64;
        }
        let overlap_end = (offset + data.len() as u64).min(size);
        if offset < overlap_end {
            let chain = self.chain(entry.cluster)?;
            let cluster_size = self.bytes_per_cluster as u64;
            let mut buf = vec![0u8; self.bytes_per_cluster];
            let mut pos = offset;
            while pos < overlap_end {
                let cluster = *chain
                    .get((pos / cluster_size) as usize)
                    .ok_or(FsError::Corrupt("file shorter than its size"))?;
                self.read_cluster(cluster, &mut buf)?;
                let within = (pos % cluster_size) as usize;
                let take = (self.bytes_per_cluster - within).min((overlap_end - pos) as usize);
                let src = (pos - offset) as usize;
                buf[within..within + take].copy_from_slice(&data[src..src + take]);
                self.write_cluster(cluster, &buf)?;
                pos += take as u64;
            }
            self.update_entry(&entry, entry.cluster, entry.size)?; // new modification time
        }
        let rest = &data[(overlap_end.max(offset) - offset) as usize..];
        if rest.is_empty() {
            self.sync()
        } else {
            self.append_file(path, rest)
        }
    }

    pub fn create_dir(&mut self, path: &str) -> Result<()> {
        let (parent, name) = Self::split_parent(path)?;
        validate_name(name)?;
        let parent_cluster = self.parent_dir(parent)?;
        if self.find_in_dir(parent_cluster, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let cluster = self.alloc_clusters(1, None)?[0];
        let mut buf = vec![0u8; self.bytes_per_cluster];
        // "." points to itself, ".." to the parent (0 means the root directory)
        let dot_dot = if parent_cluster == self.root_cluster {
            0
        } else {
            parent_cluster
        };
        buf[..ENTRY_SIZE].copy_from_slice(&Self::short_entry_bytes(b".          ", 0, ATTR_DIRECTORY, cluster, 0));
        buf[ENTRY_SIZE..2 * ENTRY_SIZE].copy_from_slice(&Self::short_entry_bytes(
            b"..         ",
            0,
            ATTR_DIRECTORY,
            dot_dot,
            0,
        ));
        self.write_cluster(cluster, &buf)?;
        if let Err(e) = self.create_entry(parent_cluster, name, ATTR_DIRECTORY, cluster, 0) {
            self.free_chain(cluster)?;
            return Err(e);
        }
        self.sync()
    }

    /// Deletes a file or an empty directory.
    pub fn remove(&mut self, path: &str) -> Result<()> {
        let entry = self.lookup(path)?;
        if entry.slots.is_empty() {
            return Err(FsError::InvalidPath); // the root directory
        }
        if entry.is_dir() && !self.read_dir_cluster(entry.cluster)?.is_empty() {
            return Err(FsError::DirectoryNotEmpty);
        }
        for &loc in &entry.slots {
            let mut e = self.read_slot(loc)?;
            e[0] = ENTRY_FREE;
            self.write_slot(loc, &e)?;
        }
        if entry.cluster != 0 {
            self.free_chain(entry.cluster)?;
        }
        self.sync()
    }

    /// Renames/moves a file or directory to a new path (same volume).
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let entry = self.lookup(from)?;
        if entry.slots.is_empty() {
            return Err(FsError::InvalidPath);
        }
        let (to_parent, to_name) = Self::split_parent(to)?;
        validate_name(to_name)?;
        let to_dir = self.parent_dir(to_parent)?;
        if self.find_in_dir(to_dir, to_name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        if entry.is_dir() && (to.starts_with(from) && to.as_bytes().get(from.len()) == Some(&b'/')) {
            return Err(FsError::InvalidPath); // cannot move a directory into itself
        }
        self.create_entry(to_dir, to_name, entry.attr, entry.cluster, entry.size)?;
        for &loc in &entry.slots {
            let mut e = self.read_slot(loc)?;
            e[0] = ENTRY_FREE;
            self.write_slot(loc, &e)?;
        }
        if entry.is_dir() {
            // Fix the ".." entry of the moved directory
            let dot_dot = if to_dir == self.root_cluster { 0 } else { to_dir };
            let loc = SlotLoc {
                cluster: entry.cluster,
                index: 1,
            };
            let mut e = self.read_slot(loc)?;
            put_u16(&mut e, 20, (dot_dot >> 16) as u16);
            put_u16(&mut e, 26, dot_dot as u16);
            self.write_slot(loc, &e)?;
        }
        self.sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::block::RamDisk;

    /// A freshly formatted 4 MiB FAT32 volume in RAM.
    fn ram_fs() -> FatFs<RamDisk> {
        let mut disk = RamDisk::new(8192);
        format(&mut disk, "TEST").unwrap();
        FatFs::mount(disk).unwrap()
    }

    #[test_case]
    fn write_at_overwrites_and_extends() {
        let mut fs = ram_fs();
        let mut expected: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
        fs.write_file("/w.bin", &expected).unwrap();
        // inside, across a cluster boundary (512 bytes), past the end, with a gap
        for (offset, len) in [(10u64, 5usize), (500, 30), (1490, 40), (1600, 20)] {
            let data: Vec<u8> = (0..len).map(|i| 200 - i as u8).collect();
            fs.write_at("/w.bin", offset, &data).unwrap();
            let end = offset as usize + len;
            if expected.len() < end {
                expected.resize(end, 0);
            }
            expected[offset as usize..end].copy_from_slice(&data);
            assert_eq!(fs.read_file("/w.bin").unwrap(), expected, "after writing at {}", offset);
        }
        assert_eq!(fs.lookup("/w.bin").unwrap().size, 1620);
        assert_eq!(fs.write_at("/missing", 0, b"x"), Err(FsError::NotFound));
    }

    #[test_case]
    fn read_at_offsets() {
        let mut fs = ram_fs();
        let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        fs.write_file("/r.bin", &data).unwrap();
        let mut buf = [0u8; 700];
        // inside one cluster, across clusters, at the end, past the end
        for offset in [0u64, 100, 500, 2600, 2999, 3000, 5000] {
            let n = fs.read_at("/r.bin", offset, &mut buf).unwrap();
            let expected = data.len().saturating_sub(offset as usize).min(buf.len());
            assert_eq!(n, expected, "offset {}", offset);
            let start = (offset as usize).min(data.len());
            assert_eq!(&buf[..n], &data[start..start + n]);
        }
        fs.create_dir("/d").unwrap();
        assert_eq!(fs.read_at("/d", 0, &mut buf), Err(FsError::IsADirectory));
        assert_eq!(fs.read_at("/none", 0, &mut buf), Err(FsError::NotFound));
    }

    #[test_case]
    fn short_name_rules() {
        assert_eq!(as_short_name("README.TXT").unwrap().0, *b"README  TXT");
        assert_eq!(
            as_short_name("readme.txt").unwrap(),
            (*b"README  TXT", CASE_LOWER_BASE | CASE_LOWER_EXT)
        );
        assert!(as_short_name("ReadMe.txt").is_none()); // mixed case
        assert!(as_short_name("verylongname.txt").is_none());
        assert!(as_short_name("a.b.c").is_none());
        assert!(as_short_name("über.txt").is_none());
        assert_eq!(&short_alias("verylongname.text", 1), b"VERYLO~1TEX");
        assert_eq!(&short_alias("my file.tar.gz", 2), b"MYFILE~2GZ ");
        assert_eq!(format_short_name(b"README  TXT", CASE_LOWER_EXT), "README.txt");
    }

    #[test_case]
    fn lfn_checksum_known_value() {
        // Reference values computed independently (Python) with the algorithm from the Microsoft FAT specification
        assert_eq!(lfn_checksum(b"VERYLO~1TEX"), 0xBE);
        assert_eq!(lfn_checksum(b"README  TXT"), 0x73);
    }

    #[test_case]
    fn invalid_names_rejected() {
        let mut fs = ram_fs();
        for bad in ["/a:b", "/a*", "/con?", "/x\\y", "/trailing.", "/."] {
            assert!(fs.write_file(bad, b"x").is_err(), "{} accepted", bad);
        }
    }

    #[test_case]
    fn format_and_mount_empty() {
        let fs = ram_fs();
        assert_eq!(fs.label(), "TEST");
        let s = fs.stats();
        assert_eq!(s.free_clusters, s.total_clusters - 1); // root directory
    }

    #[test_case]
    fn write_read_files_with_long_names() {
        let mut fs = ram_fs();
        fs.write_file("/hello.txt", b"hello").unwrap();
        fs.write_file("/A much longer file name.text", b"long").unwrap();
        fs.write_file("/Grüße.txt", "äöü".as_bytes()).unwrap();
        assert_eq!(fs.read_file("/hello.txt").unwrap(), b"hello");
        assert_eq!(fs.read_file("/a much LONGER file name.TEXT").unwrap(), b"long"); // case-insensitive
        assert_eq!(fs.read_file("/Grüße.txt").unwrap(), "äöü".as_bytes());
        let names: Vec<String> = fs.read_dir("/").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["hello.txt", "A much longer file name.text", "Grüße.txt"]);
    }

    #[test_case]
    fn overwrite_append_and_free_space() {
        let mut fs = ram_fs();
        let free0 = fs.stats().free_clusters;
        let big = vec![0x42u8; 10_000]; // 20 clusters of 512 bytes
        fs.write_file("/big.bin", &big).unwrap();
        assert_eq!(fs.stats().free_clusters, free0 - 20);
        fs.write_file("/big.bin", b"tiny").unwrap(); // shrinks: 19 clusters come back
        assert_eq!(fs.stats().free_clusters, free0 - 1);
        fs.append_file("/big.bin", &vec![b'x'; 1000]).unwrap();
        let data = fs.read_file("/big.bin").unwrap();
        assert_eq!(data.len(), 1004);
        assert_eq!(&data[..4], b"tiny");
        assert!(data[4..].iter().all(|&b| b == b'x'));
        fs.append_file("/new.log", b"line1\n").unwrap();
        fs.append_file("/new.log", b"line2\n").unwrap();
        assert_eq!(fs.read_file("/new.log").unwrap(), b"line1\nline2\n");
        fs.remove("/big.bin").unwrap();
        fs.remove("/new.log").unwrap();
        assert_eq!(fs.stats().free_clusters, free0);
    }

    #[test_case]
    fn directories() {
        let mut fs = ram_fs();
        fs.create_dir("/docs").unwrap();
        fs.create_dir("/docs/deep").unwrap();
        fs.write_file("/docs/deep/file.txt", b"deep").unwrap();
        assert_eq!(fs.read_file("/docs/deep/file.txt").unwrap(), b"deep");
        assert!(fs.lookup("/docs").unwrap().is_dir());
        assert_eq!(fs.create_dir("/docs"), Err(FsError::AlreadyExists));
        assert_eq!(fs.remove("/docs"), Err(FsError::DirectoryNotEmpty));
        assert_eq!(fs.read_file("/docs"), Err(FsError::IsADirectory));
        assert_eq!(fs.read_dir("/docs/deep/file.txt").err(), Some(FsError::NotADirectory));
        assert_eq!(fs.lookup("/nope/x").err(), Some(FsError::NotFound));
        fs.remove("/docs/deep/file.txt").unwrap();
        fs.remove("/docs/deep").unwrap();
        fs.remove("/docs").unwrap();
        assert!(fs.read_dir("/").unwrap().is_empty());
    }

    #[test_case]
    fn directory_grows_beyond_one_cluster() {
        let mut fs = ram_fs();
        // 512-byte clusters hold 16 entries; long names need 2-3 slots each
        for i in 0..40 {
            fs.write_file(&alloc::format!("/file number {:02}.txt", i), b"x")
                .unwrap();
        }
        let entries = fs.read_dir("/").unwrap();
        assert_eq!(entries.len(), 40);
        assert!(entries.iter().any(|e| e.name == "file number 39.txt"));
        // short aliases must be unique
        let mut aliases: Vec<String> = entries.iter().map(|e| e.short_name()).collect();
        aliases.sort();
        aliases.dedup();
        assert_eq!(aliases.len(), 40);
    }

    #[test_case]
    fn rename_and_move() {
        let mut fs = ram_fs();
        fs.create_dir("/a").unwrap();
        fs.create_dir("/b").unwrap();
        fs.write_file("/a/note.txt", b"note").unwrap();
        fs.rename("/a/note.txt", "/b/renamed note.txt").unwrap();
        assert_eq!(fs.lookup("/a/note.txt").err(), Some(FsError::NotFound));
        assert_eq!(fs.read_file("/b/renamed note.txt").unwrap(), b"note");
        fs.rename("/b", "/a/b").unwrap();
        assert_eq!(fs.read_file("/a/b/renamed note.txt").unwrap(), b"note");
        assert_eq!(fs.rename("/a", "/a/b/inside"), Err(FsError::InvalidPath));
    }

    #[test_case]
    fn data_survives_remount() {
        let mut fs = ram_fs();
        fs.create_dir("/keep").unwrap();
        fs.write_file("/keep/data.bin", &[1, 2, 3]).unwrap();
        let free = fs.stats().free_clusters;
        let disk = fs.into_device();
        let mut fs = FatFs::mount(disk).unwrap();
        assert_eq!(fs.read_file("/keep/data.bin").unwrap(), [1, 2, 3]);
        assert_eq!(fs.stats().free_clusters, free);
    }

    #[test_case]
    fn disk_full_is_reported() {
        let mut fs = ram_fs();
        let free = fs.stats().free_clusters as usize;
        let too_big = vec![0u8; (free + 1) * 512];
        assert_eq!(fs.write_file("/huge", &too_big), Err(FsError::NoSpace));
        assert_eq!(fs.stats().free_clusters as usize, free, "failed write leaked clusters");
        assert_eq!(fs.lookup("/huge").err(), Some(FsError::NotFound));
    }
}
