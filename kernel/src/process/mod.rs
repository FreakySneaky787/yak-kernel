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
//! - right after the image: the heap, grown and shrunk with `brk` (max `HEAP_MAX`)
//! - `USER_STACK_TOP - USER_STACK_MAX .. USER_STACK_TOP`: the stack. The top
//!   `USER_STACK_PAGES` are mapped at start, the rest on first use (like the
//!   heap); below it nothing is mapped (an overflow faults and kills the process)
//! - the argument string sits at the top of the stack; `_start` receives a
//!   pointer to it in RDI and its length in RSI (our own minimal ABI).
//!
//! Every process has a current directory (inherited from whoever spawned it;
//! relative paths in system calls start there) and a table of file
//! descriptors: 0 = stdin, 1 = stdout, 2 = stderr, then open files/directories.
//!
//! Threads (M36): a process can start further threads (`spawn_thread`) in
//! its address space; they are kernel threads whose `owner` is the main
//! thread, whose id is the pid. The process ends when the main thread ends
//! or any thread calls `exit` / dies of a signal or fault: `exit_current`
//! marks it `exiting`, every further thread leaves on its next way back to
//! user mode (or when its blocking call gives up), and the main thread waits
//! for them before it frees the address space. Futexes: `futex.rs`.
//!
//! Processes can spawn children (`parent`), which only their parent may wait
//! for or kill. Children of a process that ends become orphans and are
//! removed from the table as soon as they end themselves.
//!
//! Signals (`signal.rs`): `send_signal` marks a signal pending; it is acted
//! on when the process returns to user mode (`deliver_signals`). `kill`
//! sends SIGKILL.
//!
//! Ctrl+C: one process is in the *foreground* (the one the console belongs
//! to). The keyboard/serial interrupt handlers call `interrupt_foreground`,
//! which sends it SIGINT - unless it asked for raw Ctrl+C (`ignore_interrupt`,
//! e.g. the editor) and has no SIGINT handler, in which case the key is
//! delivered as a normal byte (0x03). While the foreground process
//! waits for a child (`wait_child`), the child is in the foreground; the
//! console comes back when the wait ends. Children that nobody waits for
//! (background, orphans) never get it.

pub mod elf;
pub mod futex;
pub mod pipe;
pub mod signal;
pub mod syscall;

use crate::arch::x86_64::{disable_interrupts, without_interrupts};
use crate::fs::fat32::DirEntry;
use crate::mem::address_space::AddressSpace;
use crate::mem::paging::{self, Flags, MapError};
use crate::mem::{PAGE_SIZE, VirtAddr, align_down};
use crate::task::{self, Tid, UserEntry};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};
use elf::ElfError;
use signal::Signals;
use spin::Mutex;

pub type Pid = Tid;

/// Lowest address an ELF segment may use (keeps the first 64 KiB unmapped,
/// so null pointer accesses always fault).
pub const USER_IMAGE_START: u64 = 0x1_0000;
pub const USER_STACK_TOP: u64 = 0x0000_7FFF_FFFF_0000;
/// Stack pages mapped when a program starts.
pub const USER_STACK_PAGES: usize = 4;
/// Largest stack: pages below the initial ones are mapped on first use.
pub const USER_STACK_MAX: u64 = 1024 * 1024;
/// ELF segments must end below the stack and its unmapped guard gap.
pub const USER_IMAGE_END: u64 = USER_STACK_TOP - USER_STACK_MAX - 16 * PAGE_SIZE;
pub const MAX_ARGS_LEN: usize = 4096;
/// Largest heap a process may have.
pub const HEAP_MAX: u64 = 64 * 1024 * 1024;

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

/// Maximum number of file descriptors per process.
pub const MAX_FDS: usize = 32;

/// What a file descriptor refers to. Cloning a handle (dup, spawn) gives an
/// independent descriptor for the same object.
#[derive(Clone)]
pub enum Handle {
    /// Keyboard + serial input.
    Stdin,
    /// The process's `Output` (stdout and stderr).
    Stdout,
    /// A file; reads and writes happen at `offset` (writes with `append`
    /// always go to the end). The offset is shared by every descriptor
    /// made from this one by `dup` or inheritance (like POSIX).
    File {
        path: String,
        offset: Arc<AtomicU64>,
        readable: bool,
        writable: bool,
        append: bool,
    },
    /// A directory opened for `readdir`: a snapshot of its entries.
    Dir { entries: Vec<DirEntry>, next: usize },
    /// One end of a pipe.
    Pipe(pipe::PipeEnd),
    /// A bound UDP port (`net`); unbound when the last descriptor closes.
    Udp(Arc<crate::net::UdpSocket>),
    /// A TCP connection; closed (FIN) when the last descriptor closes.
    Tcp(Arc<TcpConnection>),
    /// A listening TCP port.
    TcpListener(Arc<TcpListener>),
}

/// Owner of a TCP connection for descriptors (see `net::tcp`).
pub struct TcpConnection(pub crate::net::tcp::ConnId);

impl Drop for TcpConnection {
    fn drop(&mut self) {
        crate::net::tcp::close(self.0);
    }
}

/// Owner of a listening port for descriptors.
pub struct TcpListener(pub u16);

impl Drop for TcpListener {
    fn drop(&mut self) {
        crate::net::tcp::unlisten(self.0);
    }
}

/// The descriptor table of a process started without a parent.
fn standard_files() -> Vec<Option<Handle>> {
    alloc::vec![Some(Handle::Stdin), Some(Handle::Stdout), Some(Handle::Stdout)]
}

pub struct Process {
    pub name: String,
    /// Absolute, normalized current directory.
    pub cwd: String,
    /// Index = file descriptor.
    pub files: Vec<Option<Handle>>,
    /// `None` once the process has ended (or while `brk` maps pages).
    space: Option<AddressSpace>,
    /// Heap: `[heap_start, heap_end)`, pages mapped up to `heap_end` rounded up.
    pub heap_start: u64,
    pub heap_end: u64,
    stdout: Output,
    /// Bytes of a key that did not fit into the last `read` buffer.
    stdin_pending: Vec<u8>,
    status: Option<ExitStatus>,
    /// Dispositions and pending signals (acted on when returning to user mode).
    pub signals: Signals,
    /// `None` for processes started by the kernel.
    pub parent: Option<Pid>,
    /// The parent ended: nobody will wait, remove the entry at exit.
    orphan: bool,
    /// Ctrl+C is delivered as a byte instead of killing the process.
    ignore_interrupt: bool,
    /// Environment variables (inherited by children at `spawn`).
    pub env: BTreeMap<String, String>,
    /// Further threads: `None` while running, the exit code until joined.
    threads: BTreeMap<Tid, Option<i64>>,
    /// The process is ending with this status (see `exit_current`).
    exiting: Option<ExitStatus>,
}

/// The environment of processes started by the kernel.
pub fn default_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(String::from("PATH"), String::from("/bin"));
    env.insert(String::from("HOME"), String::from("/"));
    env
}

impl Process {
    pub fn set_ignore_interrupt(&mut self, on: bool) {
        self.ignore_interrupt = on;
    }

    /// Number of further threads that still run.
    pub fn thread_count(&self) -> usize {
        self.threads.values().filter(|r| r.is_none()).count()
    }

    /// Whether `addr` lies in a demand-paged range: the heap (below its end
    /// rounded up to a page) or the stack area.
    fn on_demand(&self, addr: u64) -> bool {
        let heap = addr >= self.heap_start && addr < crate::mem::align_up(self.heap_end, PAGE_SIZE);
        heap || (USER_STACK_TOP - USER_STACK_MAX..USER_STACK_TOP).contains(&addr)
    }

    /// Maps zeroed pages for every not yet used heap or stack page in
    /// `[addr, addr + len)` (demand paging). False if memory ran out.
    /// The walk stops at the first page that is neither mapped for user mode
    /// nor demand-paged: the access check after it fails there anyway, and a
    /// huge `len` must not keep the kernel walking (bug #11).
    pub fn populate(&mut self, addr: u64, len: u64) -> bool {
        let Some(end) = addr.checked_add(len) else { return true };
        let mut page = align_down(addr, PAGE_SIZE);
        while page < end {
            let on_demand = self.on_demand(page);
            let Some(space) = self.space.as_mut() else { return false };
            if !space
                .page_flags(VirtAddr(page))
                .is_some_and(|f| f.contains(Flags::USER))
            {
                if !on_demand {
                    return true;
                }
                if space
                    .page_frame(VirtAddr(page), Flags::WRITABLE | Flags::NO_EXECUTE)
                    .is_err()
                {
                    return false;
                }
            }
            page += PAGE_SIZE;
        }
        true
    }
}

/// Page fault in user mode: a not-present page inside the heap or the stack area gets a fresh
/// zeroed page and the instruction is retried. Returns false for every
/// other fault (the caller kills the process). Runs in the exception handler.
pub fn handle_page_fault(addr: u64, error_code: u64) -> bool {
    const PRESENT: u64 = 1;
    if error_code & PRESENT != 0 {
        return false; // a protection violation, not a missing page
    }
    with_current(|p| p.on_demand(addr) && p.populate(addr, 1)).unwrap_or(false)
}

/// How to start a process (see `spawn_with`).
pub struct Options<'a> {
    /// Absolute, normalized current directory of the new process.
    pub cwd: &'a str,
    pub stdout: Output,
    pub parent: Option<Pid>,
    /// Make the new process the foreground process (Ctrl+C goes to it).
    pub foreground: bool,
    pub ignore_interrupt: bool,
    /// Descriptors 0..2 for the new process (default: console in/out).
    pub std_files: Option<[Option<Handle>; 3]>,
    /// Environment (default: `default_env`).
    pub env: Option<BTreeMap<String, String>>,
    /// Signal dispositions (default: all default).
    pub signals: Option<Signals>,
}

impl<'a> Options<'a> {
    pub fn new(stdout: Output) -> Options<'a> {
        Options {
            cwd: "/",
            stdout,
            parent: None,
            foreground: false,
            ignore_interrupt: false,
            std_files: None,
            env: None,
            signals: None,
        }
    }
}

/// Pid of the foreground process (0 = none).
static FOREGROUND: AtomicU64 = AtomicU64::new(0);

pub fn foreground() -> Pid {
    FOREGROUND.load(Ordering::Relaxed)
}

/// All processes that have not been waited for yet. Only touched with
/// interrupts disabled (exception handlers and the timer use it too).
static PROCESSES: Mutex<BTreeMap<Pid, Process>> = Mutex::new(BTreeMap::new());

/// Loads `elf` into a new address space and starts it in the root
/// directory. `name` is used for `ps` and messages, `args` is handed to the program.
pub fn spawn(name: &str, elf: &[u8], args: &str, stdout: Output) -> Result<Pid, ExecError> {
    spawn_in("/", name, elf, args, stdout)
}

/// Like `spawn`, with `cwd` (absolute, normalized) as the current directory.
pub fn spawn_in(cwd: &str, name: &str, elf: &[u8], args: &str, stdout: Output) -> Result<Pid, ExecError> {
    spawn_with(
        name,
        elf,
        args,
        Options {
            cwd,
            ..Options::new(stdout)
        },
    )
}

/// Loads `elf` into a new address space and starts it as configured in `opts`.
pub fn spawn_with(name: &str, elf: &[u8], args: &str, opts: Options) -> Result<Pid, ExecError> {
    if args.len() > MAX_ARGS_LEN {
        return Err(ExecError::ArgsTooLong);
    }
    let image = elf::parse(elf)?;
    let image_end = image
        .segments
        .iter()
        .map(|s| s.vaddr + s.memsz)
        .max()
        .unwrap_or(USER_IMAGE_START);
    let heap_start = crate::mem::align_up(image_end, PAGE_SIZE);
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

    let process = Process {
        name: String::from(name),
        cwd: String::from(opts.cwd),
        files: opts.std_files.map_or_else(standard_files, |f| f.into_iter().collect()),
        space: Some(space),
        heap_start,
        heap_end: heap_start,
        stdout: opts.stdout,
        stdin_pending: Vec::new(),
        status: None,
        signals: opts.signals.unwrap_or_else(Signals::new),
        threads: BTreeMap::new(),
        exiting: None,
        parent: opts.parent,
        orphan: false,
        ignore_interrupt: opts.ignore_interrupt,
        env: opts.env.unwrap_or_else(default_env),
    };
    let root = process.space.as_ref().map(|s| s.root()).expect("just created");
    // The table lock is held until the entry exists: another CPU may start the new thread at
    // once, and its first syscall must find the process (bug #19; same order as `spawn_thread`).
    let pid = without_interrupts(|| {
        let mut procs = PROCESSES.lock();
        let pid = task::spawn_user(name, root, entry);
        procs.insert(pid, process);
        if opts.foreground {
            FOREGROUND.store(pid, Ordering::Relaxed);
        }
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
    run_in("/", name, elf, args, stdout)
}

/// `spawn_in` and `wait` in one go.
pub fn run_in(cwd: &str, name: &str, elf: &[u8], args: &str, stdout: Output) -> Result<ExitStatus, ExecError> {
    let pid = spawn_in(cwd, name, elf, args, stdout)?;
    Ok(wait(pid).expect("process vanished before wait"))
}

/// Runs `f` on the process the current thread belongs to.
pub fn with_current<R>(f: impl FnOnce(&mut Process) -> R) -> Option<R> {
    without_interrupts(|| PROCESSES.lock().get_mut(&task::current_owner()).map(f))
}

/// Most further threads a process may have at once.
pub const MAX_THREADS: usize = 64;

/// Starts a further thread of the current process at `entry` with stack
/// pointer `stack` and `arg` in RDI. Returns its id.
pub fn spawn_thread(entry: u64, stack: u64, arg: u64) -> Option<Tid> {
    let pid = task::current_owner();
    without_interrupts(|| {
        let mut procs = PROCESSES.lock();
        let p = procs.get_mut(&pid)?;
        let running = p.threads.values().filter(|r| r.is_none()).count();
        if p.exiting.is_some() || running >= MAX_THREADS {
            return None;
        }
        let root = p.space.as_ref()?.root();
        let name = alloc::format!("{}/t", p.name);
        let user = UserEntry {
            rip: entry,
            rsp: stack,
            arg0: arg,
            arg1: 0,
        };
        // inserted before the lock is released: the thread cannot look itself up earlier
        let tid = task::spawn_user_thread(&name, root, user, pid);
        p.threads.insert(tid, None);
        Some(tid)
    })
}

/// Ends the current thread with `code`; for the main thread that is the
/// end of the process.
pub fn exit_thread(code: i64) -> ! {
    disable_interrupts();
    let (me, pid) = (task::current_id(), task::current_owner());
    if me == pid {
        exit_current(ExitStatus::Exited(code));
    }
    if let Some(p) = PROCESSES.lock().get_mut(&pid) {
        p.threads.insert(me, Some(code));
    }
    task::exit()
}

/// Waits for further thread `tid` of the current process and returns its
/// exit code. `None` if it is not one (or was joined already), `Some(Err)`
/// if the wait was interrupted.
pub fn join_thread(tid: Tid) -> Option<Result<i64, WaitError>> {
    let me = task::current_id();
    if tid == me || !with_current(|p| p.threads.contains_key(&tid))? {
        return None;
    }
    if !task::join_or_abort(tid, signal_pending) {
        return Some(Err(WaitError::Interrupted));
    }
    // the thread is gone; its code is recorded unless it died with the process
    with_current(|p| p.threads.remove(&tid))
        .flatten()
        .map(|code| Ok(code.unwrap_or(-1)))
}

/// Ends the current process with `status` (if several threads end it, the
/// first status counts). A further thread only starts the end: it wakes the
/// others and leaves. The main thread waits until all further threads are
/// gone, then records the status, frees the address space and exits.
pub fn exit_current(status: ExitStatus) -> ! {
    disable_interrupts();
    let (me, pid) = (task::current_id(), task::current_owner());
    let others: Vec<Tid> = {
        let mut procs = PROCESSES.lock();
        let p = procs.get_mut(&pid).expect("exit_current: not a user process");
        if p.exiting.is_none() {
            p.exiting = Some(status);
        }
        p.threads
            .iter()
            .filter(|&(&t, r)| r.is_none() && t != me)
            .map(|(&t, _)| t)
            .collect()
    };
    for &t in &others {
        task::wake(t); // a blocking call gives up (`signal_pending`)
    }
    if me != pid {
        task::wake(pid);
        task::exit(); // the main thread finishes the process
    }
    for t in others {
        task::join(t);
    }
    let space = {
        let mut procs = PROCESSES.lock();
        let p = procs.get_mut(&pid).expect("exit_current: not a user process");
        p.status = p.exiting.take();
        p.files.clear();
        let space = p.space.take();
        let (parent, orphan) = (p.parent, p.orphan);
        // The console goes back to the parent (if it is still alive)
        if FOREGROUND.load(Ordering::Relaxed) == pid {
            let next = parent.filter(|pp| procs.get(pp).is_some_and(|q| q.status.is_none()));
            FOREGROUND.store(next.unwrap_or(0), Ordering::Relaxed);
        }
        // Our children: finished ones are dropped, running ones become orphans
        procs.retain(|_, c| c.parent != Some(pid) || c.status.is_none());
        for c in procs.values_mut().filter(|c| c.parent == Some(pid)) {
            c.orphan = true;
        }
        if orphan {
            procs.remove(&pid); // nobody will wait for us
        }
        space
    };
    // Switch back to the kernel's page tables before freeing our own
    task::set_current_cr3(paging::kernel_root());
    drop(space);
    task::exit()
}

/// Ends the current process because of `reason` (called by exception handlers).
pub fn kill_current(reason: String) -> ! {
    let name = with_current(|p| p.name.clone()).unwrap_or_default();
    crate::serial_println!("process {} ({}) killed: {}", task::current_owner(), name, reason);
    exit_current(ExitStatus::Killed(reason))
}

/// Asks process `pid` to end (SIGKILL). It dies the next time it is
/// interrupted in user mode or returns from a system call; a blocked process
/// is woken up. Returns false for unknown or already ended pids.
pub fn kill(pid: Pid) -> bool {
    send_signal(pid, signal::SIGKILL)
}

/// Makes `sig` pending for process `pid` and wakes it (a blocking system
/// call then returns `EINTR`). Returns false for unknown or already ended
/// pids; a signal the process ignores is dropped (but still returns true).
pub fn send_signal(pid: Pid, sig: u32) -> bool {
    without_interrupts(|| {
        let posted = match PROCESSES.lock().get_mut(&pid).filter(|p| p.status.is_none()) {
            Some(p) => p.signals.post(sig),
            None => return false,
        };
        if posted {
            task::wake(pid);
        }
        true
    })
}

/// Ctrl+C, called by the keyboard/serial interrupt handlers. Returns true if
/// the key was consumed (SIGINT was sent), false if it should be delivered
/// as input.
pub fn interrupt_foreground() -> bool {
    let pid = FOREGROUND.load(Ordering::Relaxed);
    if pid == 0 {
        return false;
    }
    let raw = without_interrupts(|| {
        PROCESSES
            .lock()
            .get(&pid)
            .is_none_or(|p| p.ignore_interrupt && !p.signals.is_handled(signal::SIGINT))
    });
    !raw && send_signal(pid, signal::SIGINT)
}

/// Whether the current process has a signal that needs attention (blocking
/// system calls give up and return `EINTR`).
pub fn signal_pending() -> bool {
    with_current(|p| p.signals.is_pending() || p.exiting.is_some()).unwrap_or(false)
}

/// Called on the way back to user mode (after a system call or an interrupt
/// from user mode) with the user's RIP, RSP and RFLAGS: ends the process for
/// a fatal signal, or redirects it to its signal trampoline (see `signal`).
pub fn deliver_signals(rip: &mut u64, rsp: &mut u64, rflags: u64) {
    // the process is ending: no thread may continue in user mode
    if with_current(|p| p.exiting.is_some()) == Some(true) {
        exit_current(ExitStatus::Exited(0)); // the recorded status wins
    }
    loop {
        let Some(action) = with_current(|p| p.signals.take()).flatten() else {
            return;
        };
        match action {
            signal::Action::Terminate(sig) => kill_current(String::from(signal::reason(sig))),
            signal::Action::Handle {
                sig,
                handler,
                trampoline,
            } => {
                // sig, handler, rflags, rip: what the trampoline expects at its RSP
                let frame = [sig as u64, handler, rflags, *rip];
                let sp = rsp.wrapping_sub(size_of_val(&frame) as u64);
                let bytes = unsafe { core::slice::from_raw_parts(frame.as_ptr() as *const u8, size_of_val(&frame)) };
                if syscall::write_user(sp, bytes).is_err() {
                    kill_current(String::from("bad stack for a signal handler"));
                }
                *rip = trampoline;
                *rsp = sp;
                return;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    /// No such process, or not a child of the caller.
    NotAChild,
    /// The waiting process itself was killed.
    Interrupted,
}

/// `wait` for the system call: only for children of the current process,
/// and aborted when the caller gets killed. With `no_hang`, a child that is
/// still running gives `Ok(None)` at once (for background jobs).
pub fn wait_child(pid: Pid, no_hang: bool) -> Result<Option<ExitStatus>, WaitError> {
    let me = task::current_owner();
    let (is_child, finished) = without_interrupts(|| {
        PROCESSES
            .lock()
            .get(&pid)
            .map_or((false, false), |p| (p.parent == Some(me), p.status.is_some()))
    });
    if !is_child {
        return Err(WaitError::NotAChild);
    }
    if no_hang && !finished {
        return Ok(None);
    }
    // Hand the console to the child for the duration of the wait
    let lend = FOREGROUND
        .compare_exchange(me, pid, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok();
    let joined = task::join_or_abort(pid, signal_pending);
    if lend {
        let _ = FOREGROUND.compare_exchange(pid, me, Ordering::Relaxed, Ordering::Relaxed);
        let _ = FOREGROUND.compare_exchange(0, me, Ordering::Relaxed, Ordering::Relaxed);
    }
    if !joined {
        return Err(WaitError::Interrupted);
    }
    without_interrupts(|| PROCESSES.lock().remove(&pid))
        .and_then(|p| p.status)
        .map(Some)
        .ok_or(WaitError::NotAChild)
}

/// Whether thread `tid` is a user process (still in the table).
pub fn is_process(tid: Tid) -> bool {
    without_interrupts(|| PROCESSES.lock().contains_key(&tid))
}

/// Parent of process `pid`, if it is a process with a parent.
pub fn parent_of(pid: Pid) -> Option<Pid> {
    without_interrupts(|| PROCESSES.lock().get(&pid).and_then(|p| p.parent))
}

/// Starts the first user process: `path` in `/`, in the foreground and
/// immune to Ctrl+C (it is the shell). Waits until it ends.
pub fn run_init(path: &str) -> Result<ExitStatus, alloc::string::String> {
    let image = crate::fs::with_root(|fs| fs.read_file(path)).map_err(|e| alloc::format!("{}: {}", path, e))?;
    let opts = Options {
        foreground: true,
        ignore_interrupt: true,
        ..Options::new(Output::Console)
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    let pid = spawn_with(name, &image, "", opts).map_err(|e| alloc::format!("{}: {}", path, e))?;
    Ok(wait(pid).expect("init vanished"))
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

    /// Regression test for bug #19: a new process's first syscall must find its process table
    /// entry. The thread used to be made runnable before the entry was inserted, so another CPU
    /// could start it in between - `brk` then failed with EBADF and the program's allocator broke.
    #[test_case]
    fn first_syscall_of_a_new_process_finds_its_table_entry() {
        // exit(brk(0)): the heap end on success, -EBADF if the process was not in the table yet
        let code = [&mov_eax(syscall::nr::BRK)[..], &mov_edi(0), &SYSCALL, &exit_with_rax()].concat();
        let elf = elf::build_test_elf(&code, 64);
        for round in 0..40 {
            let pids: Vec<Pid> = (0..6)
                .map(|_| spawn("brk-first", &elf, "", Output::Capture(Arc::new(Mutex::new(Vec::new())))).unwrap())
                .collect();
            for pid in pids {
                match wait(pid).unwrap() {
                    ExitStatus::Exited(end) => assert!(end > 0, "round {}: first brk returned {}", round, end),
                    other => panic!("round {}: {:?}", round, other),
                }
            }
        }
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

    /// brk(0) -> rax = heap start; mov rbx, rax
    fn heap_start_to_rbx() -> Vec<u8> {
        [
            &mov_eax(syscall::nr::BRK)[..],
            &[0x31, 0xFF],
            &SYSCALL,
            &[0x48, 0x89, 0xC3],
        ]
        .concat()
    }

    #[test_case]
    fn brk_grows_and_shrinks_the_heap() {
        let code = [
            &heap_start_to_rbx()[..],
            &[0x48, 0x8D, 0xBB, 0x00, 0x30, 0, 0], // lea rdi, [rbx + 0x3000]
            &mov_eax(syscall::nr::BRK),
            &SYSCALL,
            &[0xC6, 0x83, 0xFF, 0x2F, 0, 0, 1], // mov byte [rbx + 0x2FFF], 1 (last heap byte)
            &[0x48, 0x89, 0xDF],                // mov rdi, rbx
            &mov_eax(syscall::nr::BRK),
            &SYSCALL,         // shrink back to nothing
            &[0xC6, 0x03, 1], // mov byte [rbx], 1 -> must fault now
        ]
        .concat();
        // test ELF: code page at 0x11000, data page at 0x12000 -> heap starts at 0x13000
        let heap_start = USER_IMAGE_START + 0x3000;
        match run_code(&code, "").0 {
            ExitStatus::Killed(r) => assert!(r.contains(&format!("page fault at {:#x}", heap_start)), "{}", r),
            other => panic!("expected a page fault after shrinking, got {:?}", other),
        }
    }

    #[test_case]
    fn brk_respects_its_limits() {
        for bad in [
            USER_STACK_TOP,
            USER_IMAGE_START,
            USER_IMAGE_START + 0x3000 + HEAP_MAX + 1,
        ] {
            let code = [
                &[0x48, 0xBF][..],
                &bad.to_le_bytes(), // mov rdi, imm64
                &mov_eax(syscall::nr::BRK),
                &SYSCALL,
                &exit_with_rax(),
            ]
            .concat();
            assert_eq!(
                run_code(&code, "").0,
                ExitStatus::Exited(-syscall::errno::ENOMEM),
                "brk({:#x})",
                bad
            );
        }
    }

    #[test_case]
    fn heap_memory_is_released() {
        // grow the heap by 1 MiB and exit without shrinking it
        let code = [
            &heap_start_to_rbx()[..],
            &[0x48, 0x8D, 0xBB, 0x00, 0x00, 0x10, 0], // lea rdi, [rbx + 0x100000]
            &mov_eax(syscall::nr::BRK),
            &SYSCALL,
            &[0xC6, 0x83, 0xFF, 0xFF, 0x0F, 0, 1], // touch the last byte
            &mov_eax(syscall::nr::EXIT),
            &[0x31, 0xFF],
            &SYSCALL,
        ]
        .concat();
        assert_eq!(run_code(&code, "").0, ExitStatus::Exited(0));
        task::yield_now();
        let before = crate::mem::pmm::stats().free_frames;
        assert_eq!(run_code(&code, "").0, ExitStatus::Exited(0));
        task::yield_now();
        let after = crate::mem::pmm::stats().free_frames;
        assert!(before.saturating_sub(after) <= 4, "leaked {} frames", before - after);
    }

    #[test_case]
    fn ctrl_c_kills_the_foreground_process() {
        let elf = elf::build_test_elf(&[0xEB, 0xFE], 0); // jmp $
        let opts = Options {
            foreground: true,
            ..Options::new(Output::Console)
        };
        let pid = spawn_with("spinner", &elf, "", opts).unwrap();
        assert_eq!(foreground(), pid);
        time::sleep_ms(20);
        assert!(interrupt_foreground(), "Ctrl+C not consumed");
        assert_eq!(wait(pid), Some(ExitStatus::Killed(String::from("interrupted"))));
        assert_eq!(foreground(), 0, "the console was not given back");
        assert!(!interrupt_foreground(), "no foreground process: the key is input");

        // a process that ignores interrupts gets the key as a byte instead
        let opts = Options {
            foreground: true,
            ignore_interrupt: true,
            ..Options::new(Output::Console)
        };
        let pid = spawn_with("shell-like", &elf, "", opts).unwrap();
        assert!(!interrupt_foreground());
        assert!(kill(pid));
        assert_eq!(wait(pid), Some(ExitStatus::Killed(String::from("killed"))));
    }

    #[test_case]
    fn stack_grows_on_demand_up_to_its_limit() {
        // sub rsp, n; mov byte [rsp], 1; add rsp, n; exit(0)
        let touch_below = |n: u32| {
            let b = n.to_le_bytes();
            [
                &[0x48, 0x81, 0xEC, b[0], b[1], b[2], b[3]][..],
                &[0xC6, 0x04, 0x24, 1],
                &[0x48, 0x81, 0xC4, b[0], b[1], b[2], b[3]],
                &mov_eax(syscall::nr::EXIT),
                &[0x31, 0xFF],
                &SYSCALL,
            ]
            .concat()
        };
        assert_eq!(run_code(&touch_below(768 * 1024), "").0, ExitStatus::Exited(0));
        match run_code(&touch_below(2 * 1024 * 1024), "").0 {
            ExitStatus::Killed(reason) => assert!(reason.contains("page fault"), "{}", reason),
            other => panic!("overflow beyond USER_STACK_MAX not caught: {:?}", other),
        }
    }

    #[test_case]
    fn heap_pages_are_mapped_on_first_use() {
        // brk(start + 32 MiB), touch three bytes, sleep 300 ms (we measure meanwhile), exit
        let code = [
            &heap_start_to_rbx()[..],
            &[0x48, 0x8D, 0xBB, 0x00, 0x00, 0x00, 0x02], // lea rdi, [rbx + 0x2000000]
            &mov_eax(syscall::nr::BRK),
            &SYSCALL,
            &[0xC6, 0x03, 1],                         // mov byte [rbx], 1
            &[0xC6, 0x83, 0x00, 0x00, 0x00, 0x01, 1], // mov byte [rbx + 0x1000000], 1
            &[0xC6, 0x83, 0xFF, 0xFF, 0xFF, 0x01, 1], // mov byte [rbx + 0x1FFFFFF], 1
            &mov_eax(syscall::nr::SLEEP_MS),
            &mov_edi(300),
            &SYSCALL,
            &mov_eax(syscall::nr::EXIT),
            &[0x31, 0xFF],
            &SYSCALL,
        ]
        .concat();
        let elf = elf::build_test_elf(&code, 64);
        let free = || crate::mem::pmm::stats().free_frames;
        let before = free();
        let pid = spawn("demand", &elf, "", Output::Console).unwrap();
        time::sleep_ms(150); // the program is asleep now
        let used = before.saturating_sub(free());
        assert_eq!(wait(pid), Some(ExitStatus::Exited(0)));
        // stack + image + page tables + 3 heap pages, nowhere near 8192 heap pages
        assert!(used < 100, "a 32 MiB heap with 3 touched pages used {} frames", used);
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
