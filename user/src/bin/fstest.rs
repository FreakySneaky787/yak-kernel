//! Self-test of the file system calls, including the error cases.
//! Works in a scratch directory below the current directory and removes it again.
//! Prints every failed check; exit code = number of failures.

#![no_std]
#![no_main]

use ulib::fs::{self, Dir, EBADF, EINVAL, EISDIR, EMFILE, ENOENT, ENOTDIR, ERANGE, Error, File};
use ulib::{println, sys};

ulib::entry!(main);

const DIR: &str = "fstest.tmp";
const FILE: &str = "fstest.tmp/data.bin";
const CHUNK: usize = 5000;

fn pattern(i: usize) -> u8 {
    (i % 251) as u8
}

fn main(_args: ulib::Args) -> i32 {
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        if !ok {
            println!("FAIL: {}", what);
            failures += 1;
        }
    };

    // errors without touching the disk
    check(
        File::open("/does/not/exist").err() == Some(Error(ENOENT)),
        "open missing -> ENOENT",
    );
    check(sys::read(77, &mut [0; 4]) == -EBADF, "read bad fd -> EBADF");
    check(sys::close(77) == -EBADF, "close bad fd -> EBADF");
    check(sys::open("/x", 0) == -EINVAL, "open without READ/WRITE -> EINVAL");
    check(sys::open("/x", 64) == -EINVAL, "open with unknown flag -> EINVAL");
    check(
        sys::open("/x", sys::O_READ | sys::O_CREATE) == -EINVAL,
        "CREATE without WRITE -> EINVAL",
    );
    let bad_ptr = unsafe { sys::syscall3(sys::OPEN, 0x10, 5, sys::O_READ) };
    check(bad_ptr == -14, "open with an unmapped path pointer -> EFAULT");
    check(
        File::create("/bin").err() == Some(Error(EISDIR)),
        "open dir for writing -> EISDIR",
    );
    match File::open("/bin") {
        Ok(mut d) => check(
            d.read(&mut [0; 4]).err() == Some(Error(EISDIR)),
            "read on a dir -> EISDIR",
        ),
        Err(_) => check(false, "open /bin for reading"),
    }
    check(
        fs::cwd(&mut []).err() == Some(Error(ERANGE)),
        "getcwd with an empty buffer -> ERANGE",
    );
    let mut cwd = [0u8; 256];
    check(fs::cwd(&mut cwd).is_ok_and(|c| c.starts_with('/')), "getcwd");

    // a scratch directory with a 15000-byte file written in three appends (relative paths)
    sys::unlink(FILE);
    sys::unlink(DIR);
    check(sys::mkdir(DIR) == 0, "mkdir");
    match File::create(FILE) {
        Ok(mut f) => {
            for c in 0..3 {
                let mut buf = [0u8; CHUNK];
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = pattern(c * CHUNK + i);
                }
                check(f.write(&buf) == Ok(CHUNK), "write chunk");
            }
            check(
                f.read(&mut [0; 4]).err() == Some(Error(EBADF)),
                "read on a write-only fd -> EBADF",
            );
        }
        Err(_) => check(false, "create file"),
    }
    check(fs::stat(FILE).map(|s| s.size) == Ok(3 * CHUNK as u64), "stat size");
    match File::open(FILE) {
        Ok(mut f) => {
            let mut buf = [0u8; 4096];
            let mut total = 0;
            let mut same = true;
            loop {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        same &= buf[..n].iter().enumerate().all(|(i, &b)| b == pattern(total + i));
                        total += n;
                    }
                    Err(_) => {
                        same = false;
                        break;
                    }
                }
            }
            check(total == 3 * CHUNK && same, "read back the file");
            check(
                f.write(b"x").err() == Some(Error(EBADF)),
                "write on a read-only fd -> EBADF",
            );
            check(
                sys::readdir(f.fd(), &mut unsafe { core::mem::zeroed() }) == -ENOTDIR,
                "readdir on a file -> ENOTDIR",
            );
        }
        Err(_) => check(false, "open file"),
    }
    match Dir::open(DIR) {
        Ok(d) => {
            let mut names = 0;
            for e in d.flatten() {
                names += 1;
                check(e.name() == "data.bin" && !e.is_dir(), "directory entry");
            }
            check(names == 1, "exactly one entry");
        }
        Err(_) => check(false, "open dir"),
    }

    // positioned writes, seek, append, rename
    match File::open_with(FILE, sys::O_READ | sys::O_WRITE) {
        Ok(mut f) => {
            check(f.write(b"HEAD") == Ok(4), "overwrite the start");
            check(f.seek(0, sys::SEEK_END) == Ok(3 * CHUNK as u64), "seek to the end");
            check(f.write(b"TAIL") == Ok(4), "write at the end");
            check(f.seek(-8, sys::SEEK_CUR) == Ok(3 * CHUNK as u64 - 4), "seek relative");
            let mut buf = [0u8; 8];
            check(f.read(&mut buf) == Ok(8), "read around the old end");
            let mut expected = [0u8; 8];
            for (i, b) in expected[..4].iter_mut().enumerate() {
                *b = pattern(3 * CHUNK - 4 + i);
            }
            expected[4..].copy_from_slice(b"TAIL");
            check(buf == expected, "old end + TAIL");
            check(
                f.seek(-1, sys::SEEK_SET).err() == Some(Error(EINVAL)),
                "negative offset -> EINVAL",
            );
            check(f.seek(20_000, sys::SEEK_SET) == Ok(20_000), "seek past the end");
            check(f.write(b"X") == Ok(1), "write after a gap");
            check(f.seek(0, sys::SEEK_SET) == Ok(0), "back to the start");
            let mut head = [0u8; 6];
            check(
                f.read(&mut head) == Ok(6) && &head[..4] == b"HEAD" && head[4] == pattern(4),
                "HEAD overwrote 4 bytes only",
            );
            check(
                f.seek(15_010, sys::SEEK_SET).is_ok() && f.read(&mut buf) == Ok(8) && buf == [0; 8],
                "the gap reads as zeros",
            );
        }
        Err(_) => check(false, "open read+write"),
    }
    check(fs::stat(FILE).map(|s| s.size) == Ok(20_001), "size after the gap write");
    match File::open_with(FILE, sys::O_WRITE | sys::O_APPEND) {
        Ok(mut f) => {
            let _ = f.seek(0, sys::SEEK_SET);
            check(f.write(b"Z") == Ok(1), "append");
        }
        Err(_) => check(false, "open for append"),
    }
    check(
        fs::stat(FILE).map(|s| s.size) == Ok(20_002),
        "APPEND ignores the position",
    );
    let moved = "fstest.tmp/moved.bin";
    check(fs::rename(FILE, moved).is_ok(), "rename");
    check(
        fs::stat(FILE).err() == Some(Error(ENOENT)) && fs::stat(moved).is_ok(),
        "renamed",
    );
    check(fs::rename(moved, FILE).is_ok(), "rename back");
    let mut fds = [0u32; 2];
    sys::pipe(&mut fds);
    check(
        sys::seek(fds[0] as u64, 0, sys::SEEK_SET) == -29,
        "seek on a pipe -> ESPIPE",
    );
    sys::close(fds[0] as u64);
    sys::close(fds[1] as u64);

    // descriptor table limit: 3 standard + 29 more
    let mut opened = 0;
    let err = loop {
        match sys::open(DIR, sys::O_READ) {
            fd if fd >= 0 => opened += 1,
            e => break e,
        }
        if opened > 100 {
            break 0;
        }
    };
    check(err == -EMFILE && opened == 29, "too many open files -> EMFILE");
    for fd in 3..32 {
        sys::close(fd);
    }

    check(sys::unlink(DIR) == -39, "unlink non-empty dir -> ENOTEMPTY");
    check(sys::unlink(FILE) == 0 && sys::unlink(DIR) == 0, "clean up");
    check(fs::stat(DIR).err() == Some(Error(ENOENT)), "scratch dir is gone");

    if failures == 0 {
        println!("fstest: all checks passed");
    }
    failures
}
