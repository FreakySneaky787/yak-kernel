//! Kernel stacks with a guard page.
//!
//! A stack grows down. When it outgrows its memory, it silently overwrites whatever lies below it - unless nothing
//! lies below it. So every stack gets one page under it that is deliberately left unmapped, the *guard page*: the
//! first push past the end touches it and the CPU raises a page fault on the spot.
//!
//! The stacks live in an area of their own (entry 510 of the PML4, 512 GiB), one after the other, each with its
//! guard page. Their pages are frames from the frame allocator, mapped with `paging::map`.

use crate::mem::PAGE_SIZE;
use crate::paging::{self, Flags, MapError};
use crate::pmm;
use core::sync::atomic::{AtomicU64, Ordering};

const AREA_START: u64 = 0xFFFF_FF00_0000_0000;
const AREA_END: u64 = 0xFFFF_FF80_0000_0000;

/// Where the next stack's guard page goes. Virtual addresses are plentiful: they are never reused.
static NEXT: AtomicU64 = AtomicU64::new(AREA_START);

#[derive(Debug, Clone, Copy)]
pub struct Stack {
    /// The lowest address of the guard page; the stack's memory starts one page above.
    pub guard: u64,
    /// The first stack pointer: one past the highest byte (16-byte aligned, as the ABI wants).
    pub top: u64,
}

/// A stack of `pages` pages of 4 KiB, with an unmapped guard page below it.
pub fn alloc(pages: u64) -> Result<Stack, MapError> {
    let size = (pages + 1) * PAGE_SIZE; // the guard page takes address space, but no frame
    let guard = NEXT.fetch_add(size, Ordering::Relaxed);
    assert!(guard + size <= AREA_END, "the stack area is full");
    for page in 1..=pages {
        let frame = pmm::alloc_frame_zeroed().ok_or(MapError::OutOfFrames)?;
        paging::map(guard + page * PAGE_SIZE, frame, Flags::DATA)?;
    }
    Ok(Stack {
        guard,
        top: guard + size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn the_stack_is_mapped_and_its_guard_page_is_not() {
        let stack = alloc(4).unwrap();
        assert_eq!(stack.top % 16, 0);
        assert_eq!(paging::translate(stack.guard), None);
        for page in 1..=4 {
            assert_eq!(paging::flags(stack.guard + page * PAGE_SIZE), Some(Flags::DATA));
        }
        unsafe { ((stack.top - 8) as *mut u64).write_volatile(42) }; // the highest word is ours
        let next = alloc(1).unwrap();
        assert!(next.guard >= stack.top, "stacks overlap");
    }
}
