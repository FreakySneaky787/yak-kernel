//! Files and directories on top of the file system calls. Paths may be
//! absolute or relative to the process's current directory.

use crate::sys::{self, DirEntryInfo, Stat};
use core::fmt;

/// A failed system call: the (positive) errno value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i64);

pub const ENOENT: i64 = 2;
pub const EBADF: i64 = 9;
pub const EISDIR: i64 = 21;
pub const ENOTDIR: i64 = 20;
pub const EINVAL: i64 = 22;
pub const EMFILE: i64 = 24;
pub const ERANGE: i64 = 34;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self.0 {
            ENOENT => "no such file or directory",
            3 => "no such process",
            4 => "interrupted",
            5 => "input/output error",
            7 => "argument list too long",
            8 => "not an executable (ELF) file",
            10 => "no such child process",
            12 => "out of memory",
            EBADF => "bad file descriptor",
            14 => "bad address",
            17 => "already exists",
            ENOTDIR => "not a directory",
            EISDIR => "is a directory",
            EINVAL => "invalid argument",
            EMFILE => "too many open files",
            27 => "file too large",
            29 => "not seekable",
            28 => "no space left on the disk",
            ERANGE => "buffer too small",
            36 => "path too long",
            32 => "broken pipe",
            39 => "directory not empty",
            _ => return write!(f, "error {}", self.0),
        };
        f.write_str(text)
    }
}

fn check(ret: i64) -> Result<u64, Error> {
    if ret < 0 { Err(Error(-ret)) } else { Ok(ret as u64) }
}

/// An open file; closed when dropped.
pub struct File {
    fd: u64,
}

impl File {
    pub fn open(path: &str) -> Result<File, Error> {
        check(sys::open(path, sys::O_READ)).map(|fd| File { fd })
    }

    /// Creates the file or empties an existing one; writes append.
    pub fn create(path: &str) -> Result<File, Error> {
        check(sys::open(path, sys::O_WRITE | sys::O_CREATE | sys::O_TRUNCATE)).map(|fd| File { fd })
    }

    pub fn open_with(path: &str, flags: u64) -> Result<File, Error> {
        check(sys::open(path, flags)).map(|fd| File { fd })
    }

    pub fn fd(&self) -> u64 {
        self.fd
    }

    /// Reads the next bytes; 0 = end of file.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        check(sys::read(self.fd, buf)).map(|n| n as usize)
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, Error> {
        check(sys::write(self.fd, data)).map(|n| n as usize)
    }

    /// Moves the position (`sys::SEEK_SET` / `SEEK_CUR` / `SEEK_END`); returns the new offset.
    pub fn seek(&mut self, offset: i64, whence: u64) -> Result<u64, Error> {
        check(sys::seek(self.fd, offset, whence))
    }
}

impl Drop for File {
    fn drop(&mut self) {
        sys::close(self.fd);
    }
}

/// An open directory; iterating yields its entries.
pub struct Dir {
    fd: u64,
}

pub struct Entry {
    info: DirEntryInfo,
}

impl Entry {
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.info.name[..self.info.name_len as usize]).unwrap_or("?")
    }

    pub fn stat(&self) -> &Stat {
        &self.info.stat
    }

    pub fn is_dir(&self) -> bool {
        self.info.stat.kind == sys::KIND_DIR
    }
}

impl Dir {
    pub fn open(path: &str) -> Result<Dir, Error> {
        check(sys::open(path, sys::O_READ)).map(|fd| Dir { fd })
    }
}

impl Iterator for Dir {
    type Item = Result<Entry, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut info = DirEntryInfo {
            stat: Stat::default(),
            name_len: 0,
            _pad: 0,
            name: [0; 256],
        };
        match sys::readdir(self.fd, &mut info) {
            0 => None,
            1 => Some(Ok(Entry { info })),
            e => Some(Err(Error(-e))),
        }
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        sys::close(self.fd);
    }
}

pub fn stat(path: &str) -> Result<Stat, Error> {
    let mut st = Stat::default();
    check(sys::stat(path, &mut st)).map(|_| st)
}

/// Renames or moves a file or directory.
pub fn rename(from: &str, to: &str) -> Result<(), Error> {
    check(sys::rename(from, to)).map(|_| ())
}

/// Changes the current directory.
pub fn chdir(path: &str) -> Result<(), Error> {
    check(sys::chdir(path)).map(|_| ())
}

/// The current directory, written into `buf`.
pub fn cwd(buf: &mut [u8]) -> Result<&str, Error> {
    let n = check(sys::getcwd(buf))? as usize;
    core::str::from_utf8(&buf[..n]).map_err(|_| Error(EINVAL))
}

/// "2026-10-24 18:26:05" for a `Stat::modified` timestamp.
pub struct Timestamp(pub [u16; 6]);

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let t = self.0;
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t[0], t[1], t[2], t[3], t[4], t[5]
        )
    }
}
