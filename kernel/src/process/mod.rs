//! User processes: a program loaded from an ELF file into its own address
//! space, running in ring 3 on one kernel thread.
//!
//! Life cycle: `spawn` parses and loads the ELF image, maps a stack, copies
//! the argument string onto it and starts a user thread (`task::spawn_user`).
//! The process ends through the `exit` system call, a CPU exception in user
//! mode, or `kill`. Its address space is freed right away; the exit status
//! stays in the process table until a parent collects it with `wait`.
//!
//! User memory layout:
//! - `USER_IMAGE_START ..`: the ELF segments (our programs link at 0x40_0000)
//! - `USER_STACK_TOP - USER_STACK_PAGES * 4 KiB .. USER_STACK_TOP`: the stack,
//!   with nothing mapped below it (an overflow faults and kills the process)
//! - the argument string sits at the top of the stack; `_start` receives a
//!   pointer to it in RDI and its length in RSI (our own minimal ABI).

pub mod elf;
pub mod syscall;

use crate::arch::x86_64::{disable_interrupts, without_interrupts};
use crate::mem::address_space::AddressSpace;
use crate::mem::paging::{self, Flags, MapError};
use crate::mem::{PAGE_SIZE, VirtAddr, align_down};
use crate::task::{self, Tid, UserEntry};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use elf::ElfError;
use spin::Mutex;

pub type Pid = Tid;

/// Lowest address an ELF segment may use (keeps the first 64 KiB unmapped,
/// so null pointer accesses always fault).
pub const USER_IMAGE_START: u64 = 0x1_0000;
pub const USER_STACK_TOP: u64 = 0x0000_7FFF_FFFF_0000;
pub const USER_STACK_PAGES: usize = 16;
/// ELF segments must end below the stack and its unmapped guard gap.
pub const USER_IMAGE_END: u64 = USER_STACK_TOP - (USER_STACK_PAGES as u64 + 16) * PAGE_SIZE;
pub const MAX_ARGS_LEN: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitStatus {
    /// The program called `exit(code)`.
    Exited(i64),
    /// The kernel ended it (exception in user mode or `kill`).
    Killed(String),
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExitStatus::Exited(code) => write!(f, "exited with code {}", code),
            ExitStatus::Killed(reason) => write!(f, "killed: {}", reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    Elf(ElfError),
    OutOfMemory,
    ArgsTooLong,
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExecError::Elf(e) => write!(f, "{}", e),
            ExecError::OutOfMemory => write!(f, "out of memory"),
            ExecError::ArgsTooLong => write!(f, "argument string too long"),
        }
    }
}

impl From<ElfError> for ExecError {
    fn from(e: ElfError) -> Self {
        ExecError::Elf(e)
    }
}

impl From<MapError> for ExecError {
    fn from(_: MapError) -> Self {
        ExecError::OutOfMemory
    }
}

/// Where the process's stdout/stderr (fd 1 and 2) go.
#[derive(Clone)]
pub enum Output {
    /// Screen + serial port.
    Console,
    /// Collected in a buffer (tests).
    Capture(Arc<Mutex<Vec<u8>>>),
}

pub struct Process {
    pub name: String,
    /// `None` once the process has ended.
    space: Option<AddressSpace>,
    stdout: Output,
    /// Bytes of a key that did not fit into the last `read` buffer.
    stdin_pending: Vec<u8>,
    status: Option<ExitStatus>,
    kill_requested: bool,
}

/// All processes that have not been waited for yet. Only touched with
/// interrupts disabled (exception handlers and the timer use it too).
static PROCESSES: Mutex<BTreeMap<Pid, Process>> = Mutex::new(BTreeMap::new());

/// Loads `elf` into a new address space and starts it. `name` is used for
/// `ps` and messages, `args` is handed to the program.
pub fn spawn(name: &str, elf: &[u8], args: &str, stdout: Output) -> Result<Pid, ExecError> {
    if args.len() > MAX_ARGS_LEN {
        return Err(ExecError::ArgsTooLong);
    }
    let image = elf::parse(elf)?;
    let mut space = AddressSpace::new()?;
    elf::load(&mut space, elf, &image)?;

    let stack_bottom = USER_STACK_TOP - USER_STACK_PAGES as u64 * PAGE_SIZE;
    space.map_fresh(
        VirtAddr(stack_bottom),
        USER_STACK_PAGES,
        Flags::WRITABLE | Flags::NO_EXECUTE,
    )?;
    let args_addr = align_down(USER_STACK_TOP - args.len() as u64, 16);
    space.write(VirtAddr(args_addr), args.as_bytes());
    let entry = UserEntry {
        rip: image.entry,
        rsp: args_addr,
        arg0: args_addr,
        arg1: args.len() as u64,
    };

    // The thread must not run before its table entry exists
    let pid = without_interrupts(|| {
        let pid = task::spawn_user(name, space.root(), entry);
        PROCESSES.lock().insert(
            pid,
            Process {
                name: String::from(name),
                space: Some(space),
                stdout,
                stdin_pending: Vec::new(),
                status: None,
                kill_requested: false,
            },
        );
        pid
    });
    Ok(pid)
}

/// Waits until process `pid` has ended, removes it from the table and
/// returns its exit status (`None` for an unknown pid).
pub fn wait(pid: Pid) -> Option<ExitStatus> {
    if !without_interrupts(|| PROCESSES.lock().contains_key(&pid)) {
        return None;
    }
    task::join(pid);
    without_interrupts(|| PROCESSES.lock().remove(&pid)).and_then(|p| p.status)
}

/// Spawns and waits in one go.
pub fn run(name: &str, elf: &[u8], args: &str, stdout: Output) -> Result<ExitStatus, ExecError> {
    let pid = spawn(name, elf, args, stdout)?;
    Ok(wait(pid).expect("process vanished before wait"))
}

/// Runs `f` on the process the current thread belongs to.
pub fn with_current<R>(f: impl FnOnce(&mut Process) -> R) -> Option<R> {
    without_interrupts(|| PROCESSES.lock().get_mut(&task::current_id()).map(f))
}

/// Ends the current process: records `status`, leaves and frees its address
/// space and exits the thread.
pub fn exit_current(status: ExitStatus) -> ! {
    disable_interrupts();
    let pid = task::current_id();
    let space = {
        let mut procs = PROCESSES.lock();
        let p = procs.get_mut(&pid).expect("exit_current: not a user process");
        p.status = Some(status);
        p.space.take()
    };
    // Switch back to the kernel's page tables before freeing our own
    task::set_current_cr3(paging::kernel_root());
    drop(space);
    task::exit()
}

/// Ends the current process because of `reason` (called by exception handlers).
pub fn kill_current(reason: String) -> ! {
    let name = with_current(|p| p.name.clone()).unwrap_or_default();
    crate::serial_println!("process {} ({}) killed: {}", task::current_id(), name, reason);
    exit_current(ExitStatus::Killed(reason))
}

/// Asks process `pid` to end. It dies the next time it is interrupted in
/// user mode or returns from a system call. Returns false for unknown pids.
pub fn kill(pid: Pid) -> bool {
    without_interrupts(|| {
        PROCESSES
            .lock()
            .get_mut(&pid)
            .map(|p| p.kill_requested = true)
            .is_some()
    })
}

/// Called on the way back to user mode: honours a pending `kill`.
pub fn check_killed() {
    if with_current(|p| p.kill_requested) == Some(true) {
        kill_current(String::from("killed"));
    }
}

/// Number of processes in the table (running or not yet waited for).
pub fn count() -> usize {
    without_interrupts(|| PROCESSES.lock().len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time;
    use alloc::format;

    /// mov eax, nr
    fn mov_eax(nr: u32) -> [u8; 5] {
        let b = nr.to_le_bytes();
        [0xB8, b[0], b[1], b[2], b[3]]
    }

    fn mov_edi(v: u32) -> [u8; 5] {
        let b = v.to_le_bytes();
        [0xBF, b[0], b[1], b[2], b[3]]
    }

    const SYSCALL: [u8; 2] = [0x0F, 0x05];
    /// mov rdi, rax
    const RDI_FROM_RAX: [u8; 3] = [0x48, 0x89, 0xC7];

    /// `exit(rax)` - ends a program with the result of the last syscall.
    fn exit_with_rax() -> Vec<u8> {
        [&RDI_FROM_RAX[..], &mov_eax(syscall::nr::EXIT), &SYSCALL].concat()
    }

    fn run_code(code: &[u8], args: &str) -> (ExitStatus, String) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let elf = elf::build_test_elf(code, 64);
        let status = run("test-prog", &elf, args, Output::Capture(out.clone())).unwrap();
        let text = String::from_utf8(out.lock().clone()).unwrap();
        (status, text)
    }

    #[test_case]
    fn exit_code_is_returned() {
        let code = [&mov_eax(syscall::nr::EXIT)[..], &mov_edi(42), &SYSCALL].concat();
        assert_eq!(run_code(&code, "").0, ExitStatus::Exited(42));
        assert_eq!(count(), 0, "process not removed from the table");
    }

    #[test_case]
    fn write_reaches_stdout() {
        // write(1, "hello", 5); exit(rax)
        let mut code = [
            &mov_eax(syscall::nr::WRITE)[..],
            &mov_edi(1),
            &[0x48, 0x8D, 0x35, 17, 0, 0, 0], // lea rsi, [rip + 17] -> the string
            &[0xBA, 5, 0, 0, 0],              // mov edx, 5
            &SYSCALL,
        ]
        .concat();
        code.extend_from_slice(&exit_with_rax());
        code.extend_from_slice(b"hello");
        assert_eq!(run_code(&code, ""), (ExitStatus::Exited(5), String::from("hello")));
    }

    #[test_case]
    fn arguments_are_passed() {
        // write(1, rdi, rsi): mov rdx, rsi; mov rsi, rdi; ...
        let code = [
            &[0x48, 0x89, 0xF2][..],
            &[0x48, 0x89, 0xFE],
            &mov_edi(1),
            &mov_eax(syscall::nr::WRITE),
            &SYSCALL,
            &exit_with_rax(),
        ]
        .concat();
        assert_eq!(
            run_code(&code, "one two"),
            (ExitStatus::Exited(7), String::from("one two"))
        );
    }

    #[test_case]
    fn syscall_preserves_registers() {
        // rdi..r10 = 1000, 200, 30, 4, 5, 6; getpid(); exit(sum of them)
        let code = [
            &mov_edi(1000)[..],
            &[0xBE, 200, 0, 0, 0],     // mov esi, 200
            &[0xBA, 30, 0, 0, 0],      // mov edx, 30
            &[0x41, 0xB8, 4, 0, 0, 0], // mov r8d, 4
            &[0x41, 0xB9, 5, 0, 0, 0], // mov r9d, 5
            &[0x41, 0xBA, 6, 0, 0, 0], // mov r10d, 6
            &mov_eax(syscall::nr::GETPID),
            &SYSCALL,
            &[0x48, 0x01, 0xF7], // add rdi, rsi
            &[0x48, 0x01, 0xD7], // add rdi, rdx
            &[0x4C, 0x01, 0xC7], // add rdi, r8
            &[0x4C, 0x01, 0xCF], // add rdi, r9
            &[0x4C, 0x01, 0xD7], // add rdi, r10
            &mov_eax(syscall::nr::EXIT),
            &SYSCALL,
        ]
        .concat();
        assert_eq!(run_code(&code, "").0, ExitStatus::Exited(1245));
    }

    #[test_case]
    fn kernel_pointers_are_rejected() {
        // write(1, <kernel address>, 10) -> -EFAULT
        let kernel = (crate::init as fn() as usize as u64).to_le_bytes();
        let code = [
            &mov_eax(syscall::nr::WRITE)[..],
            &mov_edi(1),
            &[0x48, 0xBE],
            &kernel, // mov rsi, imm64
            &[0xBA, 10, 0, 0, 0],
            &SYSCALL,
            &exit_with_rax(),
        ]
        .concat();
        assert_eq!(run_code(&code, "").0, ExitStatus::Exited(-syscall::errno::EFAULT));
    }

    #[test_case]
    fn unknown_syscall_fails() {
        let code = [&mov_eax(9999)[..], &SYSCALL, &exit_with_rax()].concat();
        assert_eq!(run_code(&code, "").0, ExitStatus::Exited(-syscall::errno::ENOSYS));
    }

    fn assert_killed(code: &[u8], expected: &str) {
        match run_code(code, "").0 {
            ExitStatus::Killed(reason) => assert!(reason.contains(expected), "reason: {}", reason),
            other => panic!("expected a kill ({}), got {:?}", expected, other),
        }
    }

    #[test_case]
    fn faults_kill_only_the_process() {
        // mov [0], rax
        assert_killed(&[0x48, 0x89, 0x04, 0x25, 0, 0, 0, 0], "page fault at 0x0");
        // cli (privileged)
        assert_killed(&[0xFA], "general protection");
        // write into our own code: lea rax, [rip]; mov [rax], al
        assert_killed(
            &[0x48, 0x8D, 0x05, 0, 0, 0, 0, 0x88, 0x00],
            "protection violation, write",
        );
        // read kernel memory: mov rax, [kernel code]
        let kernel = (crate::init as fn() as usize as u64).to_le_bytes();
        assert_killed(&[&[0x48, 0xA1][..], &kernel].concat(), "protection violation, read");
        // ud2
        assert_killed(&[0x0F, 0x0B], "invalid opcode");
        // jump into the kernel
        let mut jmp = alloc::vec![0x48, 0xB8];
        jmp.extend_from_slice(&kernel);
        jmp.extend_from_slice(&[0xFF, 0xE0]); // mov rax, imm64; jmp rax
        assert_killed(&jmp, "instruction fetch");
        // the kernel still works
        assert_eq!(1 + 1, 2);
    }

    #[test_case]
    fn user_code_is_preempted_and_killable() {
        let elf = elf::build_test_elf(&[0xEB, 0xFE], 0); // jmp $ (never yields, never syscalls)
        let pid = spawn("spinner", &elf, "", Output::Console).unwrap();
        let start = time::ticks();
        time::sleep_ms(50); // only returns if the spinner gets preempted
        assert!(time::ticks() >= start + 5);
        let spun = task::list().iter().find(|t| t.id == pid).map_or(0, |t| t.cpu_ticks);
        assert!(spun > 0, "the spinner never ran");
        assert!(kill(pid));
        assert_eq!(wait(pid), Some(ExitStatus::Killed(String::from("killed"))));
    }

    #[test_case]
    fn sleep_syscall_blocks() {
        let code = [
            &mov_eax(syscall::nr::SLEEP_MS)[..],
            &mov_edi(50),
            &SYSCALL,
            &mov_eax(syscall::nr::EXIT),
            &[0x31, 0xFF], // xor edi, edi
            &SYSCALL,
        ]
        .concat();
        let start = time::ticks();
        assert_eq!(run_code(&code, "").0, ExitStatus::Exited(0));
        assert!(time::ticks() - start >= 5, "slept only {} ticks", time::ticks() - start);
    }

    #[test_case]
    fn processes_release_their_memory() {
        let code = [&mov_eax(syscall::nr::EXIT)[..], &mov_edi(0), &SYSCALL].concat();
        let elf = elf::build_test_elf(&code, 10_000);
        for _ in 0..3 {
            run("warmup", &elf, "", Output::Console).unwrap();
        }
        task::yield_now(); // let the scheduler reap the threads
        let before = crate::mem::pmm::stats().free_frames;
        for i in 0..20 {
            let status = run("leak-test", &elf, &format!("run {}", i), Output::Console).unwrap();
            assert_eq!(status, ExitStatus::Exited(0));
        }
        task::yield_now();
        let after = crate::mem::pmm::stats().free_frames;
        // a few frames for new page tables in the never-reused kernel stack area
        assert!(before.saturating_sub(after) <= 4, "leaked {} frames", before - after);
    }

    #[test_case]
    fn invalid_programs_are_refused() {
        assert_eq!(
            spawn("x", b"not an elf", "", Output::Console),
            Err(ExecError::Elf(ElfError::BadMagic))
        );
        let elf = elf::build_test_elf(&[0xEB, 0xFE], 0);
        let long = "x".repeat(MAX_ARGS_LEN + 1);
        assert_eq!(spawn("x", &elf, &long, Output::Console), Err(ExecError::ArgsTooLong));
        assert_eq!(wait(123_456), None);
    }
}
