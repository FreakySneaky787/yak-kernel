//! Programs that make the system calls a test asks for (chapters 22 and 23) - for the tests only.
//!
//! The tests drive the calls from ring 3 with a *script*: a program that reads a table of calls from its data page,
//! makes them one after the other, and writes every result over its call. The kernel fills the table, starts the
//! program, and when it has ended reads the results out of the program's memory (`Program::join_with`).

use crate::elf::{TestSegment, build_test_elf};
use crate::mem::PAGE_SIZE;
use crate::user::{self, Exit};
use alloc::vec::Vec;
use core::arch::global_asm;

// The tests drive the system calls from ring 3 with a *script*: a program that reads a table of calls from its
// data page - four numbers each: the call, three arguments - makes them one after the other, and writes every
// result over the number of its call. The kernel fills the table, starts the program, and reads the results
// from the program's memory when it has ended. A call that is not in the list is the number 1000: `ud2`.
global_asm!(
    ".section .rodata.user_programs, \"a\"",
    ".global user_script, user_script_end",
    "user_script:",
    "mov rbx, 0x401000",
    "2: mov rax, [rbx]",
    "test rax, rax",
    "jz 3f",
    "cmp rax, 1000",
    "je 4f",
    "mov rdi, [rbx + 8]",
    "mov rsi, [rbx + 16]",
    "mov rdx, [rbx + 24]",
    "int 0x80",
    "mov [rbx], rax",
    "add rbx, 32",
    "jmp 2b",
    "3: mov eax, 2",
    "xor edi, edi",
    "int 0x80",
    "4: ud2",
    "user_script_end:",
    ".text",
);

pub const CODE: u64 = 0x40_0000;
pub const DATA: u64 = 0x40_1000;
/// The table of calls is at DATA, text for the calls (paths) at STRINGS, a buffer of 128 KiB at BUFFER.
pub const STRINGS: u64 = DATA + 0x800;
pub const BUFFER: u64 = DATA + 0x1000;
pub const BUFFER_SIZE: usize = 0x2_0000;
/// Where a script's heap starts: right behind its data segment.
pub const HEAP: u64 = BUFFER + BUFFER_SIZE as u64;

pub const SYS_WRITE: u64 = 1;
pub const SYS_READ: u64 = 3;
pub const SYS_OPEN: u64 = 4;
pub const SYS_CLOSE: u64 = 5;
pub const SYS_SEEK: u64 = 6;
pub const SYS_BRK: u64 = 7;
/// Not a system call: the script program executes `ud2` here.
pub const KILL: u64 = 1000;
/// A call's error as the program sees it.
pub const fn err(errno: i64) -> i64 {
    -errno
}

pub struct Script {
    calls: Vec<[u64; 4]>,
    strings: Vec<u8>,
}

pub struct Done {
    pub exit: Exit,
    /// What the calls returned, in order.
    pub results: Vec<i64>,
    /// The program's buffer when it ended.
    pub buffer: Vec<u8>,
    /// The first eight pages of the program's heap (which starts at `HEAP`): the bytes, and for each page whether it
    /// was mapped when the program ended - a page that was never touched is not.
    pub heap: Vec<u8>,
    pub heap_mapped: Vec<bool>,
}

impl Script {
    pub fn new() -> Script {
        Script {
            calls: Vec::new(),
            strings: Vec::new(),
        }
    }

    /// Puts `text` into the program's memory; returns its address.
    pub fn text(&mut self, text: &[u8]) -> u64 {
        let address = STRINGS + self.strings.len() as u64;
        self.strings.extend_from_slice(text);
        address
    }

    pub fn call(&mut self, number: u64, a: u64, b: u64, c: u64) -> &mut Script {
        self.calls.push([number, a, b, c]);
        self
    }

    /// `open` of a path given as text.
    pub fn open(&mut self, path: &str, flags: u64) -> &mut Script {
        let address = self.text(path.as_bytes());
        self.call(SYS_OPEN, address, path.len() as u64, flags)
    }

    pub fn run(&self) -> Done {
        assert!(self.calls.len() < 60 && self.strings.len() < 0x400);
        let mut data = Vec::new();
        for call in &self.calls {
            call.iter().for_each(|n| data.extend_from_slice(&n.to_le_bytes()));
        }
        data.resize(0x800, 0); // the number 0 ends the table
        data.extend_from_slice(&self.strings);
        let code = unsafe {
            unsafe extern "C" {
                static user_script: u8;
                static user_script_end: u8;
            }
            let start = &raw const user_script as usize;
            let end = &raw const user_script_end as usize;
            core::slice::from_raw_parts(start as *const u8, end - start)
        };
        let image = build_test_elf(
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
                    bytes: &data,
                    memsz: PAGE_SIZE + BUFFER_SIZE as u64,
                },
            ],
        );
        let count = self.calls.len();
        let (exit, (results, buffer, heap, heap_mapped)) = user::spawn(&image).unwrap().join_with(|space| {
            let mut table = alloc::vec![0u8; 32 * count];
            space.read(DATA, &mut table).unwrap();
            let results = table
                .chunks(32)
                .map(|record| i64::from_le_bytes(record[..8].try_into().unwrap()))
                .collect();
            let mut buffer = alloc::vec![0u8; BUFFER_SIZE];
            space.read(BUFFER, &mut buffer).unwrap();
            let mut heap = alloc::vec![0u8; 8 * PAGE_SIZE as usize];
            let mut heap_mapped = Vec::new();
            for (i, page) in heap.chunks_mut(PAGE_SIZE as usize).enumerate() {
                let mapped = space.is_mapped(HEAP + i as u64 * PAGE_SIZE);
                if mapped {
                    space.read(HEAP + i as u64 * PAGE_SIZE, page).unwrap();
                }
                heap_mapped.push(mapped);
            }
            (results, buffer, heap, heap_mapped)
        });
        Done {
            exit,
            results,
            buffer,
            heap,
            heap_mapped,
        }
    }
}

/// The bytes between two labels of a `global_asm!` - a program's machine code.
#[allow(unused_macros)]
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
#[allow(unused_imports)]
pub(crate) use program;

/// An ELF file around `code`: the code (readable, executable) at `CODE`, a page of zeros (readable, writable) at
/// `DATA`. The heap of such a program starts at `DATA + 0x1000`.
pub fn image(code: &[u8]) -> Vec<u8> {
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
