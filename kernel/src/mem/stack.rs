//! Kernel stacks with guard pages.
//!
//! Every stack gets its own slice of a dedicated virtual area. Below each
//! stack one page is deliberately left unmapped (the guard page): a stack
//! overflow runs into it and raises a page fault - which escalates to a
//! double fault handled on the IST stack - instead of silently overwriting
//! whatever lies below.

use super::paging::{self, Flags, MapError};
use super::{PAGE_SIZE, VirtAddr};
use core::sync::atomic::{AtomicU64, Ordering};

/// PML4 slot 510: 0xFFFF_FF00_0000_0000 .. 0xFFFF_FF80_0000_0000 (512 GiB of address space)
pub const STACK_AREA_START: u64 = 0xFFFF_FF00_0000_0000;
const STACK_AREA_END: u64 = 0xFFFF_FF80_0000_0000;

/// Default size of a kernel stack in pages (64 KiB).
pub const DEFAULT_STACK_PAGES: usize = 16;

static NEXT_SLOT: AtomicU64 = AtomicU64::new(STACK_AREA_START);

#[derive(Debug, Clone, Copy)]
pub struct KernelStack {
    /// Lowest address of the (unmapped) guard page.
    pub guard: VirtAddr,
    /// Initial stack pointer (one past the highest mapped byte, 16-byte aligned).
    pub top: VirtAddr,
    pub pages: usize,
}

impl KernelStack {
    pub fn contains_guard(&self, addr: u64) -> bool {
        (self.guard.0..self.guard.0 + PAGE_SIZE).contains(&addr)
    }
}

/// Allocates and maps a stack of `pages` pages with a guard page below it.
/// Virtual address space is never reused (plenty of it in slot 510).
pub fn alloc_stack(pages: usize) -> Result<KernelStack, MapError> {
    let span = (pages as u64 + 1) * PAGE_SIZE;
    let guard = NEXT_SLOT.fetch_add(span, Ordering::Relaxed);
    assert!(guard + span <= STACK_AREA_END, "kernel stack area exhausted");
    paging::map_fresh(VirtAddr(guard + PAGE_SIZE), pages, Flags::KERNEL_DATA)?;
    Ok(KernelStack {
        guard: VirtAddr(guard),
        top: VirtAddr(guard + span),
        pages,
    })
}

/// Unmaps a stack and returns its frames. The guard page and the virtual
/// range are not reused.
pub fn free_stack(stack: KernelStack) {
    paging::unmap_and_free(VirtAddr(stack.guard.0 + PAGE_SIZE), stack.pages);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn stack_is_mapped_and_guard_is_not() {
        let s = alloc_stack(4).unwrap();
        assert_eq!(s.top.0 % 16, 0);
        assert!(paging::translate(s.guard).is_none(), "guard page is mapped");
        for i in 1..=4u64 {
            let page = VirtAddr(s.guard.0 + i * PAGE_SIZE);
            assert_eq!(paging::page_flags(page), Some(Flags::KERNEL_DATA));
        }
        // the highest word is writable
        let top_word = VirtAddr(s.top.0 - 8).as_mut_ptr::<u64>();
        unsafe { top_word.write_volatile(42) };
        assert!(s.contains_guard(s.guard.0 + 100));
        assert!(!s.contains_guard(s.guard.0 + PAGE_SIZE));
    }

    #[test_case]
    fn stacks_do_not_overlap() {
        let a = alloc_stack(2).unwrap();
        let b = alloc_stack(2).unwrap();
        assert!(b.guard.0 >= a.top.0);
    }
}
