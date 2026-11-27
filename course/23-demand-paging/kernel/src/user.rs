//! Ring 3: code that runs with the CPU's lowest privilege, and the door back into the kernel - a system call.
//!
//! The kernel runs in ring 0 and may do everything. Code in ring 3 may not use privileged instructions (`cli`,
//! `hlt`, writing CR3, port I/O: a general protection fault) and may not touch pages without the USER bit (a page
//! fault). So a broken or hostile program can only hurt itself: its fault ends it, and the kernel goes on.
//!
//! Chapter 20 ran one program at a time, machine code copied into three pages of the kernel's own page tables.
//! Since chapter 21 a program is a file - an ELF file (`elf.rs`) - and gets an address space of its own
//! (`address_space.rs`): `spawn` loads it, `Program::join` waits for it and gives its memory back. Any number of
//! programs run at once, all of them at the same virtual addresses, none of them able to see another's pages.
//! A program asks the kernel for something with `int 0x80`: the number of the call in RAX, its arguments in RDI, RSI
//! and RDX, the result back in RAX (the registers Linux uses, so the code looks familiar). Since chapter 22 the calls
//! on files live in `files.rs`.

use crate::address_space::{AddressSpace, USER_END};
use crate::elf::{self, ElfError};
use crate::fat32::FsError;
use crate::gdt::{USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use crate::mem::{PAGE_SIZE, align_down, align_up};
use crate::paging::{self, Flags, MapError};
use crate::{console, demand, files, fs, interrupts, task};
use alloc::collections::BTreeMap;
use alloc::string::String;
use core::arch::{asm, global_asm};
use spin::Mutex;

/// The stack of a program: `STACK_PAGES` pages below `STACK_TOP`, mapped when the program starts.
const STACK_TOP: u64 = 0x7FFF_FFFF_0000;
const STACK_PAGES: u64 = 4;

/// The vector of `int 0x80`, and the system calls: their numbers, and the errors they return (as negative numbers,
/// with Linux's values: EFAULT, ENOSYS).
pub const SYSCALL_VECTOR: u8 = 0x80;
const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 2;
const SYS_READ: u64 = 3;
const SYS_OPEN: u64 = 4;
const SYS_CLOSE: u64 = 5;
const SYS_SEEK: u64 = 6;
const SYS_BRK: u64 = 7;
const NO_SUCH_CALL: u64 = -38i64 as u64;

/// How a program ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// It called `exit` with this code.
    Code(u64),
    /// The CPU stopped it: a fault in ring 3.
    Killed(&'static str),
}

/// Why a program did not start. (The causes are read through `Debug`, which the compiler does not count.)
#[derive(Debug)]
#[allow(dead_code)]
pub enum SpawnError {
    /// No file system is mounted.
    NoDisk,
    /// The file could not be read.
    File(FsError),
    /// The file is no program we can load.
    Elf(ElfError),
    /// Memory ran out (or the file asked for memory it may not have).
    Memory(MapError),
}

impl From<ElfError> for SpawnError {
    fn from(error: ElfError) -> SpawnError {
        SpawnError::Elf(error)
    }
}

impl From<MapError> for SpawnError {
    fn from(error: MapError) -> SpawnError {
        SpawnError::Memory(error)
    }
}

/// How the programs that ended ended, by the id of their thread. A program is a thread that happens to run in ring 3;
/// `end` puts its exit here, `Program::join` takes it out.
static EXITS: Mutex<BTreeMap<task::Id, Exit>> = Mutex::new(BTreeMap::new());
/// What programs wrote - the tests read it.
pub(crate) static OUTPUT: Mutex<String> = Mutex::new(String::new());

/// A program that was started: its thread, and the address space the thread runs in.
///
/// The address space must stay alive as long as the thread runs - its frames are the program's code and stack. So
/// the only way to get rid of a `Program` is `join`, which waits for the thread to end first.
#[must_use = "a program must be joined: its memory is freed only then"]
pub struct Program {
    thread: task::Id,
    space: AddressSpace,
}

/// Starts the program in the ELF file `image`: a new address space, the file's segments and a stack in it, and a
/// thread that goes to ring 3 at the file's entry point. Returns at once; the program runs concurrently.
pub fn spawn(image: &[u8]) -> Result<Program, SpawnError> {
    let elf = elf::parse(image)?;
    let mut space = AddressSpace::new().ok_or(MapError::OutOfFrames)?;
    // an error from here on drops `space`, which gives back what was mapped so far
    elf::load(&mut space, image, &elf)?;
    for page in 1..=STACK_PAGES {
        space.map_fresh(STACK_TOP - page * PAGE_SIZE, Flags::USER_DATA)?;
    }
    let entry = elf.entry;
    // the heap starts at the first page behind the last segment
    let heap = elf
        .segments
        .iter()
        .map(|s| align_up(s.vaddr + s.memsz, PAGE_SIZE))
        .max()
        .unwrap_or(PAGE_SIZE);
    let thread = task::spawn_in("user", space.root(), move || {
        demand::begin(heap); // by the program's own thread: before it is in ring 3, its heap exists
        enter(entry, STACK_TOP)
    });
    Ok(Program { thread, space })
}

impl Program {
    /// Waits until the program has ended, frees its memory and says how it ended.
    pub fn join(self) -> Exit {
        self.join_with(|_| ()).0
    }

    /// The same - but when the thread is gone and nothing changes any more, there is a last look into the program's
    /// memory first (the tests read what a program left there).
    pub fn join_with<T>(self, look: impl FnOnce(&AddressSpace) -> T) -> (Exit, T) {
        task::join(self.thread);
        files::forget(self.thread); // what it left open is closed
        demand::forget(self.thread);
        let seen = look(&self.space);
        // the thread is off its CPU, and that CPU has loaded other tables: the address space is nobody's now
        drop(self.space);
        let exit = interrupts::without_interrupts(|| EXITS.lock().remove(&self.thread));
        (exit.expect("a program ends by exit or by a fault"), seen)
    }
}

/// Starts the program in `image` and waits until it ends.
pub fn run(image: &[u8]) -> Result<Exit, SpawnError> {
    Ok(spawn(image)?.join())
}

/// Reads the file at `path` from the disk and runs it.
pub fn run_file(path: &str) -> Result<Exit, SpawnError> {
    let image = fs::FS
        .lock()
        .as_mut()
        .ok_or(SpawnError::NoDisk)?
        .read_file(path)
        .map_err(SpawnError::File)?;
    run(&image)
}

/// Goes to ring 3: `iretq` takes RIP, CS, RFLAGS, RSP and SS from the stack, as if returning from an interrupt that
/// came from there. CS and SS with privilege level 3 make it ring 3; RFLAGS 0x202 has interrupts on, so the timer
/// can take the CPU away from the program. The registers are cleared first: they hold kernel values.
fn enter(rip: u64, stack_top: u64) -> ! {
    unsafe {
        asm!(
            "push {ss}",
            "push {sp}",
            "push {flags}",
            "push {cs}",
            "push {ip}",
            "xor eax, eax", "xor ebx, ebx", "xor ecx, ecx", "xor edx, edx",
            "xor esi, esi", "xor edi, edi", "xor ebp, ebp",
            "xor r8d, r8d", "xor r9d, r9d", "xor r10d, r10d", "xor r11d, r11d",
            "xor r12d, r12d", "xor r13d, r13d", "xor r14d, r14d", "xor r15d, r15d",
            "iretq",
            ss = in(reg) USER_DATA_SELECTOR as u64,
            sp = in(reg) stack_top,
            flags = in(reg) 0x202u64,
            cs = in(reg) USER_CODE_SELECTOR as u64,
            ip = in(reg) rip,
            options(noreturn)
        )
    }
}

/// Ends the running program: from its `exit` call, or from a fault handler. The thread ends with it.
pub fn end(exit: Exit) -> ! {
    let me = task::current();
    interrupts::without_interrupts(|| EXITS.lock().insert(me, exit));
    task::exit()
}

/// The registers as `syscall_entry` saved them, lowest address first - and above them what the CPU pushed.
#[repr(C)]
struct Registers {
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rax: u64,
    // pushed by the CPU: where the program was, and its stack
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

// The entry of `int 0x80`. The CPU has switched to the thread's kernel stack (RSP0 of the TSS) and pushed SS, RSP,
// RFLAGS, CS and RIP of the program. We save the registers a Rust function may change (the others it keeps itself),
// hand their address to `syscall`, which writes the result into the saved RAX, and go back with `iretq`.
// 5 + 9 values of 8 bytes: the stack is 16-byte aligned again at the `call`, as the ABI wants.
global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "push rax", "push rcx", "push rdx", "push rsi", "push rdi", "push r8", "push r9", "push r10", "push r11",
    "mov rdi, rsp",
    "call {dispatch}",
    "pop r11", "pop r10", "pop r9", "pop r8", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rax",
    "iretq",
    dispatch = sym syscall,
);

unsafe extern "C" {
    pub fn syscall_entry();
}

/// One system call.
extern "C" fn syscall(registers: &mut Registers) {
    debug_assert_eq!(registers.cs & 3, 3, "only ring 3 calls int 0x80");
    // The gate is an interrupt gate: interrupts are off. A call may take long - a disk read waits for the disk's
    // interrupt - so it runs with them on, like the kernel code of any thread; the thread may even be taken off the
    // CPU in the middle of a call, and goes on later with its own kernel stack (RSP0 follows it, chapter 20).
    unsafe { asm!("sti", options(nomem, nostack)) };
    let (a, b, c) = (registers.rdi, registers.rsi, registers.rdx);
    registers.rax = match registers.rax {
        SYS_WRITE => files::to_register(files::write(a, b, c)),
        SYS_EXIT => end(Exit::Code(a)),
        SYS_READ => files::to_register(files::read(a, b, c)),
        SYS_OPEN => files::to_register(files::open(a, b, c)),
        SYS_CLOSE => files::to_register(files::close(a)),
        SYS_SEEK => files::to_register(files::seek(a, b, c)),
        SYS_BRK => files::to_register(demand::brk(a)),
        _ => NO_SUCH_CALL,
    };
}

/// What programs write to the console (file descriptors 1 and 2).
pub fn console_write(bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    console::_print(format_args!("{}", text));
    OUTPUT.lock().push_str(&text);
}

/// Whether the whole range lies in the lower half and on pages that allow at least `needed` - the page tables of the
/// running program decide, not the program.
fn range_allows(address: u64, length: u64, needed: Flags) -> bool {
    let Some(end) = address.checked_add(length) else {
        return false;
    };
    if end > USER_END {
        return false;
    }
    // the pages of the heap the program has not touched yet are no mistake: they are mapped now (chapter 23)
    demand::populate(address, length);
    let mut page = align_down(address, PAGE_SIZE);
    while page < end {
        if !paging::flags(page).is_some_and(|flags| flags.contains(needed)) {
            return false;
        }
        page += PAGE_SIZE;
    }
    true
}

/// The bytes of a program's memory the kernel may read. The kernel never trusts an address from a program: every byte
/// must be in the lower half and on a page the program may use - otherwise the call fails, and the kernel reads nothing.
pub fn user_bytes(address: u64, length: u64) -> Option<&'static [u8]> {
    if length == 0 {
        // nothing to read, so the address is nobody's business - and a null pointer must never reach `from_raw_parts`,
        // not even for an empty slice
        return Some(&[]);
    }
    range_allows(address, length, Flags::USER)
        .then(|| unsafe { core::slice::from_raw_parts(address as *const u8, length as usize) })
}

/// The same for memory the kernel is to *write* (a `read` into the program's buffer): the program must be allowed to
/// write there too. Else the kernel would write into the program's read-only code for anyone who asks.
pub fn user_bytes_mut(address: u64, length: u64) -> Option<&'static mut [u8]> {
    if length == 0 {
        return Some(&mut []);
    }
    range_allows(address, length, Flags::USER_WRITABLE)
        .then(|| unsafe { core::slice::from_raw_parts_mut(address as *mut u8, length as usize) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::{TestSegment, build_test_elf};
    use crate::pmm;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicU64, Ordering};

    const BAD_ADDRESS: u64 = (-files::EFAULT) as u64;

    // Machine code for the tests, assembled into the kernel. Each program expects what `image` makes of it: its
    // code at CODE, a page of data at DATA - fixed addresses, since a program has an address space of its own now.
    const CODE: u64 = 0x40_0000;
    const DATA: u64 = 0x40_1000;

    global_asm!(
        ".section .rodata.user_programs, \"a\"",
        // write "Hello from ring 3!\n", then exit(42)
        ".global user_hello, user_hello_end",
        "user_hello:",
        "mov eax, 1",
        "mov edi, 1",
        "lea rsi, [rip + 2f]",
        "mov edx, 19",
        "int 0x80",
        "mov eax, 2",
        "mov edi, 42",
        "int 0x80",
        "ud2",
        "2: .ascii \"Hello from ring 3!\\n\"",
        "user_hello_end:",
        // a privileged instruction
        ".global user_cli, user_cli_end",
        "user_cli:",
        "cli",
        "mov eax, 2",
        "xor edi, edi",
        "int 0x80",
        "user_cli_end:",
        // read a byte of the kernel
        ".global user_peek, user_peek_end",
        "user_peek:",
        "movabs rax, 0xffffffff80000000",
        "mov al, [rax]",
        "user_peek_end:",
        // write into its own code page, which is read-only
        ".global user_patch, user_patch_end",
        "user_patch:",
        "mov byte ptr [rip], 0x90",
        "user_patch_end:",
        // ask the kernel to print the kernel's memory; exit with what the call returned
        ".global user_bad_pointer, user_bad_pointer_end",
        "user_bad_pointer:",
        "mov eax, 1",
        "mov edi, 1",
        "movabs rsi, 0xffffffff80000000",
        "mov edx, 8",
        "int 0x80",
        "mov rdi, rax",
        "mov eax, 2",
        "int 0x80",
        "user_bad_pointer_end:",
        // a system call that does not exist; exit with what it returned
        ".global user_unknown, user_unknown_end",
        "user_unknown:",
        "mov eax, 99",
        "int 0x80",
        "mov rdi, rax",
        "mov eax, 2",
        "int 0x80",
        "user_unknown_end:",
        // the stack works: push 5, pop it, exit with it
        ".global user_stack, user_stack_end",
        "user_stack:",
        "push 5",
        "pop rdi",
        "mov eax, 2",
        "int 0x80",
        "user_stack_end:",
        // spin until the first byte of the data page (0x401000) is not 0, then exit(7)
        ".global user_spin, user_spin_end",
        "user_spin:",
        "2: cmp byte ptr [0x401000], 0",
        "je 2b",
        "mov eax, 2",
        "mov edi, 7",
        "int 0x80",
        "user_spin_end:",
        // write(address 0, length 0) - nothing, from nowhere; exit with what the call returned
        ".global user_null, user_null_end",
        "user_null:",
        "mov eax, 1",
        "mov edi, 1",
        "xor esi, esi",
        "xor edx, edx",
        "int 0x80",
        "mov rdi, rax",
        "mov eax, 2",
        "int 0x80",
        "user_null_end:",
        // put 1 into the first byte of the data page, spin while it is still 1, exit with what is there then
        ".global user_stamp_1, user_stamp_1_end",
        "user_stamp_1:",
        "mov byte ptr [0x401000], 1",
        "2: cmp byte ptr [0x401000], 1",
        "je 2b",
        "movzx edi, byte ptr [0x401000]",
        "mov eax, 2",
        "int 0x80",
        "user_stamp_1_end:",
        // the same with 2: the same addresses, the same code - in another address space
        ".global user_stamp_2, user_stamp_2_end",
        "user_stamp_2:",
        "mov byte ptr [0x401000], 2",
        "2: cmp byte ptr [0x401000], 2",
        "je 2b",
        "movzx edi, byte ptr [0x401000]",
        "mov eax, 2",
        "int 0x80",
        "user_stamp_2_end:",
        ".text",
    );

    /// The bytes between two labels of the `global_asm!` above.
    macro_rules! program {
        ($start:ident, $end:ident) => {{
            unsafe extern "C" {
                static $start: u8;
                static $end: u8;
            }
            let start = &raw const $start as usize;
            let end = &raw const $end as usize;
            unsafe { core::slice::from_raw_parts(start as *const u8, end - start) }
        }};
    }

    /// An ELF file around `code`: the code (readable, executable) at CODE, a page of zeros (readable, writable) at
    /// DATA.
    fn image(code: &[u8]) -> Vec<u8> {
        build_test_elf(
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
                    bytes: &[],
                    memsz: PAGE_SIZE,
                },
            ],
        )
    }

    fn run_code(code: &[u8]) -> Exit {
        run(&image(code)).expect("a program that starts")
    }

    #[test_case]
    fn a_program_writes_and_exits() {
        OUTPUT.lock().clear();
        assert_eq!(run_code(program!(user_hello, user_hello_end)), Exit::Code(42));
        assert_eq!(*OUTPUT.lock(), "Hello from ring 3!\n");
    }

    #[test_case]
    fn writing_nothing_is_fine_whatever_the_address() {
        // an empty range is nobody's memory: the address is never looked at (bug #12 of the reference kernel - a
        // slice of length 0 from a null pointer is already undefined behaviour, and the debug build panics)
        assert_eq!(run_code(program!(user_null, user_null_end)), Exit::Code(0));
        assert!(user_bytes(0, 0).is_some_and(|bytes| bytes.is_empty()));
        assert!(user_bytes(u64::MAX, 0).is_some_and(|bytes| bytes.is_empty()));
    }

    #[test_case]
    fn a_program_has_a_stack() {
        assert_eq!(run_code(program!(user_stack, user_stack_end)), Exit::Code(5));
    }

    #[test_case]
    fn privileged_instructions_end_the_program() {
        assert_eq!(
            run_code(program!(user_cli, user_cli_end)),
            Exit::Killed("general protection fault")
        );
    }

    #[test_case]
    fn kernel_memory_is_out_of_reach() {
        assert_eq!(run_code(program!(user_peek, user_peek_end)), Exit::Killed("page fault"));
        assert_eq!(
            run_code(program!(user_patch, user_patch_end)),
            Exit::Killed("page fault")
        );
    }

    #[test_case]
    fn system_calls_check_what_they_get() {
        assert_eq!(
            run_code(program!(user_bad_pointer, user_bad_pointer_end)),
            Exit::Code(BAD_ADDRESS)
        );
        assert_eq!(
            run_code(program!(user_unknown, user_unknown_end)),
            Exit::Code(NO_SUCH_CALL)
        );
    }

    #[test_case]
    fn user_bytes_looks_at_the_tables_of_the_running_thread() {
        // the checks of `user_bytes` read the page tables in use: so look from a thread that runs on those of a
        // program (a kernel thread - it only borrows the program's address space)
        let mut space = AddressSpace::new().unwrap();
        space.map_fresh(CODE, Flags::USER_CODE).unwrap();
        space.map_fresh(DATA, Flags::DATA).unwrap(); // a page the kernel may use, but the program may not
        let probe = task::spawn_in("probe", space.root(), || {
            assert_eq!(user_bytes(CODE, 16).map(|b| b.len()), Some(16));
            assert!(
                user_bytes(CODE, PAGE_SIZE + 1).is_none(),
                "runs into a page without the USER bit"
            );
            assert!(user_bytes(DATA, 1).is_none(), "no USER bit");
            assert!(user_bytes(USER_END - 8, 16).is_none(), "past the lower half");
            assert!(user_bytes(u64::MAX - 4, 8).is_none(), "wraps around");
            assert!(user_bytes(0x1000, 8).is_none(), "not mapped");
            assert!(user_bytes(0xFFFF_FFFF_8000_0000, 8).is_none(), "the kernel");
        });
        task::join(probe);
        drop(space);
    }

    #[test_case]
    fn the_timer_takes_the_cpu_away_from_ring_3() {
        // a program that never gives up the CPU - with KERNEL_SMP=1 this thread still runs because the timer
        // interrupts ring 3
        let mut program = spawn(&image(program!(user_spin, user_spin_end))).unwrap();
        crate::time::sleep_ms(50);
        let ended = interrupts::without_interrupts(|| EXITS.lock().contains_key(&program.thread));
        assert!(!ended, "the program spins");
        program.space.write(DATA, &[1]).unwrap(); // let it end
        assert_eq!(program.join(), Exit::Code(7));
    }

    #[test_case]
    fn each_cpu_switches_to_the_thread_s_own_kernel_stack() {
        // RSP0 is the stack of the thread that runs - this one, the test thread is main, which has no stack of its
        // own; so look from a new thread
        let thread = task::spawn("rsp0", || {
            let rsp0 = interrupts::without_interrupts(crate::gdt::kernel_stack);
            let here: u64;
            unsafe { asm!("mov {}, rsp", out(reg) here, options(nomem, nostack, preserves_flags)) };
            assert!(
                rsp0 > here && rsp0 - here < 8 * PAGE_SIZE,
                "rsp0 {:#x}, rsp {:#x}",
                rsp0,
                here
            );
        });
        task::join(thread);
    }

    /// Waits until the byte at `DATA` of the program's address space is `value` - the program has started.
    fn wait_for_byte(program: &Program, value: u8) {
        let mut byte = [0];
        loop {
            program.space.read(DATA, &mut byte).unwrap();
            if byte[0] == value {
                return;
            }
            crate::time::sleep_ms(1);
        }
    }

    #[test_case]
    fn two_programs_use_the_same_addresses_without_meeting() {
        // both programs have their code at 0x400000 and their data at 0x401000 and write their own number into the
        // first byte of it. If they shared the page, the one that wrote last would be what both of them find.
        let mut one = spawn(&image(program!(user_stamp_1, user_stamp_1_end))).unwrap();
        let mut two = spawn(&image(program!(user_stamp_2, user_stamp_2_end))).unwrap();
        wait_for_byte(&one, 1);
        wait_for_byte(&two, 2);
        assert_ne!(
            paging::translate_in(one.space.root(), DATA),
            paging::translate_in(two.space.root(), DATA)
        );
        // each program leaves its loop when its own byte changes - and exits with what is there
        one.space.write(DATA, &[10]).unwrap();
        two.space.write(DATA, &[20]).unwrap();
        assert_eq!((one.join(), two.join()), (Exit::Code(10), Exit::Code(20)));
    }

    #[test_case]
    fn a_program_comes_from_a_file() {
        // /bin/hello is made by `as` and `ld` from programs/hello.S, and copied to the disk by mkdisk.sh
        OUTPUT.lock().clear();
        assert_eq!(run_file("/bin/hello").unwrap(), Exit::Code(42));
        assert_eq!(*OUTPUT.lock(), "Hello from ring 3!\n");
        assert_eq!(run_file("/bin/cli").unwrap(), Exit::Killed("general protection fault"));
        // not every file is a program
        assert!(matches!(
            run_file("/hello.txt"),
            Err(SpawnError::Elf(ElfError::TooShort))
        ));
        assert!(matches!(run_file("/big.bin"), Err(SpawnError::Elf(ElfError::BadMagic))));
        assert!(matches!(run_file("/nothing"), Err(SpawnError::File(_))));
        assert!(matches!(run_file("/bin"), Err(SpawnError::File(_))));
    }

    #[test_case]
    fn a_program_that_cannot_start_leaves_nothing_behind() {
        let free = pmm::free_frames();
        assert!(matches!(
            spawn(b"not an ELF file, but long enough to be looked at ..."),
            Err(SpawnError::Elf(_))
        ));
        // two segments on one page: parsed fine, then the second cannot be mapped
        let two_on_a_page = build_test_elf(
            CODE,
            &[
                TestSegment {
                    vaddr: CODE,
                    flags: 5,
                    bytes: b"\xC3",
                    memsz: 1,
                },
                TestSegment {
                    vaddr: CODE + 0x100,
                    flags: 6,
                    bytes: b"x",
                    memsz: 1,
                },
            ],
        );
        assert!(matches!(
            spawn(&two_on_a_page),
            Err(SpawnError::Memory(MapError::AlreadyMapped))
        ));
        assert_eq!(pmm::free_frames(), free);
    }

    #[test_case]
    fn programs_give_their_memory_back() {
        let hello = image(program!(user_hello, user_hello_end));
        for _ in 0..2 {
            run(&hello).unwrap(); // warm up: the heap grows once, the first thread stacks are made
        }
        let free = pmm::free_frames();
        for _ in 0..10 {
            run(&hello).unwrap();
        }
        // a program takes about 13 frames (code, data, 4 stack pages, 7 tables) - ten of them would be 130. What may
        // be missing is the stack of the last thread, which is freed when the next one starts
        let lost = free.saturating_sub(pmm::free_frames());
        assert!(lost <= 16, "{} frames lost", lost);
    }

    #[test_case]
    fn the_tables_in_use_are_the_kernels_again_after_a_program() {
        let before = paging::read_cr3();
        run_code(program!(user_stack, user_stack_end));
        assert_eq!(paging::read_cr3(), before);
        assert_eq!(before, paging::kernel_root());
    }

    #[test_case]
    fn threads_of_programs_and_of_the_kernel_interleave() {
        // programs and kernel threads in one run queue: the switch between tables does not lose anyone
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let programs: Vec<Program> = (0..4)
            .map(|_| spawn(&image(program!(user_stack, user_stack_end))).unwrap())
            .collect();
        let threads: Vec<task::Id> = (0..4)
            .map(|_| {
                task::spawn("counter", || {
                    COUNT.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        for program in programs {
            assert_eq!(program.join(), Exit::Code(5));
        }
        for thread in threads {
            task::join(thread);
        }
        assert_eq!(COUNT.load(Ordering::SeqCst), 4);
    }
}
