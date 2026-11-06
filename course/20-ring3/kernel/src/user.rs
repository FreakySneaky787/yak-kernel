//! Ring 3: code that runs with the CPU's lowest privilege, and the door back into the kernel - a system call.
//!
//! The kernel runs in ring 0 and may do everything. Code in ring 3 may not use privileged instructions (`cli`,
//! `hlt`, writing CR3, port I/O: a general protection fault) and may not touch pages without the USER bit (a page
//! fault). So a broken or hostile program can only hurt itself: its fault ends it, and the kernel goes on.
//!
//! This chapter has one address space - the kernel's page tables - with three USER pages in the lower half: code,
//! data and a stack. One program runs at a time; chapter 21 of the plan would give each its own address space and
//! load programs from the disk. A program asks the kernel for something with `int 0x80`: the number of the call in
//! RAX, its arguments in RDI and RSI, the result back in RAX (the registers Linux uses, so the code looks familiar).

use crate::gdt::{USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use crate::mem::{PAGE_SIZE, PhysAddr, align_down};
use crate::paging::{self, Flags};
use crate::{console, interrupts, pmm, task};
use alloc::string::String;
use core::arch::{asm, global_asm};
use spin::{Mutex, Once};

/// Where a program lives: its code, a page of data, and a stack page below `STACK_TOP`.
pub const CODE: u64 = 0x40_0000;
pub const DATA: u64 = 0x40_1000;
pub const STACK_TOP: u64 = 0x80_0000;
/// Everything below this address is the lower half - the only part a program may hand to a system call.
const USER_END: u64 = 0x0000_8000_0000_0000;

/// The vector of `int 0x80`, and the system calls: their numbers, and the errors they return (as negative numbers,
/// with Linux's values: EFAULT, ENOSYS).
pub const SYSCALL_VECTOR: u8 = 0x80;
const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 2;
const BAD_ADDRESS: u64 = -14i64 as u64;
const NO_SUCH_CALL: u64 = -38i64 as u64;

/// How a program ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// It called `exit` with this code.
    Code(u64),
    /// The CPU stopped it: a fault in ring 3.
    Killed(&'static str),
}

struct Pages {
    code: PhysAddr,
    data: PhysAddr,
    stack: PhysAddr,
}

/// The three pages, mapped once and kept: a page that was unmapped on one CPU may still be in the TLB of another,
/// which would then use a frame that already belongs to someone else.
static PAGES: Once<Pages> = Once::new();
/// One program at a time: they share the three pages.
static RUNNING: Mutex<()> = Mutex::new(());
static EXIT: Mutex<Option<Exit>> = Mutex::new(None);
/// What programs wrote - the tests read it.
static OUTPUT: Mutex<String> = Mutex::new(String::new());

fn pages() -> &'static Pages {
    PAGES.call_once(|| {
        let frame = || pmm::alloc_frame_zeroed().expect("a frame for a ring 3 page");
        let pages = Pages {
            code: frame(),
            data: frame(),
            stack: frame(),
        };
        paging::map(CODE, pages.code, Flags::USER_CODE).expect("the code page is free");
        paging::map(DATA, pages.data, Flags::USER_DATA).expect("the data page is free");
        paging::map(STACK_TOP - PAGE_SIZE, pages.stack, Flags::USER_DATA).expect("the stack page is free");
        pages
    })
}

/// Runs `program` (machine code, at most a page) in ring 3 in a thread of its own, and waits until it ends.
pub fn run(program: &[u8]) -> Exit {
    let _one_at_a_time = RUNNING.lock();
    assert!(program.len() <= PAGE_SIZE as usize);
    let pages = pages();
    // the kernel writes the pages through the HHDM: the code page is read-only for the program
    unsafe {
        core::ptr::write_bytes(pages.code.as_mut_ptr::<u8>(), 0, PAGE_SIZE as usize);
        core::ptr::copy_nonoverlapping(program.as_ptr(), pages.code.as_mut_ptr::<u8>(), program.len());
        core::ptr::write_bytes(pages.data.as_mut_ptr::<u8>(), 0, PAGE_SIZE as usize);
        core::ptr::write_bytes(pages.stack.as_mut_ptr::<u8>(), 0, PAGE_SIZE as usize);
    }
    interrupts::without_interrupts(|| *EXIT.lock() = None);
    let thread = task::spawn("user", || enter(CODE, STACK_TOP));
    task::join(thread);
    interrupts::without_interrupts(|| EXIT.lock().take()).expect("a program ends by exit or by a fault")
}

/// The data page as the kernel sees it - a test changes a byte there while a program runs.
#[cfg(test)]
fn data() -> *mut u8 {
    pages().data.as_mut_ptr::<u8>()
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
    interrupts::without_interrupts(|| *EXIT.lock() = Some(exit));
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

/// One system call. Runs with interrupts off (the gate is an interrupt gate).
extern "C" fn syscall(registers: &mut Registers) {
    debug_assert_eq!(registers.cs & 3, 3, "only ring 3 calls int 0x80");
    registers.rax = match registers.rax {
        SYS_WRITE => write(registers.rdi, registers.rsi),
        SYS_EXIT => end(Exit::Code(registers.rdi)),
        _ => NO_SUCH_CALL,
    };
}

/// `write(address, length)`: prints the bytes. The kernel never trusts an address from a program: every byte must be
/// in the lower half and on a page the program may read - otherwise the call fails, and the kernel reads nothing.
fn write(address: u64, length: u64) -> u64 {
    let Some(bytes) = user_bytes(address, length) else {
        return BAD_ADDRESS;
    };
    let text = String::from_utf8_lossy(bytes);
    console::_print(format_args!("{}", text));
    OUTPUT.lock().push_str(&text);
    length
}

fn user_bytes(address: u64, length: u64) -> Option<&'static [u8]> {
    let end = address.checked_add(length)?;
    if end > USER_END {
        return None;
    }
    let mut page = align_down(address, PAGE_SIZE);
    while page < end {
        if !paging::flags(page)?.contains(Flags::USER) {
            return None;
        }
        page += PAGE_SIZE;
    }
    Some(unsafe { core::slice::from_raw_parts(address as *const u8, length as usize) })
}

// The programs: machine code, assembled into the kernel and copied to `CODE` by `run`. They only use addresses
// relative to RIP or inside their own pages, so they work at any address.
global_asm!(
    ".section .rodata.user_programs, \"a\"",
    // write "Hello from ring 3!\n", then exit(42)
    ".global user_hello, user_hello_end",
    "user_hello:",
    "mov eax, 1",
    "lea rdi, [rip + 2f]",
    "mov esi, 19",
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
    "movabs rdi, 0xffffffff80000000",
    "mov esi, 8",
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

pub fn hello() -> &'static [u8] {
    program!(user_hello, user_hello_end)
}

pub fn privileged() -> &'static [u8] {
    program!(user_cli, user_cli_end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};

    #[test_case]
    fn a_program_writes_and_exits() {
        OUTPUT.lock().clear();
        assert_eq!(run(hello()), Exit::Code(42));
        assert_eq!(*OUTPUT.lock(), "Hello from ring 3!\n");
    }

    #[test_case]
    fn a_program_has_a_stack() {
        assert_eq!(run(program!(user_stack, user_stack_end)), Exit::Code(5));
    }

    #[test_case]
    fn privileged_instructions_end_the_program() {
        assert_eq!(run(privileged()), Exit::Killed("general protection fault"));
    }

    #[test_case]
    fn kernel_memory_is_out_of_reach() {
        assert_eq!(run(program!(user_peek, user_peek_end)), Exit::Killed("page fault"));
        assert_eq!(run(program!(user_patch, user_patch_end)), Exit::Killed("page fault"));
    }

    #[test_case]
    fn system_calls_check_what_they_get() {
        assert_eq!(
            run(program!(user_bad_pointer, user_bad_pointer_end)),
            Exit::Code(BAD_ADDRESS)
        );
        assert_eq!(run(program!(user_unknown, user_unknown_end)), Exit::Code(NO_SUCH_CALL));
        assert_eq!(user_bytes(CODE, 16).map(|b| b.len()), Some(16));
        assert!(user_bytes(USER_END - 8, 16).is_none(), "past the lower half");
        assert!(user_bytes(u64::MAX - 4, 8).is_none(), "wraps around");
        assert!(user_bytes(0x1000, 8).is_none(), "not mapped");
    }

    #[test_case]
    fn the_timer_takes_the_cpu_away_from_ring_3() {
        // a program that never gives up the CPU - with KERNEL_SMP=1 the kernel still runs only because the timer
        // interrupts ring 3
        static DONE: AtomicBool = AtomicBool::new(false);
        let thread = task::spawn("runner", || {
            assert_eq!(run(program!(user_spin, user_spin_end)), Exit::Code(7));
            DONE.store(true, Ordering::SeqCst);
        });
        crate::time::sleep_ms(50);
        assert!(!DONE.load(Ordering::SeqCst), "the program spins");
        unsafe { data().write_volatile(1) }; // let it end
        task::join(thread);
        assert!(DONE.load(Ordering::SeqCst));
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
}
