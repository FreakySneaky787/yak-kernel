//! Memory on demand: the heap of a program (chapter 23).
//!
//! A program's data segment is whatever its file says, and its stack is fixed. For anything else - a buffer whose size
//! it learns while it runs - it needs memory it can ask for. The classic call is `brk`: the program has a *heap*, a
//! region of memory that starts right behind its image, and `brk(address)` says where it should end - the *break*. The
//! program grows the heap by moving the break up, and gives memory back by moving it down.
//!
//! The kernel does not map the pages when the break moves. A program may ask for 32 MiB and touch three pages of it,
//! and 8192 frames would be gone for nothing. Moving the break only changes a number; the first *touch* of a page
//! that is not there makes the CPU raise a page fault, and the fault handler maps a fresh, zeroed frame and lets the
//! program run its instruction again - it never notices. That is **demand paging**. A fault in the heap is not an
//! error; a fault anywhere else still ends the program (chapter 20).
//!
//! Pages of the heap that the *kernel* is about to use for a program - the buffer of a `read` - are mapped the same
//! way, before the call checks them (`populate`): the program may hand over memory it has not touched yet.

use crate::mem::{PAGE_SIZE, align_down, align_up};
use crate::paging::{self, Flags};
use crate::{interrupts, pmm, task};
use alloc::collections::BTreeMap;
use spin::Mutex;

/// The most a heap may grow to: 64 MiB.
pub const HEAP_MAX: u64 = 64 * 1024 * 1024;

/// The error `brk` returns for a break outside the heap's limits (Linux's `ENOMEM`).
pub const ENOMEM: i64 = 12;

/// Where a program's heap begins, and where it ends right now: the break. `start` is a multiple of the page size.
#[derive(Clone, Copy)]
struct Heap {
    start: u64,
    end: u64,
}

impl Heap {
    /// The pages that are the heap - those below the break, rounded up: a break in the middle of a page keeps the page.
    fn pages(&self) -> core::ops::Range<u64> {
        self.start..align_up(self.end, PAGE_SIZE)
    }
}

/// The heap of every program, by the id of its thread.
static HEAPS: Mutex<BTreeMap<task::Id, Heap>> = Mutex::new(BTreeMap::new());

/// The heap of the program that runs.
fn heap() -> Option<Heap> {
    let me = task::current();
    interrupts::without_interrupts(|| HEAPS.lock().get(&me).copied())
}

/// The running program gets an empty heap at `start`. The program's own thread calls this, before it goes to ring 3:
/// when the program first asks for its heap, the entry is there. (Made by whoever starts the thread, it could come too
/// late - the thread is runnable from the moment it exists. Bug #19 of the reference kernel.)
pub fn begin(start: u64) {
    let me = task::current();
    interrupts::without_interrupts(|| HEAPS.lock().insert(me, Heap { start, end: start }));
}

/// The program has ended: its heap is gone - the frames go with its address space.
pub fn forget(thread: task::Id) {
    interrupts::without_interrupts(|| HEAPS.lock().remove(&thread));
}

/// `brk(address)`: moves the break to `address` and returns it. `brk(0)` moves nothing and returns where it is. A break
/// below the start of the heap or more than `HEAP_MAX` above it is refused (`ENOMEM`), and nothing changes.
pub fn brk(address: u64) -> Result<u64, i64> {
    let me = task::current();
    let heap = heap().ok_or(ENOMEM)?;
    if address == 0 {
        return Ok(heap.end);
    }
    if address < heap.start || address - heap.start > HEAP_MAX {
        return Err(ENOMEM);
    }
    // moving down: the pages that are above the new break are given back. Moving up needs no work.
    let (old_end, new_end) = (align_up(heap.end, PAGE_SIZE), align_up(address, PAGE_SIZE));
    for page in (new_end..old_end).step_by(PAGE_SIZE as usize) {
        // a page that was never touched is not there: nothing to give back
        if let Some(frame) = paging::unmap(page) {
            pmm::free_frame(frame);
        }
    }
    interrupts::without_interrupts(|| {
        if let Some(entry) = HEAPS.lock().get_mut(&me) {
            entry.end = address;
        }
    });
    Ok(address)
}

/// Maps a fresh, zeroed frame at `page`. False if there is none left.
fn map_fresh(page: u64) -> bool {
    let Some(frame) = pmm::alloc_frame_zeroed() else {
        return false;
    };
    match paging::map(page, frame, Flags::USER_DATA) {
        Ok(()) => true,
        Err(_) => {
            pmm::free_frame(frame);
            false
        }
    }
}

/// The page fault handler asks: is this a program touching a page of its heap that is not there yet? Then the page is
/// mapped and the program goes on. `error_code` is the CPU's: bit 0 set means the page *is* there and the access was
/// not allowed - a write to a read-only page, say - which is never the heap's business.
pub fn handle_page_fault(address: u64, error_code: u64) -> bool {
    if error_code & 1 != 0 {
        return false;
    }
    let page = align_down(address, PAGE_SIZE);
    heap().is_some_and(|heap| heap.pages().contains(&page)) && map_fresh(page)
}

/// Before the kernel looks at the bytes `address..address + length` of the running program, the pages of them that are
/// heap and not there yet are mapped. At most the heap's size of work, whatever length the program names: only the
/// part of the range inside the heap is looked at (bug #11 of the reference kernel was a loop over the *whole* range).
pub fn populate(address: u64, length: u64) {
    let Some(heap) = heap() else {
        return;
    };
    let Some(end) = address.checked_add(length) else {
        return;
    };
    let pages = heap.pages();
    let first = align_down(address, PAGE_SIZE).max(pages.start);
    let last = align_up(end, PAGE_SIZE).min(pages.end);
    for page in (first..last.max(first)).step_by(PAGE_SIZE as usize) {
        if paging::translate(page).is_none() && !map_fresh(page) {
            return; // out of frames: the check that follows fails for the pages that are missing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::EFAULT;
    use crate::script::{self, *};
    use crate::user::{self, Exit};
    use alloc::vec::Vec;
    use core::arch::global_asm;

    // Programs that touch their heap. Each starts with `brk(0)` to find it (the start in RBX).
    global_asm!(
        ".section .rodata.user_programs, \"a\"",
        // grow by 64 KiB, read the 4th page: a page that was never there reads as zeros - exit(7 + what was read)
        ".global user_heap_zero, user_heap_zero_end",
        "user_heap_zero:",
        "mov eax, 7",
        "xor edi, edi",
        "int 0x80",
        "mov rbx, rax",
        "mov eax, 7",
        "lea rdi, [rbx + 0x10000]",
        "int 0x80",
        "mov rdi, [rbx + 0x3000]",
        "add rdi, 7",
        "mov eax, 2",
        "int 0x80",
        "user_heap_zero_end:",
        // grow, touch the first four pages, shrink to nothing, touch the first again: that must be a page fault
        ".global user_heap_shrink, user_heap_shrink_end",
        "user_heap_shrink:",
        "mov eax, 7",
        "xor edi, edi",
        "int 0x80",
        "mov rbx, rax",
        "mov eax, 7",
        "lea rdi, [rbx + 0x10000]",
        "int 0x80",
        "mov qword ptr [rbx], 1",
        "mov qword ptr [rbx + 0x1000], 1",
        "mov qword ptr [rbx + 0x2000], 1",
        "mov qword ptr [rbx + 0x3000], 1",
        "mov eax, 7",
        "mov rdi, rbx",
        "int 0x80",
        "mov qword ptr [rbx], 2",
        "mov eax, 2",
        "xor edi, edi",
        "int 0x80",
        "user_heap_shrink_end:",
        // touch a page that is not in the heap: the break was never moved
        ".global user_heap_outside, user_heap_outside_end",
        "user_heap_outside:",
        "mov eax, 7",
        "xor edi, edi",
        "int 0x80",
        "mov rbx, rax",
        "mov qword ptr [rbx + 0x10000], 1",
        "mov eax, 2",
        "xor edi, edi",
        "int 0x80",
        "user_heap_outside_end:",
        // the break is 100 bytes into the first page: the last 8 bytes of that page are the heap's - exit(3)
        ".global user_heap_partial, user_heap_partial_end",
        "user_heap_partial:",
        "mov eax, 7",
        "xor edi, edi",
        "int 0x80",
        "mov rbx, rax",
        "mov eax, 7",
        "lea rdi, [rbx + 100]",
        "int 0x80",
        "mov qword ptr [rbx + 4088], 1",
        "mov eax, 2",
        "mov edi, 3",
        "int 0x80",
        "user_heap_partial_end:",
        // ... and the first byte of the *next* page is not
        ".global user_heap_next_page, user_heap_next_page_end",
        "user_heap_next_page:",
        "mov eax, 7",
        "xor edi, edi",
        "int 0x80",
        "mov rbx, rax",
        "mov eax, 7",
        "lea rdi, [rbx + 100]",
        "int 0x80",
        "mov byte ptr [rbx + 4096], 1",
        "mov eax, 2",
        "xor edi, edi",
        "int 0x80",
        "user_heap_next_page_end:",
        ".text",
    );

    fn run_code(code: &[u8]) -> Exit {
        user::run(&image(code)).unwrap()
    }

    #[test_case]
    fn the_heap_starts_behind_the_image_and_the_break_stays_within_its_limits() {
        let mut s = Script::new();
        let s_ = HEAP;
        s.call(SYS_BRK, 0, 0, 0) // 0: where it is
            .call(SYS_BRK, s_ + 100, 0, 0) // 1: 100 bytes up
            .call(SYS_BRK, 0, 0, 0) // 2: and there it stays
            .call(SYS_BRK, s_ - 1, 0, 0) // 3: below the start of the heap
            .call(SYS_BRK, 1, 0, 0) // 4: ... or far below it
            .call(SYS_BRK, s_ + HEAP_MAX + 1, 0, 0) // 5: one byte too much
            .call(SYS_BRK, u64::MAX - 8, 0, 0) // 6: the end of the address space
            .call(SYS_BRK, s_ + HEAP_MAX, 0, 0) // 7: exactly the most
            .call(SYS_BRK, s_, 0, 0); // 8: and all of it back
        let done = s.run();
        let enomem = -ENOMEM;
        assert_eq!(
            done.results,
            [
                s_ as i64,
                s_ as i64 + 100,
                s_ as i64 + 100,
                enomem,
                enomem,
                enomem,
                enomem,
                (s_ + HEAP_MAX) as i64,
                s_ as i64
            ]
        );
        assert_eq!(s_ % PAGE_SIZE, 0);
    }

    #[test_case]
    fn the_first_touch_of_a_page_maps_it_zeroed() {
        // 64 KiB of heap, and the program reads the 4th page without ever having written it
        assert_eq!(run_code(program!(user_heap_zero, user_heap_zero_end)), Exit::Code(7));
    }

    #[test_case]
    fn a_heap_of_32_mib_costs_the_pages_touched() {
        // /bin/heap asks for 32 MiB (8192 pages), touches three of them, writes a line and exits with 0
        let image = crate::fs::FS.lock().as_mut().unwrap().read_file("/bin/heap").unwrap();
        crate::user::OUTPUT.lock().clear();
        let free = pmm::free_frames();
        let (exit, used) = user::spawn(&image).unwrap().join_with(|_| free - pmm::free_frames());
        assert_eq!(exit, Exit::Code(0));
        assert_eq!(
            *crate::user::OUTPUT.lock(),
            "Heap: 32 MiB asked for, three pages touched\n"
        );
        // the frames the program still holds when it has ended: its image, the stack, three heap pages and their tables
        // - and the stack of its thread, which is only given back when the next thread starts
        crate::println!("a 32 MiB heap, three pages touched: {} frames in use", used);
        assert!(used < 40, "{} frames", used);
        assert_eq!(pmm::free_frames().abs_diff(free) < 16, true);
    }

    #[test_case]
    fn pages_given_back_are_gone_for_good() {
        assert_eq!(
            run_code(program!(user_heap_shrink, user_heap_shrink_end)),
            Exit::Killed("page fault")
        );
        // The same on every CPU: four programs at once, several rounds - they change CPUs between their calls. A stale
        // translation of the page would let the second touch succeed, and the program would end with `Code(0)`.
        let free = pmm::free_frames();
        for _ in 0..6 {
            let programs: Vec<_> = (0..4)
                .map(|_| user::spawn(&image(program!(user_heap_shrink, user_heap_shrink_end))).unwrap())
                .collect();
            for program in programs {
                assert_eq!(program.join(), Exit::Killed("page fault"));
            }
        }
        // and the frames of the pages that were given back are free again: 24 programs, 4 pages each
        let lost = free.saturating_sub(pmm::free_frames());
        assert!(lost <= 40, "{} frames lost", lost);
    }

    #[test_case]
    fn only_the_heap_is_demand_paged() {
        assert_eq!(
            run_code(program!(user_heap_outside, user_heap_outside_end)),
            Exit::Killed("page fault")
        );
        // the break is byte-precise, the pages are whole: the rest of the page the break is in belongs to the heap
        assert_eq!(
            run_code(program!(user_heap_partial, user_heap_partial_end)),
            Exit::Code(3)
        );
        assert_eq!(
            run_code(program!(user_heap_next_page, user_heap_next_page_end)),
            Exit::Killed("page fault")
        );
    }

    #[test_case]
    fn a_system_call_may_fill_a_page_the_program_has_not_touched() {
        let mut s = Script::new();
        s.call(SYS_BRK, HEAP + 0x4000, 0, 0) // 4 pages of heap
            .open("/hello.txt", crate::files::READ)
            .call(SYS_READ, 3, HEAP + 0x1008, 5) // into the second page, which nobody touched
            .call(SYS_READ, 3, HEAP + 0x4000, 5) // behind the break: not the program's
            .call(SYS_READ, 3, HEAP + 0x3ffe, 5); // across the break
        let done = s.run();
        assert_eq!(done.exit, Exit::Code(0));
        assert_eq!(done.results[0], (HEAP + 0x4000) as i64);
        assert_eq!(&done.results[2..], [5, -EFAULT, -EFAULT]);
        assert_eq!(&done.heap[0x1008..0x100d], b"Hello");
        // Only the pages the kernel needed are there, the others were never touched and never made. The third read
        // failed, but not before the page below the break was mapped: the program cannot tell - it reads zeros from
        // a page that is there as it does from one that is not.
        assert_eq!(&done.heap_mapped[..5], [false, true, false, true, false]);
    }

    #[test_case]
    fn programs_give_their_heap_back() {
        let image = crate::fs::FS.lock().as_mut().unwrap().read_file("/bin/heap").unwrap();
        for _ in 0..2 {
            user::run(&image).unwrap(); // warm up
        }
        let free = pmm::free_frames();
        for _ in 0..12 {
            assert_eq!(user::run(&image).unwrap(), Exit::Code(0));
        }
        let lost = free.saturating_sub(pmm::free_frames());
        assert!(lost <= 16, "{} frames lost", lost);
    }

    #[test_case]
    fn a_program_without_a_heap_entry_has_no_break() {
        // the kernel's own threads have no heap: `brk` for them is refused, and nothing is looked up for a thread
        // that is not a program
        assert_eq!(brk(0), Err(ENOMEM));
        assert!(!handle_page_fault(0x1000, 0));
    }

    #[test_case]
    fn the_heap_is_data_not_code() {
        // the program stores a `ret` in its heap and jumps there: the page is there, and not executable - a
        // *protection* fault, which the heap's handler leaves alone: the program ends
        assert_eq!(
            run_code(program!(user_heap_exec, user_heap_exec_end)),
            Exit::Killed("page fault")
        );
    }

    global_asm!(
        ".section .rodata.user_programs, \"a\"",
        ".global user_heap_exec, user_heap_exec_end",
        "user_heap_exec:",
        "mov eax, 7",
        "xor edi, edi",
        "int 0x80",
        "mov rbx, rax",
        "mov eax, 7",
        "lea rdi, [rbx + 0x1000]",
        "int 0x80",
        "mov byte ptr [rbx], 0xC3",
        "jmp rbx",
        "user_heap_exec_end:",
        ".text",
    );
}
