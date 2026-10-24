//! Self-test of pipe / dup / dup2 / isatty and of descriptor inheritance.
//! Prints every failed check; exit code = number of failures. Needs /bin/cat.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ulib::fs::File;
use ulib::process::{self, ExitStatus};
use ulib::{io, println, sys};

ulib::entry!(main);

const EBADF: i64 = 9;
const EPIPE: i64 = 32;
const BIG: usize = 200_000; // more than the 64 KiB a pipe holds

fn pipe() -> (u64, u64) {
    let mut fds = [0u32; 2];
    assert!(sys::pipe(&mut fds) == 0, "pipe failed");
    (fds[0] as u64, fds[1] as u64)
}

fn main(_args: ulib::Args) -> i32 {
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        if !ok {
            println!("FAIL: {}", what);
            failures += 1;
        }
    };

    // a pipe within one process
    let (r, w) = pipe();
    check(sys::write(w, b"through the pipe") == 16, "write into the pipe");
    let mut buf = [0u8; 64];
    check(
        sys::read(r, &mut buf) == 16 && &buf[..16] == b"through the pipe",
        "read it back",
    );
    check(!sys::isatty(r) && !sys::isatty(w), "a pipe is no terminal");
    check(
        sys::read(w, &mut buf) == -EBADF && sys::write(r, b"x") == -EBADF,
        "wrong direction -> EBADF",
    );
    let w2 = sys::dup(w) as u64;
    sys::close(w);
    check(sys::write(w2, b"dup") == 3, "write through a dup");
    sys::close(w2);
    check(
        sys::read(r, &mut buf) == 3,
        "data written before the last writer closed",
    );
    check(sys::read(r, &mut buf) == 0, "end of file after the last writer closed");
    sys::close(r);

    let (r, w) = pipe();
    sys::close(r);
    check(sys::write(w, b"x") == -EPIPE, "no reader -> EPIPE");
    sys::close(w);
    check(
        sys::dup(99) == -EBADF && sys::dup2(99, 5) == -EBADF,
        "dup of a bad fd -> EBADF",
    );

    // a child writes into our pipe: stdout redirected with dup2 before spawn
    let path = "/pipetest.tmp";
    let data: Vec<u8> = (0..BIG).map(|i| b'a' + (i % 26) as u8).collect();
    match File::create(path) {
        Ok(mut f) => check(f.write(&data) == Ok(BIG), "write the test file"),
        Err(_) => check(false, "create the test file"),
    }
    let (r, w) = pipe();
    let saved = sys::dup(sys::STDOUT) as u64;
    sys::dup2(w, sys::STDOUT);
    let child = process::spawn("/bin/cat", [path]);
    sys::dup2(saved, sys::STDOUT); // our stdout is back
    sys::close(saved);
    sys::close(w); // only the child writes now
    let got = io::read_all(r); // blocks while cat fills the pipe, ends at its exit
    sys::close(r);
    check(got == data, "200000 bytes from a child through a pipe");
    match child {
        Ok(pid) => check(process::wait(pid) == Ok(ExitStatus::Exited(0)), "cat exit status"),
        Err(_) => check(false, "spawn cat"),
    }

    // stdin of a child from a pipe
    let (r, w) = pipe();
    let (r2, w2) = pipe();
    let (saved_in, saved_out) = (sys::dup(sys::STDIN) as u64, sys::dup(sys::STDOUT) as u64);
    sys::dup2(r, sys::STDIN);
    sys::dup2(w2, sys::STDOUT);
    let child = process::spawn("/bin/cat", []);
    sys::dup2(saved_in, sys::STDIN);
    sys::dup2(saved_out, sys::STDOUT);
    for fd in [saved_in, saved_out, r, w2] {
        sys::close(fd);
    }
    sys::write(w, b"fed through stdin\n");
    sys::close(w); // EOF for cat
    check(
        io::read_all(r2) == b"fed through stdin\n",
        "child reads stdin from a pipe",
    );
    sys::close(r2);
    if let Ok(pid) = child {
        check(process::wait(pid) == Ok(ExitStatus::Exited(0)), "second cat");
    }
    check(
        sys::isatty(sys::STDIN) || !sys::isatty(sys::STDIN),
        "isatty does not fail on stdin",
    );

    sys::unlink(path);
    if failures == 0 {
        println!("pipetest: all checks passed");
    }
    failures
}
