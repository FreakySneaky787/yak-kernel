//! Self-test of signals. Prints every failed check; exit code = number of
//! failures. Starts copies of itself (`/bin/sigtest <mode>`) as children.
//! `sigtest wait-int` waits for Ctrl+C (used by the E2E test).

#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use ulib::fs::Error;
use ulib::process::{self, ExitStatus};
use ulib::signal::{self, SIGINT, SIGKILL, SIGTERM, SIGUSR1, SIGUSR2};
use ulib::{println, sys};

ulib::entry!(main);

const EINTR: i64 = 4;
const ESRCH: i64 = 3;

static FLAG: AtomicU32 = AtomicU32::new(0);
static HANDLED: AtomicU32 = AtomicU32::new(0);
/// Digits appended by the handlers, in the order they ran.
static LOG: AtomicU64 = AtomicU64::new(0);

fn log(digit: u64) {
    LOG.store(LOG.load(Ordering::SeqCst) * 10 + digit, Ordering::SeqCst);
}

extern "C" fn count(_sig: u32) {
    HANDLED.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn first(_sig: u32) {
    log(1);
    // stays pending until this handler has returned
    let _ = signal::raise(SIGUSR2);
    log(2);
}

extern "C" fn second(_sig: u32) {
    log(3);
}

/// Sets the flag and destroys every register a function may clobber.
extern "C" fn set_flag_and_trash(_sig: u32) {
    FLAG.store(1, Ordering::SeqCst);
    unsafe {
        asm!(
            "mov rax, -1",
            "mov rcx, -1",
            "mov rdx, -1",
            "mov rsi, -1",
            "mov rdi, -1",
            "mov r8, -1",
            "mov r9, -1",
            "mov r10, -1",
            "mov r11, -1",
            "pcmpeqd xmm0, xmm0",
            "pcmpeqd xmm1, xmm1",
            "pcmpeqd xmm2, xmm2",
            "pcmpeqd xmm3, xmm3",
            "pcmpeqd xmm4, xmm4",
            "pcmpeqd xmm5, xmm5",
            "pcmpeqd xmm6, xmm6",
            "pcmpeqd xmm7, xmm7",
            "pcmpeqd xmm8, xmm8",
            "pcmpeqd xmm9, xmm9",
            "pcmpeqd xmm10, xmm10",
            "pcmpeqd xmm11, xmm11",
            "pcmpeqd xmm12, xmm12",
            "pcmpeqd xmm13, xmm13",
            "pcmpeqd xmm14, xmm14",
            "pcmpeqd xmm15, xmm15",
            out("rax") _, out("rcx") _, out("rdx") _, out("rsi") _, out("rdi") _, out("r8") _, out("r9") _, out("r10") _, out("r11") _, out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _, out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _, out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _, out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
        )
    };
}

/// Fills all general purpose and XMM registers with known values, spins
/// (no system calls) until a signal handler sets FLAG and checks that every
/// register still has its value. Returns 0 if so.
fn spin_keeping_registers() -> u64 {
    let result: u64;
    unsafe {
        asm!(
            "push rbx",
            "push rbp",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            "mov eax, 0x100",
            "movq xmm0, rax",
            "mov eax, 0x101",
            "movq xmm1, rax",
            "mov eax, 0x102",
            "movq xmm2, rax",
            "mov eax, 0x103",
            "movq xmm3, rax",
            "mov eax, 0x104",
            "movq xmm4, rax",
            "mov eax, 0x105",
            "movq xmm5, rax",
            "mov eax, 0x106",
            "movq xmm6, rax",
            "mov eax, 0x107",
            "movq xmm7, rax",
            "mov eax, 0x108",
            "movq xmm8, rax",
            "mov eax, 0x109",
            "movq xmm9, rax",
            "mov eax, 0x10a",
            "movq xmm10, rax",
            "mov eax, 0x10b",
            "movq xmm11, rax",
            "mov eax, 0x10c",
            "movq xmm12, rax",
            "mov eax, 0x10d",
            "movq xmm13, rax",
            "mov eax, 0x10e",
            "movq xmm14, rax",
            "mov eax, 0x10f",
            "movq xmm15, rax",
            "mov eax, 0x11",
            "mov ebx, 0x22",
            "mov ecx, 0x33",
            "mov edx, 0x44",
            "mov esi, 0x55",
            "mov edi, 0x66",
            "mov ebp, 0x77",
            "mov r8d, 0x88",
            "mov r9d, 0x99",
            "mov r10d, 0xaa",
            "mov r11d, 0xbb",
            "mov r12d, 0xcc",
            "mov r13d, 0xdd",
            "mov r14d, 0xee",
            "mov r15d, 0xff",
            "2:",
            "cmp dword ptr [rip + {flag}], 0",
            "je 2b",
            "cmp rax, 0x11",
            "jne 3f",
            "cmp rbx, 0x22",
            "jne 3f",
            "cmp rcx, 0x33",
            "jne 3f",
            "cmp rdx, 0x44",
            "jne 3f",
            "cmp rsi, 0x55",
            "jne 3f",
            "cmp rdi, 0x66",
            "jne 3f",
            "cmp rbp, 0x77",
            "jne 3f",
            "cmp r8, 0x88",
            "jne 3f",
            "cmp r9, 0x99",
            "jne 3f",
            "cmp r10, 0xaa",
            "jne 3f",
            "cmp r11, 0xbb",
            "jne 3f",
            "cmp r12, 0xcc",
            "jne 3f",
            "cmp r13, 0xdd",
            "jne 3f",
            "cmp r14, 0xee",
            "jne 3f",
            "cmp r15, 0xff",
            "jne 3f",
            "movq rax, xmm0",
            "cmp rax, 0x100",
            "jne 3f",
            "movq rax, xmm1",
            "cmp rax, 0x101",
            "jne 3f",
            "movq rax, xmm2",
            "cmp rax, 0x102",
            "jne 3f",
            "movq rax, xmm3",
            "cmp rax, 0x103",
            "jne 3f",
            "movq rax, xmm4",
            "cmp rax, 0x104",
            "jne 3f",
            "movq rax, xmm5",
            "cmp rax, 0x105",
            "jne 3f",
            "movq rax, xmm6",
            "cmp rax, 0x106",
            "jne 3f",
            "movq rax, xmm7",
            "cmp rax, 0x107",
            "jne 3f",
            "movq rax, xmm8",
            "cmp rax, 0x108",
            "jne 3f",
            "movq rax, xmm9",
            "cmp rax, 0x109",
            "jne 3f",
            "movq rax, xmm10",
            "cmp rax, 0x10a",
            "jne 3f",
            "movq rax, xmm11",
            "cmp rax, 0x10b",
            "jne 3f",
            "movq rax, xmm12",
            "cmp rax, 0x10c",
            "jne 3f",
            "movq rax, xmm13",
            "cmp rax, 0x10d",
            "jne 3f",
            "movq rax, xmm14",
            "cmp rax, 0x10e",
            "jne 3f",
            "movq rax, xmm15",
            "cmp rax, 0x10f",
            "jne 3f",
            "xor eax, eax",
            "jmp 4f",
            "3:",
            "mov eax, 1",
            "4:",
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbp",
            "pop rbx",
            flag = sym FLAG,
            out("rax") result, out("rcx") _, out("rdx") _, out("rsi") _, out("rdi") _, out("r8") _, out("r9") _, out("r10") _, out("r11") _, out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _, out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _, out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _, out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
        )
    };
    result
}

/// The modes children are started in.
fn child(mode: &str) -> i32 {
    match mode {
        "spin" => {
            signal::set_handler(SIGUSR1, set_flag_and_trash).unwrap();
            spin_keeping_registers() as i32
        }
        "sleep" => {
            signal::set_handler(SIGUSR1, count).unwrap();
            let start = sys::uptime_ms();
            let ret = unsafe { sys::syscall3(sys::SLEEP_MS, 5000, 0, 0) };
            let quick = sys::uptime_ms() - start < 2000;
            (ret != -EINTR) as i32 + (!quick) as i32 + (HANDLED.load(Ordering::SeqCst) != 1) as i32
        }
        "inherit" => (signal::reset(SIGUSR2) != Ok(signal::SIG_IGN)) as i32,
        "wait-int" => {
            signal::set_handler(SIGINT, count).unwrap();
            println!("press Ctrl+C");
            while HANDLED.load(Ordering::SeqCst) == 0 {
                sys::sleep_ms(100);
            }
            println!("caught SIGINT");
            0
        }
        _ => {
            println!("sigtest: unknown mode '{}'", mode);
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
    let me = sys::getpid();

    // a handler runs before raise() returns (delivery after a system call)
    check(
        signal::set_handler(SIGUSR1, count) == Ok(signal::SIG_DFL),
        "set handler",
    );
    check(signal::raise(SIGUSR1).is_ok(), "raise");
    check(HANDLED.load(Ordering::SeqCst) == 1, "handler ran once");
    check(
        signal::raise(SIGUSR1).is_ok() && HANDLED.load(Ordering::SeqCst) == 2,
        "handler runs again",
    );

    // a signal raised inside a handler waits until that handler returned
    signal::set_handler(SIGUSR1, first).unwrap();
    signal::set_handler(SIGUSR2, second).unwrap();
    let _ = signal::raise(SIGUSR1);
    check(LOG.load(Ordering::SeqCst) == 123, "handlers do not nest");

    // ignoring, and SIGKILL cannot be caught
    check(
        signal::ignore(SIGUSR2).is_ok() && signal::raise(SIGUSR2).is_ok(),
        "ignored signal",
    );
    check(signal::set_handler(SIGKILL, count).is_err(), "SIGKILL handler refused");
    check(signal::ignore(SIGKILL).is_err(), "SIGKILL ignore refused");
    check(signal::send(me, 0).is_ok(), "signal 0 checks existence");
    check(
        signal::send(me + 1000, SIGTERM) == Err(Error(ESRCH)),
        "unknown pid -> ESRCH",
    );
    check(signal::send(me, 99).is_err(), "bad signal number");

    // children: ignored signals are inherited
    check(
        process::run("/bin/sigtest", ["inherit"]) == Ok(ExitStatus::Exited(0)),
        "SIG_IGN inherited",
    );
    let _ = signal::reset(SIGUSR2);

    // default actions end the program with the signal's reason and number
    for (sig, reason) in [
        (SIGTERM, "terminated"),
        (SIGINT, "interrupted"),
        (SIGUSR1, "user signal 1"),
    ] {
        match process::spawn("/bin/fault", ["sleep"]) {
            Ok(pid) => {
                sys::sleep_ms(30);
                check(signal::send(pid, sig).is_ok(), "send to a child");
                let status = process::wait(pid);
                check(status == Ok(ExitStatus::Killed(reason.into(), sig)), reason);
                check(
                    status.is_ok_and(|s| s.code() == 128 + sig as i32),
                    "$? of a signalled child",
                );
            }
            Err(_) => check(false, "spawn sleeper"),
        }
    }

    // a handled signal interrupts a sleep (EINTR)
    match process::spawn("/bin/sigtest", ["sleep"]) {
        Ok(pid) => {
            sys::sleep_ms(50);
            let _ = signal::send(pid, SIGUSR1);
            check(
                process::wait(pid) == Ok(ExitStatus::Exited(0)),
                "sleep interrupted by a handler",
            );
        }
        Err(_) => check(false, "spawn sleep child"),
    }

    // delivery to a program that never makes a system call (timer interrupt)
    // keeps every register
    match process::spawn("/bin/sigtest", ["spin"]) {
        Ok(pid) => {
            sys::sleep_ms(100);
            let _ = signal::send(pid, SIGUSR1);
            check(
                process::wait(pid) == Ok(ExitStatus::Exited(0)),
                "registers survive a handler",
            );
        }
        Err(_) => check(false, "spawn spin child"),
    }

    if failures == 0 {
        println!("sigtest: all checks passed");
    }
    failures
}
