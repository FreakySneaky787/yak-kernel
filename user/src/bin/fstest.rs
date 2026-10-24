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
