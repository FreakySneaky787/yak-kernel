//! Self-test of spawn / wait / kill. Prints every failed check; exit code =
//! number of failures. Needs /bin/fault and /bin/hello.

#![no_std]
#![no_main]

extern crate alloc;

use ulib::fs::Error;
use ulib::process::{self, ExitStatus};
use ulib::{println, sys};

ulib::entry!(main);

const ENOENT: i64 = 2;
const ESRCH: i64 = 3;
const ENOEXEC: i64 = 8;
const ECHILD: i64 = 10;

fn killed_with(status: &Result<ExitStatus, Error>, text: &str) -> bool {
    matches!(status, Ok(ExitStatus::Killed(r, _)) if r.contains(text))
}

fn main(_args: ulib::Args) -> i32 {
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        if !ok {
            println!("FAIL: {}", what);
            failures += 1;
        }
    };

    check(
        process::run("/bin/fault", ["exit", "5"]) == Ok(ExitStatus::Exited(5)),
        "child exit code",
    );
    check(
        killed_with(&process::run("/bin/fault", ["null"]), "page fault at 0x0"),
        "child killed by a fault",
    );
    check(
        process::spawn("/bin/nothing", []) == Err(Error(ENOENT)),
        "spawn missing -> ENOENT",
    );
    check(
        process::spawn("/README.txt", []) == Err(Error(ENOEXEC)),
        "spawn non-ELF -> ENOEXEC",
    );

    // a sleeping child is killed (and woken up for it)
    match process::spawn("/bin/fault", ["sleep"]) {
        Ok(pid) => {
            sys::sleep_ms(50);
            check(process::kill(pid).is_ok(), "kill child");
            check(
                process::wait(pid) == Ok(ExitStatus::Killed("killed".into(), 9)),
                "status of the killed child",
            );
            check(process::wait(pid) == Err(Error(ECHILD)), "second wait -> ECHILD");
        }
        Err(_) => check(false, "spawn sleeper"),
    }
    // a spinning child too
    match process::spawn("/bin/fault", ["spin"]) {
        Ok(pid) => {
            sys::sleep_ms(30);
            check(process::kill(pid).is_ok(), "kill spinner");
            check(
                process::wait(pid) == Ok(ExitStatus::Killed("killed".into(), 9)),
                "status of the spinner",
            );
        }
        Err(_) => check(false, "spawn spinner"),
    }
    // foreign processes are off limits
    let me = sys::getpid();
    check(process::kill(me + 1000) == Err(Error(ESRCH)), "kill unknown -> ESRCH");
    check(process::wait(me + 1000) == Err(Error(ECHILD)), "wait unknown -> ECHILD");
    check(process::wait(me) == Err(Error(ECHILD)), "wait for myself -> ECHILD");

    // several children at once, collected in reverse order
    let pids: alloc::vec::Vec<u64> = (0..4)
        .filter_map(|i| process::spawn("/bin/fault", ["exit", ["0", "1", "2", "3"][i]]).ok())
        .collect();
    check(pids.len() == 4, "four children");
    for (i, &pid) in pids.iter().enumerate().rev() {
        check(process::wait(pid) == Ok(ExitStatus::Exited(i as i64)), "collect child");
    }

    // an orphan: started and never waited for (the kernel cleans up)
    check(process::spawn("/bin/hello", ["orphan"]).is_ok(), "spawn orphan");

    if failures == 0 {
        println!("spawntest: all checks passed");
    }
    failures
}
