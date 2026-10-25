//! Self-test of threads, futexes and the thread-safe heap. Prints every
//! failed check; exit code = number of failures. Starts copies of itself
//! (`/bin/threadtest <mode>`) for the ways a process with threads can end.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use ulib::process::{self, ExitStatus};
use ulib::sync::Mutex;
use ulib::{println, signal, sys, thread};

ulib::entry!(main);

const EAGAIN: i64 = 11;
const ETIMEDOUT: i64 = 110;
const ESRCH: i64 = 3;

fn spin_forever() -> i32 {
    let x = AtomicU64::new(0);
    loop {
        x.fetch_add(1, Ordering::Relaxed);
    }
}

/// Modes for the child processes.
fn child(mode: &str) -> i32 {
    match mode {
        // a further thread ends the whole process; the main thread sleeps meanwhile
        "exit-in-thread" => {
            let _t = thread::spawn(|| {
                sys::sleep_ms(20);
                sys::exit(7)
            });
            let _spinner = thread::spawn(spin_forever);
            sys::sleep_ms(10_000);
            1
        }
        // a fault in a further thread kills the process
        "fault-in-thread" => {
            let _t = thread::spawn(|| unsafe { core::ptr::read_volatile(8 as *const u8) as i32 });
            sys::sleep_ms(10_000);
            1
        }
        // spinning threads everywhere: the parent ends us with a signal
        "spin" => {
            for _ in 0..3 {
                let _ = thread::spawn(spin_forever);
            }
            spin_forever()
        }
        // main returns while threads still run: they are stopped
        "main-returns" => {
            let _ = thread::spawn(spin_forever);
            let _ = thread::spawn(|| {
                sys::sleep_ms(10_000);
                0
            });
            sys::sleep_ms(20);
            3
        }
        _ => {
            println!("threadtest: unknown mode '{}'", mode);
            2
        }
    }
}

fn main(mut args: ulib::Args) -> i32 {
    if let Some(mode) = args.next() {
        return child(mode);
    }
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        if !ok {
            println!("FAIL: {}", what);
            failures += 1;
        }
    };

    // threads share memory; join returns the exit code
    let total = Arc::new(AtomicU64::new(0));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let total = total.clone();
            thread::spawn(move || {
                for _ in 0..100_000 {
                    total.fetch_add(1, Ordering::Relaxed);
                }
                10 + i
            })
        })
        .collect();
    let mut codes = Vec::new();
    for h in handles {
        match h {
            Ok(h) => {
                check(h.id() != sys::getpid(), "a thread has its own id");
                codes.push(h.join().unwrap_or(-1));
            }
            Err(_) => check(false, "spawn a thread"),
        }
    }
    check(total.load(Ordering::Relaxed) == 400_000, "atomic counter of 4 threads");
    check(codes == [10, 11, 12, 13], "join returns the exit codes");

    // a futex mutex protects a plain counter; the heap is used by all threads at once
    let counter = Arc::new(Mutex::new(0u64));
    let handles: Vec<_> = (0..4)
        .filter_map(|_| {
            let counter = counter.clone();
            thread::spawn(move || {
                let mut strings: Vec<String> = Vec::new();
                for i in 0..5000 {
                    *counter.lock() += 1;
                    if i % 10 == 0 {
                        strings.push(alloc::format!("item {}", i)); // allocations from all threads
                    }
                }
                let len: usize = strings.iter().map(|s| s.len()).sum();
                (len == 4389) as i32
            })
            .ok()
        })
        .collect();
    let all_ok = handles.into_iter().all(|h| h.join() == Ok(1));
    check(all_ok, "heap allocations in 4 threads");
    check(*counter.lock() == 20_000, "mutex counter of 4 threads");

    // futex: wrong value, timeout, and a real wake-up
    let word = Arc::new(AtomicU32::new(5));
    check(
        sys::futex_wait(&word, 6, 0) == -EAGAIN,
        "futex_wait with a changed value",
    );
    let start = sys::uptime_ms();
    check(sys::futex_wait(&word, 5, 50) == -ETIMEDOUT, "futex_wait timeout");
    check(sys::uptime_ms() - start >= 40, "the timeout waited");
    let w = word.clone();
    let waiter = thread::spawn(move || {
        while w.load(Ordering::SeqCst) == 5 {
            sys::futex_wait(&w, 5, 0);
        }
        w.load(Ordering::SeqCst) as i32
    });
    sys::sleep_ms(30);
    word.store(9, Ordering::SeqCst);
    sys::futex_wake(&word, 1);
    check(waiter.and_then(|h| h.join()) == Ok(9), "futex wake");

    // joining something that is not our thread
    check(
        sys::thread_join(sys::getpid()) == -ESRCH,
        "join the main thread -> ESRCH",
    );
    check(sys::thread_join(99_999) == -ESRCH, "join unknown -> ESRCH");

    // the ways a process with threads ends
    let run = |mode: &str| process::run("/bin/threadtest", [mode]);
    check(
        run("exit-in-thread") == Ok(ExitStatus::Exited(7)),
        "exit in a thread ends the process",
    );
    check(
        matches!(run("fault-in-thread"), Ok(ExitStatus::Killed(r, _)) if r.contains("page fault at 0x8")),
        "a fault in a thread kills the process",
    );
    check(
        run("main-returns") == Ok(ExitStatus::Exited(3)),
        "main returns while threads run",
    );
    match process::spawn("/bin/threadtest", ["spin"]) {
        Ok(pid) => {
            sys::sleep_ms(50);
            let _ = signal::send(pid, signal::SIGTERM);
            check(
                process::wait(pid) == Ok(ExitStatus::Killed("terminated".into(), signal::SIGTERM)),
                "SIGTERM ends 4 spinning threads",
            );
        }
        Err(_) => check(false, "spawn spinner"),
    }

    if failures == 0 {
        println!("threadtest: all checks passed");
    }
    failures
}
