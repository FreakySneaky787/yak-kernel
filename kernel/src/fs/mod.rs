//! File system layer: the mounted FAT32 volume on the system disk and path
//! helpers. Paths given to the FS are absolute and normalized ("/a/b");
//! the shell keeps a current directory and normalizes user input with
//! `normalize`.

pub mod fat32;

use crate::drivers::block::{BlockCache, BlockDevice, BlockError};
use alloc::string::String;
use alloc::vec::Vec;
use fat32::{FatFs, FsError};
use spin::Mutex;

/// Block device adapter for the global system disk (`drivers::DISK`).
pub struct SystemDisk;

impl BlockDevice for SystemDisk {
    fn sector_count(&self) -> u64 {
        crate::drivers::DISK.lock().as_ref().map_or(0, |d| d.sector_count())
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        crate::drivers::DISK
            .lock()
            .as_mut()
            .ok_or(BlockError::Device)?
            .read(lba, buf)
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        crate::drivers::DISK
            .lock()
            .as_mut()
            .ok_or(BlockError::Device)?
            .write(lba, buf)
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        crate::drivers::DISK.lock().as_mut().ok_or(BlockError::Device)?.flush()
    }
}

/// What the root file system runs on: the system disk behind a
/// write-through sector cache.
pub type RootDisk = BlockCache<SystemDisk>;

/// Sectors kept in the cache (512 KiB).
pub const CACHE_SECTORS: usize = 1024;

/// The mounted root file system. Like `DISK`, held with interrupts enabled
/// during I/O; never use it from interrupt handlers.
pub static ROOT: Mutex<Option<FatFs<RootDisk>>> = Mutex::new(None);

/// Mounts the FAT32 file system of the system disk as the root.
pub fn mount_root() -> Result<(), FsError> {
    if crate::drivers::DISK.lock().is_none() {
        return Err(FsError::NotMounted);
    }
    let fs = FatFs::mount(BlockCache::new(SystemDisk, CACHE_SECTORS))?;
    *ROOT.lock() = Some(fs);
    Ok(())
}

/// Runs `f` with the root file system.
pub fn with_root<R>(f: impl FnOnce(&mut FatFs<RootDisk>) -> Result<R, FsError>) -> Result<R, FsError> {
    f(ROOT.lock().as_mut().ok_or(FsError::NotMounted)?)
}

/// (hits, misses, cached sectors) of the root file system's cache.
pub fn cache_stats() -> Option<(u64, u64, usize)> {
    ROOT.lock()
        .as_ref()
        .map(|fs| (fs.device().hits, fs.device().misses, fs.device().cached_sectors()))
}

/// Turns `path` (absolute or relative to `cwd`) into an absolute path
/// without ".", ".." or duplicate slashes. ".." at the root stays at the root.
pub fn normalize(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let start = if path.starts_with('/') { "" } else { cwd };
    for part in start.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = String::new();
    for p in &parts {
        out.push('/');
        out.push_str(p);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::block::BlockDevice;

    #[test_case]
    fn normalize_paths() {
        assert_eq!(normalize("/", "docs"), "/docs");
        assert_eq!(normalize("/docs", "../etc//motd"), "/etc/motd");
        assert_eq!(normalize("/docs", "/a/./b/"), "/a/b");
        assert_eq!(normalize("/", ".."), "/");
        assert_eq!(normalize("/a/b", "../.."), "/");
        assert_eq!(normalize("/a", ""), "/a");
    }

    #[test_case]
    fn root_is_mounted_and_reads_rootfs_files() {
        // The test disk is built from rootfs/ by scripts/mkdisk.sh (mtools)
        let expected = include_str!("../../../rootfs/README.txt");
        let data = with_root(|fs| fs.read_file("/README.txt")).expect("cannot read /README.txt");
        assert_eq!(core::str::from_utf8(&data).unwrap(), expected);
        let hello = with_root(|fs| fs.read_file("/docs/hello.txt")).unwrap();
        assert_eq!(hello, include_bytes!("../../../rootfs/docs/hello.txt"));
        let label = with_root(|fs| Ok(String::from(fs.label()))).unwrap();
        assert_eq!(label, "KERNELTRY");
    }

    #[test_case]
    fn write_on_real_disk_and_remount() {
        with_root(|fs| {
            fs.create_dir("/from kernel")?;
            fs.write_file("/from kernel/written by KernelTry.txt", b"persistent data\n")?;
            fs.append_file("/from kernel/written by KernelTry.txt", b"second line\n")
        })
        .unwrap();
        // Mount a second instance on the same disk: everything must be on disk
        let mut fresh = FatFs::mount(SystemDisk).unwrap();
        let data = fresh.read_file("/From Kernel/WRITTEN BY KERNELTRY.TXT").unwrap();
        assert_eq!(data, b"persistent data\nsecond line\n");
        assert!(SystemDisk.sector_count() > 0);
    }

    #[test_case]
    fn big_file_on_real_disk() {
        // 300 KB = 600 clusters, many multi-sector disk commands
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        with_root(|fs| fs.write_file("/big.bin", &data)).unwrap();
        let back = with_root(|fs| fs.read_file("/big.bin")).unwrap();
        assert!(back == data, "big file differs after reading back");
    }
}
