//! Files for programs (chapter 22): file descriptors, and the system calls `open`, `read`, `write`, `seek` and
//! `close` on them.
//!
//! A program cannot be given the kernel's idea of a file - a path, a place on the disk - and it must not be: it could
//! name any file of the volume, or any number it likes. It is given a *file descriptor*: a small number that means
//! something to the kernel only, an index into a table that belongs to this one program. 0, 1 and 2 are the console
//! (nothing to read yet; text to write); `open` hands out 3 and up, the lowest free number first. When the program
//! ends, its table goes with it: files it did not close are closed.
//!
//! What a call may do is what the file system can do: files can be read anywhere, but written only at their end
//! (chapter 18's `append`), and a directory cannot be opened at all - there is no `readdir` yet. All errors are
//! negative numbers with Linux's values.

use crate::user;
use crate::{fat32::FsError, fs, interrupts, task};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use spin::Mutex;

/// Flags of `open`.
pub const READ: u64 = 1;
pub const WRITE: u64 = 2;
/// Make the file if it is not there (needs WRITE).
pub const CREATE: u64 = 4;
/// Throw away what is in the file (needs WRITE).
pub const TRUNCATE: u64 = 8;
/// Start writing at the end of the file, not at its start (needs WRITE).
pub const APPEND: u64 = 16;
const ALL_FLAGS: u64 = READ | WRITE | CREATE | TRUNCATE | APPEND;

/// Numbers below this are the console; a program has room for `FILES` open files.
const FIRST_FILE: u64 = 3;
const FILES: usize = 5;

/// The longest path `open` takes, and the most bytes one `read` or `write` moves: a program that asks for more gets a
/// *short count* and asks again. Nothing a program says may make the kernel allocate more than this.
pub const MAX_PATH: u64 = 1024;
pub const MAX_IO: u64 = 64 * 1024;

// The errors, with the numbers of Linux (the call returns the negative number).
const EIO: i64 = 5;
const EBADF: i64 = 9;
pub const EFAULT: i64 = 14;
const EEXIST: i64 = 17;
const ENOTDIR: i64 = 20;
const EISDIR: i64 = 21;
const EINVAL: i64 = 22;
const EMFILE: i64 = 24;
const ENOSPC: i64 = 28;
const ESPIPE: i64 = 29;
const ENAMETOOLONG: i64 = 36;
const ENOTEMPTY: i64 = 39;
const ENOENT: i64 = 2;

/// What a call returns: a number, or the error's number (positive here, negative in the program's RAX).
type Outcome = Result<u64, i64>;

/// What the kernel remembers about an open file.
#[derive(Clone)]
struct Handle {
    path: String,
    /// Where the next `read` starts - and the next `write`, which must be the end of the file.
    offset: u64,
    readable: bool,
    writable: bool,
}

type Table = [Option<Handle>; FILES];

/// The open files of every program, by the id of its thread. A table is made by the first call that needs it and
/// removed by `forget` when the program has ended.
static TABLES: Mutex<BTreeMap<task::Id, Table>> = Mutex::new(BTreeMap::new());

/// Runs `f` on the table of the program that is running.
fn with_table<T>(f: impl FnOnce(&mut Table) -> T) -> T {
    let me = task::current();
    interrupts::without_interrupts(|| f(TABLES.lock().entry(me).or_default()))
}

/// The program has ended: its files are closed.
pub fn forget(thread: task::Id) {
    interrupts::without_interrupts(|| TABLES.lock().remove(&thread));
}

/// The number a call leaves in RAX: the result, or minus the error.
pub fn to_register(outcome: Outcome) -> u64 {
    match outcome {
        Ok(value) => value,
        Err(error) => (-error) as u64,
    }
}

fn errno(error: FsError) -> i64 {
    match error {
        FsError::NotFound => ENOENT,
        FsError::NotADirectory => ENOTDIR,
        FsError::IsADirectory => EISDIR,
        // a name FAT cannot store (this volume has no long names written): the program asked for something invalid
        FsError::BadName => EINVAL,
        FsError::Exists => EEXIST,
        FsError::NotEmpty => ENOTEMPTY,
        FsError::NoSpace => ENOSPC,
        FsError::Disk(_) | FsError::NotFat32 | FsError::Corrupt(_) => EIO,
    }
}

/// The table slot of a file descriptor, or EBADF: the console and numbers beyond the table are not files.
fn slot(fd: u64) -> Result<usize, i64> {
    match fd.checked_sub(FIRST_FILE) {
        Some(i) if i < FILES as u64 => Ok(i as usize),
        _ => Err(EBADF),
    }
}

/// A copy of what the table says about `fd`. Nothing is held while the disk is read: the disk may take a while.
fn handle(fd: u64) -> Result<(usize, Handle), i64> {
    let slot = slot(fd)?;
    let handle = with_table(|table| table[slot].clone()).ok_or(EBADF)?;
    Ok((slot, handle))
}

/// The path in the program's memory. It is checked like everything a program hands over: not too long, in memory the
/// program may read, text.
fn user_path(address: u64, length: u64) -> Result<String, i64> {
    if length > MAX_PATH {
        return Err(ENAMETOOLONG);
    }
    let bytes = user::user_bytes(address, length).ok_or(EFAULT)?;
    let text = core::str::from_utf8(bytes).map_err(|_| EINVAL)?;
    match text {
        "" => Err(ENOENT),
        text if text.starts_with('/') => Ok(String::from(text)),
        text => Ok(alloc::format!("/{}", text)), // there is no current directory yet: everything starts at the root
    }
}

/// `open(path, length, flags)`: the file descriptor, or an error.
pub fn open(address: u64, length: u64, flags: u64) -> Outcome {
    let valid = flags & !ALL_FLAGS == 0
        && flags & (READ | WRITE) != 0
        && (flags & (CREATE | TRUNCATE | APPEND) == 0 || flags & WRITE != 0);
    if !valid {
        return Err(EINVAL);
    }
    let path = user_path(address, length)?;
    // a free slot first: a program that cannot use the file gets no file made
    let slot = with_table(|table| table.iter().position(Option::is_none)).ok_or(EMFILE)?;
    let size = {
        let mut guard = fs::FS.lock();
        let fs = guard.as_mut().ok_or(EIO)?;
        match fs.lookup(&path) {
            Ok(entry) if entry.is_dir => return Err(EISDIR),
            Ok(_) if flags & TRUNCATE != 0 => {
                fs.write_file(&path, b"").map_err(errno)?;
                0
            }
            Ok(entry) => entry.size as u64,
            Err(FsError::NotFound) if flags & CREATE != 0 => {
                fs.write_file(&path, b"").map_err(errno)?;
                0
            }
            Err(error) => return Err(errno(error)),
        }
    };
    let handle = Handle {
        path,
        offset: if flags & APPEND != 0 { size } else { 0 },
        readable: flags & READ != 0,
        writable: flags & WRITE != 0,
    };
    with_table(|table| table[slot] = Some(handle));
    Ok(FIRST_FILE + slot as u64)
}

pub fn close(fd: u64) -> Outcome {
    let slot = slot(fd)?;
    with_table(|table| table[slot].take()).map(|_| 0).ok_or(EBADF)
}

/// Whether the last `read` ran with interrupts on - the test looks (see `user::syscall`).
#[cfg(test)]
static INTERRUPTS_ON_IN_READ: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// `read(fd, buffer, length)`: how many bytes were put into the buffer - fewer than asked (at most `MAX_IO`, at most
/// what is left of the file), 0 at the end of the file.
pub fn read(fd: u64, address: u64, length: u64) -> Outcome {
    #[cfg(test)]
    INTERRUPTS_ON_IN_READ.store(interrupts::enabled(), core::sync::atomic::Ordering::SeqCst);
    if fd == 0 {
        return Ok(0); // the console has no input for programs yet: always the end
    }
    let (slot, handle) = handle(fd)?;
    if !handle.readable {
        return Err(EBADF);
    }
    let length = length.min(MAX_IO);
    // the buffer is checked *before* anything happens: a call that fails changes nothing - the next `read` must
    // still start where the file offset says (bug #13 of the reference kernel)
    let buffer = user::user_bytes_mut(address, length).ok_or(EFAULT)?;
    let mut bytes = vec![0u8; length as usize];
    let count = {
        let mut guard = fs::FS.lock();
        let fs = guard.as_mut().ok_or(EIO)?;
        fs.read_at(&handle.path, handle.offset, &mut bytes).map_err(errno)?
    };
    buffer[..count].copy_from_slice(&bytes[..count]);
    with_table(|table| {
        if let Some(open) = &mut table[slot] {
            open.offset += count as u64;
        }
    });
    Ok(count as u64)
}

/// `write(fd, bytes, length)`: how many bytes were taken. 1 and 2 are the console; a file grows at its end only.
pub fn write(fd: u64, address: u64, length: u64) -> Outcome {
    let length = length.min(MAX_IO);
    let bytes = user::user_bytes(address, length).ok_or(EFAULT)?;
    if fd == 1 || fd == 2 {
        user::console_write(bytes);
        return Ok(length);
    }
    let (slot, handle) = handle(fd)?;
    if !handle.writable {
        return Err(EBADF);
    }
    if bytes.is_empty() {
        return Ok(0);
    }
    {
        let mut guard = fs::FS.lock();
        let fs = guard.as_mut().ok_or(EIO)?;
        let size = fs.lookup(&handle.path).map_err(errno)?.size as u64;
        if handle.offset != size {
            return Err(EINVAL); // FAT32 here can only append: the program must be at the end of the file
        }
        fs.append(&handle.path, bytes).map_err(errno)?;
    }
    with_table(|table| {
        if let Some(open) = &mut table[slot] {
            open.offset += length;
        }
    });
    Ok(length)
}

/// `seek(fd, offset, whence)`: moves the offset - to `offset` (whence 0), by `offset` (1), or from the end of the
/// file (2) - and returns the new one. Behind the end is allowed: a `read` there finds nothing.
pub fn seek(fd: u64, offset: u64, whence: u64) -> Outcome {
    if fd < FIRST_FILE {
        return Err(ESPIPE); // the console has no places
    }
    let (slot, handle) = handle(fd)?;
    let base = match whence {
        0 => 0,
        1 => handle.offset as i64,
        2 => {
            let mut guard = fs::FS.lock();
            let fs = guard.as_mut().ok_or(EIO)?;
            fs.lookup(&handle.path).map_err(errno)?.size as i64
        }
        _ => return Err(EINVAL),
    };
    let new = base.checked_add(offset as i64).filter(|&new| new >= 0).ok_or(EINVAL)? as u64;
    with_table(|table| {
        if let Some(open) = &mut table[slot] {
            open.offset = new;
        }
    });
    Ok(new)
}

/// How many programs have a table - the tests look at it.
#[cfg(test)]
pub fn tables() -> usize {
    interrupts::without_interrupts(|| TABLES.lock().len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::{TestSegment, build_test_elf};
    use crate::mem::PAGE_SIZE;
    use crate::user::{self, Exit};
    use alloc::vec::Vec;
    use core::arch::global_asm;

    // The tests drive the system calls from ring 3 with a *script*: a program that reads a table of calls from its
    // data page - four numbers each: the call, three arguments - makes them one after the other, and writes every
    // result over the number of its call. The kernel fills the table, starts the program, and reads the results
    // from the program's memory when it has ended. A call that is not in the list is the number 1000: `ud2`.
    global_asm!(
        ".section .rodata.user_programs, \"a\"",
        ".global user_script, user_script_end",
        "user_script:",
        "mov rbx, 0x401000",
        "2: mov rax, [rbx]",
        "test rax, rax",
        "jz 3f",
        "cmp rax, 1000",
        "je 4f",
        "mov rdi, [rbx + 8]",
        "mov rsi, [rbx + 16]",
        "mov rdx, [rbx + 24]",
        "int 0x80",
        "mov [rbx], rax",
        "add rbx, 32",
        "jmp 2b",
        "3: mov eax, 2",
        "xor edi, edi",
        "int 0x80",
        "4: ud2",
        "user_script_end:",
        ".text",
    );

    const CODE: u64 = 0x40_0000;
    const DATA: u64 = 0x40_1000;
    /// The table of calls is at DATA, text for the calls (paths) at STRINGS, a buffer of 128 KiB at BUFFER.
    const STRINGS: u64 = DATA + 0x800;
    const BUFFER: u64 = DATA + 0x1000;
    const BUFFER_SIZE: usize = 0x2_0000;

    const SYS_WRITE: u64 = 1;
    const SYS_READ: u64 = 3;
    const SYS_OPEN: u64 = 4;
    const SYS_CLOSE: u64 = 5;
    const SYS_SEEK: u64 = 6;
    /// Not a system call: the script program executes `ud2` here.
    const KILL: u64 = 1000;
    /// A call's error as the program sees it.
    const fn err(errno: i64) -> i64 {
        -errno
    }

    struct Script {
        calls: Vec<[u64; 4]>,
        strings: Vec<u8>,
    }

    struct Done {
        exit: Exit,
        /// What the calls returned, in order.
        results: Vec<i64>,
        /// The program's buffer when it ended.
        buffer: Vec<u8>,
    }

    impl Script {
        fn new() -> Script {
            Script {
                calls: Vec::new(),
                strings: Vec::new(),
            }
        }

        /// Puts `text` into the program's memory; returns its address.
        fn text(&mut self, text: &[u8]) -> u64 {
            let address = STRINGS + self.strings.len() as u64;
            self.strings.extend_from_slice(text);
            address
        }

        fn call(&mut self, number: u64, a: u64, b: u64, c: u64) -> &mut Script {
            self.calls.push([number, a, b, c]);
            self
        }

        /// `open` of a path given as text.
        fn open(&mut self, path: &str, flags: u64) -> &mut Script {
            let address = self.text(path.as_bytes());
            self.call(SYS_OPEN, address, path.len() as u64, flags)
        }

        fn run(&self) -> Done {
            assert!(self.calls.len() < 60 && self.strings.len() < 0x400);
            let mut data = Vec::new();
            for call in &self.calls {
                call.iter().for_each(|n| data.extend_from_slice(&n.to_le_bytes()));
            }
            data.resize(0x800, 0); // the number 0 ends the table
            data.extend_from_slice(&self.strings);
            let code = unsafe {
                unsafe extern "C" {
                    static user_script: u8;
                    static user_script_end: u8;
                }
                let start = &raw const user_script as usize;
                let end = &raw const user_script_end as usize;
                core::slice::from_raw_parts(start as *const u8, end - start)
            };
            let image = build_test_elf(
                CODE,
                &[
                    TestSegment {
                        vaddr: CODE,
                        flags: 5,
                        bytes: code,
                        memsz: code.len() as u64,
                    },
                    TestSegment {
                        vaddr: DATA,
                        flags: 6,
                        bytes: &data,
                        memsz: PAGE_SIZE + BUFFER_SIZE as u64,
                    },
                ],
            );
            let count = self.calls.len();
            let (exit, (results, buffer)) = user::spawn(&image).unwrap().join_with(|space| {
                let mut table = alloc::vec![0u8; 32 * count];
                space.read(DATA, &mut table).unwrap();
                let results = table
                    .chunks(32)
                    .map(|record| i64::from_le_bytes(record[..8].try_into().unwrap()))
                    .collect();
                let mut buffer = alloc::vec![0u8; BUFFER_SIZE];
                space.read(BUFFER, &mut buffer).unwrap();
                (results, buffer)
            });
            Done { exit, results, buffer }
        }
    }

    fn disk_file(path: &str) -> Result<alloc::vec::Vec<u8>, FsError> {
        fs::FS.lock().as_mut().unwrap().read_file(path)
    }

    #[test_case]
    fn a_file_is_opened_read_in_pieces_and_closed() {
        let mut s = Script::new();
        s.open("/hello.txt", READ)
            .call(SYS_READ, 3, BUFFER, 5) // "Hello"
            .call(SYS_READ, 3, BUFFER + 5, 5) // " from": the offset moved on
            .call(SYS_SEEK, 3, 0, 0) // back to the start
            .call(SYS_READ, 3, BUFFER + 10, 100) // all of it - 21 bytes, no more than there are
            .call(SYS_READ, 3, BUFFER + 40, 10) // at the end: nothing
            .call(SYS_SEEK, 3, (-3i64) as u64, 2) // 3 bytes before the end
            .call(SYS_READ, 3, BUFFER + 50, 10)
            .call(SYS_SEEK, 3, 1, 1) // 1 further from where it is: behind the end
            .call(SYS_READ, 3, BUFFER + 60, 10)
            .call(SYS_CLOSE, 3, 0, 0)
            .call(SYS_CLOSE, 3, 0, 0); // twice
        let done = s.run();
        assert_eq!(done.exit, Exit::Code(0));
        assert_eq!(done.results, [3, 5, 5, 0, 21, 0, 18, 3, 22, 0, 0, err(EBADF)]);
        assert_eq!(&done.buffer[..10], b"Hello from");
        assert_eq!(&done.buffer[10..31], b"Hello from the disk!\n");
        assert_eq!(&done.buffer[50..53], b"k!\n");
    }

    #[test_case]
    fn descriptors_are_handed_out_lowest_first_until_the_table_is_full() {
        let mut s = Script::new();
        for _ in 0..5 {
            s.open("/hello.txt", READ);
        }
        s.open("/hello.txt", READ) // the table has room for 5
            .call(SYS_CLOSE, 5, 0, 0)
            .open("/hello.txt", READ); // the lowest free one: 5 again
        let done = s.run();
        assert_eq!(done.results, [3, 4, 5, 6, 7, err(EMFILE), 0, 5]);
    }

    #[test_case]
    fn bad_calls_get_an_error_and_change_nothing() {
        let mut s = Script::new();
        let kernel = 0xFFFF_FFFF_8000_0000u64;
        let not_utf8 = s.text(&[0xFF, 0xFE]);
        let long = s.text(&[b'a'; 4]);
        s.open("/nothing.txt", READ) // 0
            .open("/docs", READ) // 1: a directory
            .call(SYS_OPEN, long, 0, READ) // 2: no name
            .call(SYS_OPEN, long, MAX_PATH + 1, READ) // 3: a name too long to look at
            .call(SYS_OPEN, kernel, 8, READ) // 4: a path in the kernel
            .call(SYS_OPEN, not_utf8, 2, READ) // 5: not text
            .open("/hello.txt", 0) // 6: neither READ nor WRITE
            .open("/hello.txt", READ | CREATE) // 7: CREATE without WRITE
            .open("/hello.txt", READ | 32) // 8: a flag nobody knows
            .open("/hello.txt/inside", READ) // 9: a file is no directory
            .open("hello.txt", READ) // 10: no `/` in front: from the root - this one works, as fd 3
            // the descriptor 3 is open; failing reads must not move its offset
            .call(SYS_READ, 3, kernel, 5) // 11: a buffer in the kernel
            .call(SYS_READ, 3, CODE, 5) // 12: a buffer the program cannot write
            .call(SYS_READ, 3, u64::MAX - 2, 10) // 13: wraps around
            .call(SYS_READ, 3, 0, 5) // 14: nothing there
            .call(SYS_READ, 3, BUFFER, 5) // 15: and now a good one - still at the start of the file
            .call(SYS_READ, 3, BUFFER + 8, u64::MAX) // 16: a huge length is a short count: what is left of the file
            .call(SYS_READ, 99, BUFFER, 5) // 17: a number nobody has
            .call(SYS_READ, 1, BUFFER, 5) // 18: the console is not for reading from files
            .call(SYS_READ, 0, BUFFER, 5) // 19: but it is an input that has ended
            .call(SYS_READ, 4, BUFFER, 5) // 20: not open
            .call(SYS_WRITE, 1, kernel, 5) // 21
            .call(SYS_WRITE, 99, BUFFER, 5) // 22
            .call(SYS_WRITE, 3, BUFFER, 5) // 23: opened for reading only
            .call(SYS_SEEK, 1, 0, 0) // 24: the console has no places
            .call(SYS_SEEK, 99, 0, 0) // 25
            .call(SYS_SEEK, 3, 0, 9) // 26: no such whence
            .call(SYS_SEEK, 3, (-1i64) as u64, 0) // 27: before the start
            .call(SYS_SEEK, 3, 1000, 0) // 28: behind the end is fine ...
            .call(SYS_READ, 3, BUFFER + 100, 5) // 29: ... and finds nothing
            .call(99, 0, 0, 0) // 30: no such call
            .call(SYS_CLOSE, 99, 0, 0) // 31
            .call(SYS_CLOSE, 1, 0, 0); // 32: the console is not closed
        let done = s.run();
        assert_eq!(done.exit, Exit::Code(0));
        let expected = [
            err(ENOENT),
            err(EISDIR),
            err(ENOENT),
            err(ENAMETOOLONG),
            err(EFAULT),
            err(EINVAL),
            err(EINVAL),
            err(EINVAL),
            err(EINVAL),
            err(ENOTDIR),
            3,
            err(EFAULT),
            err(EFAULT),
            err(EFAULT),
            err(EFAULT),
            5,
            16, // 21 - 5 bytes: it went on where the good read stopped
            err(EBADF),
            err(EBADF),
            0,
            err(EBADF),
            err(EFAULT),
            err(EBADF),
            err(EBADF),
            err(ESPIPE),
            err(EBADF),
            err(EINVAL),
            err(EINVAL),
            1000,
            0,
            err(38),
            err(EBADF),
            err(EBADF),
        ];
        assert_eq!(done.results, expected);
        assert_eq!(&done.buffer[..5], b"Hello");
        assert_eq!(&done.buffer[8..24], b" from the disk!\n");
    }

    #[test_case]
    fn files_are_made_written_and_appended() {
        let mut s = Script::new();
        let (a, b) = (s.text(b"hello"), s.text(b" disk"));
        let b_at = b;
        s.open("/made.txt", WRITE | CREATE | TRUNCATE) // 0: a new file
            .call(SYS_WRITE, 3, a, 5) // 1
            .call(SYS_WRITE, 3, b_at, 5) // 2: after the first five
            .call(SYS_READ, 3, BUFFER, 5) // 3: opened for writing only
            .call(SYS_CLOSE, 3, 0, 0) // 4
            .open("/made.txt", WRITE | APPEND) // 5: behind what is there
            .call(SYS_WRITE, 3, a, 1) // 6: "h"
            .call(SYS_CLOSE, 3, 0, 0) // 7
            .open("/made.txt", WRITE) // 8: at the start of a file that has 11 bytes
            .call(SYS_WRITE, 3, a, 1) // 9: files grow at their end only
            .call(SYS_SEEK, 3, 0, 2) // 10: to the end ...
            .call(SYS_WRITE, 3, a, 0) // 11: ... nothing to write is fine anywhere
            .call(SYS_WRITE, 3, a, 5) // 12: ... and now it works
            .call(SYS_CLOSE, 3, 0, 0) // 13
            .open("/hello.txt", READ) // 14: for reading only
            .call(SYS_WRITE, 3, a, 5) // 15
            .call(SYS_CLOSE, 3, 0, 0) // 16
            .open("/nodir/x.txt", WRITE | CREATE) // 17: the directory is not there
            .open("/Long-Name.txt", WRITE | CREATE) // 18: a name FAT32 here cannot store
            .open("/made.txt", WRITE | CREATE) // 19: CREATE does not empty an existing file
            .call(SYS_CLOSE, 3, 0, 0)
            .open("/made.txt", WRITE | TRUNCATE) // 21: TRUNCATE does
            .call(SYS_CLOSE, 3, 0, 0);
        let done = s.run();
        assert_eq!(done.exit, Exit::Code(0));
        assert_eq!(
            done.results,
            [
                3,
                5,
                5,
                err(EBADF),
                0,
                3,
                1,
                0,
                3,
                err(EINVAL),
                11,
                0,
                5,
                0,
                3,
                err(EBADF),
                0,
                err(ENOENT),
                err(EINVAL),
                3,
                0,
                3,
                0
            ]
        );
        // what the second to last `open` left: nothing - and before it, the file held "hello disk" + "h" + "hello"
        assert_eq!(disk_file("/made.txt").unwrap(), b"");
        fs::FS.lock().as_mut().unwrap().remove("/made.txt").unwrap();
    }

    #[test_case]
    fn what_a_program_wrote_is_on_the_disk_for_everyone() {
        let mut s = Script::new();
        let text = s.text(b"written by ring 3\n");
        s.open("/ring3.txt", WRITE | CREATE | TRUNCATE)
            .call(SYS_WRITE, 3, text, 18)
            .call(SYS_CLOSE, 3, 0, 0);
        assert_eq!(s.run().results, [3, 18, 0]);
        assert_eq!(disk_file("/ring3.txt").unwrap(), b"written by ring 3\n");
        fs::FS.lock().as_mut().unwrap().remove("/ring3.txt").unwrap();
    }

    #[test_case]
    fn one_call_moves_at_most_64_kib() {
        let mut s = Script::new();
        s.open("/big.bin", READ)
            .call(SYS_READ, 3, BUFFER, 1 << 30) // 1 GiB asked, 64 KiB come
            .call(SYS_READ, 3, BUFFER, 1 << 30) // the rest of the 100 000 bytes
            .call(SYS_READ, 3, BUFFER, 1 << 30) // nothing
            .open("/big0.bin", WRITE | CREATE | TRUNCATE)
            .call(SYS_WRITE, 4, BUFFER, 100_000) // 64 KiB are taken ...
            .call(SYS_WRITE, 4, BUFFER + 65_536, 100_000 - 65_536) // ... and the program asks again for the rest
            .call(SYS_CLOSE, 4, 0, 0);
        let done = s.run();
        assert_eq!(done.results, [3, 65_536, 100_000 - 65_536, 0, 4, 65_536, 34_464, 0]);
        // the second `read` wrote over the first one's buffer: its last bytes are the end of the file
        assert_eq!(
            done.buffer[..34_464],
            (65_536..100_000).map(|i| (i % 253) as u8).collect::<Vec<u8>>()[..]
        );
        assert_eq!(disk_file("/big0.bin").unwrap().len(), 100_000);
        fs::FS.lock().as_mut().unwrap().remove("/big0.bin").unwrap();
    }

    #[test_case]
    fn a_program_that_ends_closes_its_files() {
        assert_eq!(tables(), 0);
        let mut s = Script::new();
        s.open("/hello.txt", READ).open("/big.bin", READ); // never closed
        assert_eq!(s.run().exit, Exit::Code(0));
        assert_eq!(tables(), 0, "its table went with it");
        // and one that is killed with files open
        let mut s = Script::new();
        s.open("/hello.txt", READ).call(KILL, 0, 0, 0);
        let done = s.run();
        assert_eq!(done.exit, Exit::Killed("invalid opcode"));
        assert_eq!(done.results[0], 3);
        assert_eq!(tables(), 0);
    }

    #[test_case]
    fn a_system_call_runs_with_interrupts_on() {
        // the gate switched them off; a call that waits for the disk must let the timer and the disk's interrupt in
        let mut s = Script::new();
        s.open("/hello.txt", READ).call(SYS_READ, 3, BUFFER, 5);
        assert_eq!(s.run().results, [3, 5]);
        assert!(INTERRUPTS_ON_IN_READ.load(core::sync::atomic::Ordering::SeqCst));
    }

    #[test_case]
    fn two_programs_have_tables_of_their_own() {
        // both open the same file and read their own piece of it; each has its own offset and its own number 3
        let mut s1 = Script::new();
        s1.open("/hello.txt", READ).call(SYS_READ, 3, BUFFER, 5);
        let mut s2 = Script::new();
        s2.open("/hello.txt", READ)
            .call(SYS_SEEK, 3, 6, 0)
            .call(SYS_READ, 3, BUFFER, 4);
        let (p1, p2) = (s1.run(), s2.run());
        assert_eq!(p1.results, [3, 5]);
        assert_eq!(p2.results, [3, 6, 4]);
        assert_eq!(&p1.buffer[..5], b"Hello");
        assert_eq!(&p2.buffer[..4], b"from");
    }

    #[test_case]
    fn programs_from_the_disk_read_and_write_files() {
        user::OUTPUT.lock().clear();
        assert_eq!(user::run_file("/bin/cat").unwrap(), Exit::Code(0));
        assert_eq!(*user::OUTPUT.lock(), "Hello from the disk!\n");
        // copies the file 16 bytes at a time - a file of 21 bytes takes two reads and two writes
        assert_eq!(user::run_file("/bin/copy").unwrap(), Exit::Code(21));
        assert_eq!(disk_file("/copy.txt").unwrap(), b"Hello from the disk!\n");
        // a second time: TRUNCATE empties the copy first, so it does not grow
        assert_eq!(user::run_file("/bin/copy").unwrap(), Exit::Code(21));
        assert_eq!(disk_file("/copy.txt").unwrap(), b"Hello from the disk!\n");
    }
}
