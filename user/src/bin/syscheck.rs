//! Robustness test of the syscall interface: every syscall is called with bad
//! pointers, bad lengths, bad descriptors and bad numbers. Each call must come
//! back with an error (and the kernel must stay alive); then a seeded random
//! phase throws argument mixes at all non-blocking syscalls.
//! `syscheck race [ms]` races buffer copies against another thread shrinking
//! the heap (the kernel must neither fault nor panic).
//! Usage: syscheck [random-calls] [seed] | syscheck race [ms]. Prints every
//! failed check; exit code = number of failures.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use ulib::process::{self, ExitStatus};
use ulib::{println, sys};

ulib::entry!(main);

const ENOSYS: i64 = 38;
const EBADF: i64 = 9;
const EFAULT: i64 = 14;
const EPIPE: i64 = 32;

/// Addresses no program may hand to the kernel: unmapped low memory, the gap
/// below the stack area, above the stack, non-canonical and kernel addresses.
const BAD_PTRS: [u64; 10] = [
    0,
    1,
    0xFFF,
    0x7FFF_FFEE_0000, // below the stack area (USER_STACK_TOP - 1 MiB - 64 KiB)
    0x7FFF_FFFF_0000, // USER_STACK_TOP: first byte above the stack
    0x0000_8000_0000_0000,
    0xFFFF_8000_0000_0000,
    0xFFFF_FFFF_8000_0000,
    u64::MAX - 0xFFF,
    u64::MAX,
];
const BAD_FDS: [u64; 6] = [32, 33, 1000, 1 << 32, u64::MAX - 1, u64::MAX];
const LENS: [u64; 6] = [1, 8, 4096, 1 << 32, 1 << 47, u64::MAX];

fn call(nr: u64, a: [u64; 5]) -> i64 {
    unsafe { sys::syscall5(nr, a[0], a[1], a[2], a[3], a[4]) }
}

/// xorshift64*: the same seed gives the same sequence of calls.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn pick(&mut self, from: &[u64]) -> u64 {
        from[(self.next() % from.len() as u64) as usize]
    }
}

/// One thread keeps growing, touching and shrinking the heap above its
/// current end while this one hands exactly that memory to syscalls that
/// copy from and to user buffers. Nothing is allocated during the race (the
/// allocator would use the same heap end).
fn race(ms: u64) -> i32 {
    use core::sync::atomic::{AtomicU64, Ordering::SeqCst};
    static BASE: AtomicU64 = AtomicU64::new(0);
    static STOP: AtomicU64 = AtomicU64::new(0);
    const SPAN: u64 = 64 * 1024;
    let file = sys::open("/README.txt", sys::O_READ) as u64;
    let racer = ulib::thread::spawn(|| {
        let base = (sys::brk(0) as u64 + 0xFFF) & !0xFFF;
        BASE.store(base, SeqCst);
        let mut rounds = 0;
        while STOP.load(SeqCst) == 0 {
            sys::brk(base + SPAN);
            for page in (base..base + SPAN).step_by(4096) {
                unsafe { (page as *mut u8).write_volatile(1) };
            }
            sys::brk(base);
            rounds += 1;
        }
        rounds
    })
    .expect("thread_spawn");
    while BASE.load(SeqCst) == 0 {
        sys::yield_now();
    }
    let base = BASE.load(SeqCst);
    let (end, mut calls) = (sys::uptime_ms() + ms, 0u64);
    while sys::uptime_ms() < end {
        call(sys::OPEN, [base, 1024, 0, 0, 0]); // copy from user
        call(sys::SEEK, [file, 0, 0, 0, 0]);
        call(sys::READ, [file, base, 200, 0, 0]); // copy to user
        call(sys::GETCWD, [base + SPAN - 8, 8, 0, 0, 0]);
        calls += 3;
    }
    STOP.store(1, SeqCst);
    let rounds = racer.join().unwrap_or(-1);
    println!(
        "syscheck race: {} copies against {} heap shrinks in {} ms, kernel still alive",
        calls, rounds, ms
    );
    0
}

fn main(mut args: ulib::Args) -> i32 {
    let mut peek = args.clone();
    if peek.next() == Some("race") {
        args.next();
        return race(args.next().and_then(|s| s.parse().ok()).unwrap_or(2000));
    }
    let random_calls: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0x5EED);
    let failures = core::cell::Cell::new(0);
    let check = |ok: bool, what: &dyn core::fmt::Display| {
        if !ok {
            println!("FAIL: {}", what);
            failures.set(failures.get() + 1);
        }
    };

    // valid things to combine with the bad arguments
    let path = "/README.txt";
    let (pp, pl) = (path.as_ptr() as u64, path.len() as u64);
    let file = sys::open(path, sys::O_READ);
    let dir = sys::open("/", sys::O_READ);
    check(file >= 3 && dir >= 3, &"open /README.txt and / for the test");
    let (file, dir) = (file as u64, dir as u64);
    let mut heap = vec![0u8; 8192];
    let hp = heap.as_mut_ptr() as u64;
    let code = main as fn(ulib::Args) -> i32 as usize as u64; // mapped, but not writable

    // 1. bad pointers: every call that takes one must fail cleanly
    let expect_error = |nr: u64, a: [u64; 5], name: &str| {
        let r = call(nr, a);
        check(
            r < 0,
            &format_args!(
                "{}({:#x}, {:#x}, {:#x}, {:#x}) returned {}",
                name, a[0], a[1], a[2], a[3], r
            ),
        );
    };
    for &p in BAD_PTRS.iter() {
        for &l in LENS.iter() {
            expect_error(sys::WRITE, [1, p, l, 0, 0], "write");
            expect_error(sys::READ, [file, p, l, 0, 0], "read");
            expect_error(sys::OPEN, [p, l, 0, 0, 0], "open");
            expect_error(sys::STAT, [p, l, hp, 0, 0], "stat");
            expect_error(sys::GETCWD, [p, l, 0, 0, 0], "getcwd");
            expect_error(sys::UNLINK, [p, l, 0, 0, 0], "unlink");
            expect_error(sys::MKDIR, [p, l, 0, 0, 0], "mkdir");
            expect_error(sys::CHDIR, [p, l, 0, 0, 0], "chdir");
            expect_error(sys::KSHELL, [p, l, 0, 0, 0], "kshell");
            expect_error(sys::KCOMMANDS, [p, l, 0, 0, 0], "kcommands");
            expect_error(sys::SPAWN, [p, l, 0, 0, 0], "spawn path");
            expect_error(sys::SPAWN, [pp, pl, p, l, 0], "spawn args");
            expect_error(sys::PROCLIST, [p, l, 0, 0, 0], "proclist");
            expect_error(sys::RENAME, [p, l, pp, pl, 0], "rename from");
            expect_error(sys::RENAME, [pp, pl, p, l, 0], "rename to");
            expect_error(sys::SETENV, [p, l, pp, pl, 0], "setenv name");
            if p != 0 {
                // a null value pointer means "remove the variable"
                expect_error(sys::SETENV, [pp, 4, p, l, 0], "setenv value");
            }
            expect_error(sys::GETENV, [p, l, hp, 64, 0], "getenv name");
            expect_error(sys::ENVIRON, [p, l, 0, 0, 0], "environ");
            expect_error(sys::UNAME, [p, l, 0, 0, 0], "uname");
        }
        expect_error(sys::STAT, [pp, pl, p, 0, 0], "stat buffer");
        expect_error(sys::READDIR, [dir, p, 0, 0, 0], "readdir");
        expect_error(sys::PIPE, [p, 0, 0, 0, 0], "pipe");
        expect_error(sys::SYSINFO, [p, 0, 0, 0, 0], "sysinfo");
        expect_error(sys::NET_INFO, [p, 0, 0, 0, 0], "net_info");
        expect_error(sys::FUTEX_WAIT, [p, 0, 1, 0, 0], "futex_wait");
        expect_error(sys::WAIT, [0, p, 0, 0, 0], "wait");
        if p >= 0x0000_8000_0000_0000 || p == 0 {
            expect_error(sys::THREAD_SPAWN, [p, hp + 4096, 0, 0, 0], "thread_spawn entry");
        }
        if p >= 0x0000_8000_0000_0000 {
            // (stack 0 is valid: the kernel picks a stack, M52)
            expect_error(sys::THREAD_SPAWN, [code, p, 0, 0, 0], "thread_spawn stack");
        }
    }
    // a valid start with a length running into unmapped or kernel memory
    for &l in LENS[3..].iter() {
        expect_error(sys::WRITE, [1, hp, l, 0, 0], "write heap+huge");
        expect_error(sys::READ, [file, hp, l, 0, 0], "read heap+huge");
        expect_error(sys::WRITE, [1, code, l, 0, 0], "write code+huge");
        let stack_var = 0u64;
        let sp = &stack_var as *const u64 as u64;
        expect_error(sys::READ, [file, sp, l, 0, 0], "read stack+huge");
    }
    // the length of an output buffer is only its capacity
    check(
        call(sys::GETCWD, [hp, u64::MAX, 0, 0, 0]) == 1,
        &"getcwd into a buffer with a huge capacity",
    );
    // a failed readdir must not use up a directory entry
    let first = call(sys::READDIR, [dir, hp, 0, 0, 0]);
    let d2 = sys::open("/", sys::O_READ) as u64;
    check(
        call(sys::READDIR, [d2, 0, 0, 0, 0]) == -EFAULT,
        &"readdir into null -> EFAULT",
    );
    let mut a = [0u8; 300];
    let mut b = [0u8; 300];
    call(sys::READDIR, [d2, a.as_mut_ptr() as u64, 0, 0, 0]);
    let d3 = sys::open("/", sys::O_READ) as u64;
    call(sys::READDIR, [d3, b.as_mut_ptr() as u64, 0, 0, 0]);
    check(first == 1 && a == b, &"readdir after EFAULT returns the first entry");
    sys::close(d2);
    sys::close(d3);
    // empty buffers may have any pointer, even null
    for p in [0, 1, u64::MAX] {
        check(
            call(sys::WRITE, [1, p, 0, 0, 0]) == 0,
            &format_args!("write({:#x}, 0) -> 0", p),
        );
        check(
            call(sys::READ, [file, p, 0, 0, 0]) == 0,
            &format_args!("read({:#x}, 0) -> 0", p),
        );
    }

    // the kernel must not write into read-only (code) pages
    expect_error(sys::READ, [file, code, 16, 0, 0], "read into code");
    expect_error(sys::GETCWD, [code, 64, 0, 0, 0], "getcwd into code");
    expect_error(sys::SYSINFO, [code, 0, 0, 0, 0], "sysinfo into code");
    check(
        call(sys::READ, [file, code, 16, 0, 0]) == -EFAULT,
        &"read into code -> EFAULT",
    );

    // 2. bad descriptors
    for &fd in BAD_FDS.iter() {
        for (nr, name) in [
            (sys::CLOSE, "close"),
            (sys::DUP, "dup"),
            (sys::SHUTDOWN, "shutdown"),
            (sys::READDIR, "readdir"),
        ] {
            check(
                call(nr, [fd, hp, 0, 0, 0]) == -EBADF,
                &format_args!("{}({:#x}) -> EBADF", name, fd),
            );
        }
        check(
            call(sys::READ, [fd, hp, 16, 0, 0]) == -EBADF,
            &format_args!("read({:#x}) -> EBADF", fd),
        );
        check(
            call(sys::WRITE, [fd, hp, 16, 0, 0]) == -EBADF,
            &format_args!("write({:#x}) -> EBADF", fd),
        );
        check(
            call(sys::SEEK, [fd, 0, 0, 0, 0]) == -EBADF,
            &format_args!("seek({:#x}) -> EBADF", fd),
        );
        check(
            call(sys::ISATTY, [fd, 0, 0, 0, 0]) <= 0,
            &format_args!("isatty({:#x}) is no terminal", fd),
        );
        check(
            call(sys::DUP2, [fd, 5, 0, 0, 0]) < 0,
            &format_args!("dup2({:#x}, 5) fails", fd),
        );
        check(
            call(sys::DUP2, [0, fd, 0, 0, 0]) < 0,
            &format_args!("dup2(0, {:#x}) fails", fd),
        );
        check(
            call(sys::TCP_ACCEPT, [fd, hp, 0, 0, 0]) < 0,
            &format_args!("tcp_accept({:#x}) fails", fd),
        );
        check(
            call(sys::SENDTO, [fd, hp, 4, 0x0A00_0202, 7]) < 0,
            &format_args!("sendto({:#x}) fails", fd),
        );
        check(
            call(sys::RECVFROM, [fd, hp, 16, hp + 64, 1]) < 0,
            &format_args!("recvfrom({:#x}) fails", fd),
        );
    }
    // wrong kinds of descriptor
    check(call(sys::READDIR, [file, hp, 0, 0, 0]) < 0, &"readdir on a file fails");
    check(
        call(sys::TCP_ACCEPT, [file, hp, 0, 0, 0]) < 0,
        &"tcp_accept on a file fails",
    );
    check(
        call(sys::SEEK, [file, 0, 99, 0, 0]) < 0,
        &"seek with a bad whence fails",
    );
    check(
        call(sys::SEEK, [file, i64::MIN as u64, 1, 0, 0]) < 0,
        &"seek to a negative offset fails",
    );

    // 3. bad numbers and values
    for nr in [
        sys::SYSCALL_COUNT,
        sys::SYSCALL_COUNT + 1,
        1000,
        u32::MAX as u64,
        1 << 32,
        u64::MAX,
    ] {
        check(
            call(nr, [0; 5]) == -ENOSYS,
            &format_args!("syscall {:#x} -> ENOSYS", nr),
        );
    }
    for v in [1, 0x1000, u64::MAX, 1 << 47] {
        check(
            call(sys::BRK, [v, 0, 0, 0, 0]) < 0,
            &format_args!("brk({:#x}) fails", v),
        );
    }
    for t in [0, 1, u64::MAX, sys::getpid(), sys::gettid()] {
        check(
            call(sys::THREAD_JOIN, [t, 0, 0, 0, 0]) < 0,
            &format_args!("thread_join({:#x}) fails", t),
        );
    }
    for (pid, sig) in [
        (0, 15),
        (u64::MAX, 9),
        (sys::getpid() + 1000, 9),
        (sys::getpid(), 65),
        (sys::getpid(), u64::MAX),
    ] {
        check(
            call(sys::KILL, [pid, sig, 0, 0, 0]) < 0,
            &format_args!("kill({:#x}, {}) fails", pid, sig),
        );
    }
    for sig in [0, 65, 1000, u64::MAX] {
        check(
            call(sys::SIGACTION, [sig, 0, 0, 0, 0]) < 0,
            &format_args!("sigaction({:#x}) fails", sig),
        );
    }
    for pid in [0, u64::MAX] {
        check(
            call(sys::WAIT, [pid, hp, 0, 0, 0]) < 0,
            &format_args!("wait({:#x}) without children fails", pid),
        );
    }

    // a huge write must not be copied into the kernel in one piece (bug #16):
    // 16 MiB into a pipe without a reader fails at once, the kernel heap stays put
    let mut fds = [0u32; 2];
    check(sys::pipe(&mut fds) == 0, &"pipe for the big write");
    sys::close(fds[0] as u64);
    let big = vec![b'x'; 16 << 20];
    let heap_before = sys::sysinfo().heap_total;
    check(
        sys::write(fds[1] as u64, &big) == -EPIPE,
        &"16 MiB into a pipe without reader -> EPIPE",
    );
    let grown = sys::sysinfo().heap_total - heap_before;
    check(
        grown < 1 << 20,
        &format_args!("kernel heap grew by {} KiB for one write", grown / 1024),
    );
    sys::close(fds[1] as u64);
    drop(big);
    // a datagram larger than the network can carry is refused before it is copied
    let udp = sys::udp_socket(0);
    if udp >= 0 {
        let r = call(sys::SENDTO, [udp as u64, hp, 1 << 30, 0x0A00_0202, 7]);
        check(r == -90, &format_args!("sendto with 1 GiB -> EMSGSIZE, got {}", r));
        sys::close(udp as u64);
    }

    // 4. a bad status pointer must not lose a child's exit status
    match process::spawn("/bin/hello", []) {
        Ok(child) => {
            check(
                call(sys::WAIT, [child, 0xFFFF_8000_0000_0000, 0, 0, 0]) == -EFAULT,
                &"wait with a kernel pointer",
            );
            check(
                matches!(process::wait(child), Ok(ExitStatus::Exited(0))),
                &"child status kept after EFAULT",
            );
        }
        Err(_) => check(false, &"spawn /bin/hello"),
    }

    // 5. random mixes of all of the above (no calls that block, end the
    // process, change its memory, its signals or its descriptors 0-2)
    // (kill is left out: any process may signal any other one, by design)
    const RANDOM_OK: [u64; 34] = [
        1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 31, 36, 40, 41,
        42, 43, 49, 52,
    ];
    let mut pool = vec![0u64, 3, 7, 15, 64, 999];
    pool.extend_from_slice(&BAD_PTRS);
    pool.extend_from_slice(&BAD_FDS);
    pool.extend_from_slice(&LENS);
    let mut rng = Rng(seed | 1);
    let before = sys::sysinfo();
    let start = sys::uptime_ms();
    for _ in 0..random_calls {
        let nr = if rng.next() % 64 == 0 {
            sys::SYSCALL_COUNT + rng.next() % 1000
        } else {
            rng.pick(&RANDOM_OK)
        };
        let mut a = [0u64; 5];
        for x in a.iter_mut() {
            *x = rng.pick(&pool);
        }
        // never touch the standard descriptors (the test would lose its output)
        if matches!(nr, 2 | 8 | 22 | 23 | 28 | 40) && a[0] <= 2 {
            a[0] = 32;
        }
        if nr == 23 && a[1] <= 2 {
            a[1] = 32;
        }
        let _ = call(nr, a);
    }
    let after = sys::sysinfo();
    let ms = sys::uptime_ms() - start;
    let lost_kib = (before.mem_free as i64 - after.mem_free as i64) / 1024;
    println!(
        "syscheck: {} random calls (seed {:#x}) in {} ms, free memory {:+} KiB",
        random_calls, seed, ms, -lost_kib
    );
    check(
        lost_kib < 1024,
        &format_args!("random phase lost {} KiB of memory", lost_kib),
    );

    // the process still works normally afterwards
    check(sys::write(1, b"") == 0, &"write still works");
    heap[0] = 1;
    // (the random calls may have closed or replaced descriptors 3 and up)
    let file = sys::open(path, sys::O_READ) as u64;
    check(
        call(sys::READ, [file, hp, 4, 0, 0]) == 4,
        &"open + read into the heap still work",
    );

    if failures.get() == 0 {
        println!("syscheck: all checks passed");
    }
    failures.get()
}
