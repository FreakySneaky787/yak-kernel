//! The kernel heap: memory in any size, not just whole frames - and with it `Box`, `Vec` and `String`.
//!
//! The allocator is a *free list*: every gap of free memory carries its own bookkeeping in its first 16 bytes - its
//! size and a pointer to the next gap. The gaps are sorted by address. `alloc` takes the first gap that is big enough
//! (*first fit*) and gives back what it does not need; `dealloc` puts a block back at its place in the list and
//! merges it with the gaps right before and after it.
//!
//! The heap lives in an area of its own (entry 507 of the PML4). It starts empty: when a request does not fit, it
//! maps fresh frames at its end with `paging::map` - the first `Box` maps the first pages.

use crate::mem::PAGE_SIZE;
use crate::paging::{self, Flags};
use crate::pmm;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use spin::Mutex;

/// Where the heap starts: the first address of PML4 entry 507.
pub const START: usize = 0xFFFF_FD80_0000_0000;
/// How big it may grow. Beyond that `alloc` returns null - for `Box` and `Vec` that is a panic.
pub const MAX_SIZE: usize = 16 * 1024 * 1024;
/// It grows by at least this much at a time, so that small requests do not map one page each.
const GROW_STEP: usize = 64 * 1024;

/// Every block starts and ends on a multiple of 16 bytes: room for a `Gap` in any leftover piece.
const BLOCK_ALIGN: usize = 16;

/// The bookkeeping in the first 16 bytes of every free gap.
#[repr(C)]
struct Gap {
    size: usize,
    next: *mut Gap,
}

const _: () = assert!(size_of::<Gap>() == BLOCK_ALIGN);

/// The bytes a request really takes: at least one `Gap`, rounded up to 16 - so that it can be a gap again when freed.
fn block_size(layout: Layout) -> usize {
    layout.size().max(BLOCK_ALIGN).next_multiple_of(BLOCK_ALIGN)
}

/// The free list: the first gap, and how many bytes are handed out.
pub struct FreeList {
    first: *mut Gap,
    used: usize,
}

// the raw pointer points into memory only this list owns, so the list may move between CPUs (or into a Mutex)
unsafe impl Send for FreeList {}

impl FreeList {
    pub const fn empty() -> FreeList {
        FreeList {
            first: null_mut(),
            used: 0,
        }
    }

    /// Hands the memory `start..start + size` to the list.
    ///
    /// # Safety
    /// The memory must be mapped, writable and used by nobody else; `start` and `size` multiples of 16.
    pub unsafe fn add_region(&mut self, start: usize, size: usize) {
        assert!(start % BLOCK_ALIGN == 0 && size % BLOCK_ALIGN == 0 && size > 0);
        unsafe { self.insert(start, size) };
    }

    /// Puts the gap `start..start + size` into the list at its address, merged with the gaps around it.
    unsafe fn insert(&mut self, start: usize, size: usize) {
        // find the neighbours: `before` ends at or below `start`, `after` starts above it
        let mut before: *mut Gap = null_mut();
        let mut after = self.first;
        while !after.is_null() && (after as usize) < start {
            before = after;
            after = unsafe { (*after).next };
        }
        let gap = start as *mut Gap;
        unsafe {
            gap.write(Gap { size, next: after });
            // touches the gap after it: one gap
            if !after.is_null() && start + size == after as usize {
                (*gap).size += (*after).size;
                (*gap).next = (*after).next;
            }
            if before.is_null() {
                self.first = gap;
            } else if before as usize + (*before).size == start {
                // touches the gap before it: that one grows
                (*before).size += (*gap).size;
                (*before).next = (*gap).next;
            } else {
                (*before).next = gap;
            }
        }
    }

    /// A block for `layout`, or null if no gap is big enough.
    pub fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let size = block_size(layout);
        let align = layout.align().max(BLOCK_ALIGN);
        let mut before: *mut Gap = null_mut();
        let mut gap = self.first;
        while !gap.is_null() {
            let gap_start = gap as usize;
            let (gap_size, next) = unsafe { ((*gap).size, (*gap).next) };
            let gap_end = gap_start + gap_size;
            // the block may have to start further up to be aligned; the padding in front stays free
            let block_start = gap_start.next_multiple_of(align);
            let block_end = block_start + size;
            if block_end <= gap_end {
                // take the gap out of the list, then give back what the block does not cover
                if before.is_null() {
                    self.first = next;
                } else {
                    unsafe { (*before).next = next };
                }
                unsafe {
                    if block_start > gap_start {
                        self.insert(gap_start, block_start - gap_start);
                    }
                    if gap_end > block_end {
                        self.insert(block_end, gap_end - block_end);
                    }
                }
                self.used += size;
                return block_start as *mut u8;
            }
            before = gap;
            gap = next;
        }
        null_mut()
    }

    /// Gives a block back.
    ///
    /// # Safety
    /// `block` must come from `alloc` on this list, with the same `layout`, and not be freed already.
    pub unsafe fn dealloc(&mut self, block: *mut u8, layout: Layout) {
        let size = block_size(layout);
        self.used -= size;
        unsafe { self.insert(block as usize, size) };
    }

    pub fn used(&self) -> usize {
        self.used
    }

    /// How many gaps the list has - after everything is freed, one per region if merging works.
    #[cfg(test)]
    pub fn gaps(&self) -> usize {
        let mut count = 0;
        let mut gap = self.first;
        while !gap.is_null() {
            count += 1;
            gap = unsafe { (*gap).next };
        }
        count
    }
}

/// The free list and the end of the mapped part of the heap area.
struct State {
    list: FreeList,
    end: usize,
}

/// The heap of the whole kernel: a free list that grows by mapping pages. No interrupt handler allocates, so a spin
/// lock with interrupts on is enough (a handler that did would wait forever for the lock its own CPU holds).
pub struct KernelHeap(Mutex<State>);

impl State {
    /// Maps at least `bytes` more at the end of the heap and hands them to the list. False if the heap is at its
    /// limit or the frames run out.
    fn grow(&mut self, bytes: usize) -> bool {
        let bytes = bytes.max(GROW_STEP).next_multiple_of(PAGE_SIZE as usize);
        if self.end + bytes > START + MAX_SIZE {
            return false;
        }
        let start = self.end;
        while self.end < start + bytes {
            let Some(frame) = pmm::alloc_frame() else {
                break;
            };
            if paging::map(self.end as u64, frame, Flags::DATA).is_err() {
                pmm::free_frame(frame);
                break;
            }
            self.end += PAGE_SIZE as usize;
        }
        // what got mapped is the heap's even if not all of it did
        if self.end > start {
            unsafe { self.list.add_region(start, self.end - start) };
        }
        self.end == start + bytes
    }
}

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut state = self.0.lock();
        let block = state.list.alloc(layout);
        if !block.is_null() {
            return block;
        }
        // enough for the block even if its alignment wastes almost `align` bytes in front of it
        if state.grow(block_size(layout) + layout.align()) {
            state.list.alloc(layout)
        } else {
            null_mut()
        }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        unsafe { self.0.lock().list.dealloc(block, layout) }
    }
}

/// The allocator behind `Box`, `Vec`, `String` and everything else in the `alloc` crate.
#[global_allocator]
static HEAP: KernelHeap = KernelHeap(Mutex::new(State {
    list: FreeList::empty(),
    end: START,
}));

/// Bytes of the heap area that are mapped.
pub fn mapped() -> usize {
    HEAP.0.lock().end - START
}

/// Bytes handed out and not yet freed.
pub fn used() -> usize {
    HEAP.0.lock().list.used()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::collections::BTreeMap;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// 16 KiB of plain memory in the kernel's .bss: the free list can be tested without the heap area.
    #[repr(C, align(4096))]
    struct Arena([u8; ARENA_SIZE]);
    const ARENA_SIZE: usize = 16 * 1024;

    fn list_on(arena: *mut Arena) -> FreeList {
        let mut list = FreeList::empty();
        unsafe { list.add_region(arena as usize, ARENA_SIZE) };
        list
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test_case]
    fn freed_blocks_merge_into_one_gap() {
        static mut ARENA: Arena = Arena([0; ARENA_SIZE]);
        let mut list = list_on(&raw mut ARENA);
        let a = list.alloc(layout(100, 8));
        let b = list.alloc(layout(200, 8));
        let c = list.alloc(layout(300, 8));
        assert!(!a.is_null() && a < b && b < c);
        assert_eq!(list.used(), 112 + 208 + 304); // each rounded up to 16
        unsafe {
            // the middle one first: it has no free neighbour, then a and c each touch it
            list.dealloc(b, layout(200, 8));
            assert_eq!(list.gaps(), 2);
            list.dealloc(a, layout(100, 8));
            list.dealloc(c, layout(300, 8));
        }
        assert_eq!(list.used(), 0);
        assert_eq!(list.gaps(), 1, "the free blocks were not merged");
    }

    #[test_case]
    fn blocks_are_aligned_as_asked() {
        static mut ARENA: Arena = Arena([0; ARENA_SIZE]);
        let mut list = list_on(&raw mut ARENA);
        let small = list.alloc(layout(8, 8));
        let page = list.alloc(layout(64, 4096));
        assert_eq!(small as usize % 16, 0);
        assert_eq!(page as usize % 4096, 0);
        unsafe {
            list.dealloc(page, layout(64, 4096));
            list.dealloc(small, layout(8, 8));
        }
        assert_eq!(list.gaps(), 1); // the padding in front of `page` was free and merged back too
    }

    #[test_case]
    fn a_full_list_says_null() {
        static mut ARENA: Arena = Arena([0; ARENA_SIZE]);
        let mut list = list_on(&raw mut ARENA);
        let all = list.alloc(layout(ARENA_SIZE, 16));
        assert!(!all.is_null());
        assert!(list.alloc(layout(1, 1)).is_null());
        unsafe { list.dealloc(all, layout(ARENA_SIZE, 16)) };
        assert!(!list.alloc(layout(1, 1)).is_null());
    }

    #[test_case]
    fn first_fit_takes_the_first_hole() {
        static mut ARENA: Arena = Arena([0; ARENA_SIZE]);
        let mut list = list_on(&raw mut ARENA);
        let a = list.alloc(layout(1024, 16));
        let _b = list.alloc(layout(1024, 16));
        unsafe { list.dealloc(a, layout(1024, 16)) };
        assert_eq!(list.alloc(layout(512, 16)), a);
    }

    #[test_case]
    fn box_string_and_format_work() {
        let boxed = Box::new(41u64);
        assert_eq!(*boxed + 1, 42);
        let mut text = String::from("Y");
        text.push_str("AK");
        assert_eq!(alloc::format!("{}!", text), "YAK!");
        let address = &*boxed as *const u64 as usize;
        assert!((START..START + mapped()).contains(&address));
    }

    #[test_case]
    fn the_heap_grows_when_it_is_full() {
        let before = mapped();
        // more than everything mapped so far: no gap can be big enough
        let count = (before + GROW_STEP) / 8;
        let numbers: Vec<u64> = (0..count as u64).collect();
        assert_eq!(numbers.iter().sum::<u64>(), count as u64 * (count as u64 - 1) / 2);
        assert!(mapped() > before, "the heap did not grow");
    }

    #[test_case]
    fn freed_memory_is_used_again() {
        let (mapped_before, used_before) = (mapped(), used());
        // 10 000 boxes of 32 bytes are 320 KiB - only if every freed one is reused does the heap stay as it is
        for i in 0..10_000u64 {
            let boxed = Box::new([i; 4]);
            core::hint::black_box(&boxed);
        }
        assert_eq!(mapped(), mapped_before);
        assert_eq!(used(), used_before);
    }

    #[test_case]
    fn a_btreemap_works() {
        let mut squares = BTreeMap::new();
        for i in 0..1000u32 {
            squares.insert(i, i * i);
        }
        assert_eq!(squares.get(&31), Some(&961));
        assert_eq!(squares.len(), 1000);
    }

    #[test_case]
    fn more_than_the_limit_is_null() {
        let before = mapped();
        let block = unsafe { alloc::alloc::alloc(layout(MAX_SIZE + 1, 16)) };
        assert!(block.is_null());
        assert_eq!(mapped(), before, "a refused request mapped pages");
    }
}
