//! Kernel heap: a linked-list allocator behind `#[global_allocator]`, which
//! enables the `alloc` crate (`Box`, `Vec`, `String`, `BTreeMap`, ...).
//!
//! Free memory is kept as a singly linked list of blocks sorted by address.
//! Each free block stores its own size and the pointer to the next block in
//! its first 16 bytes. Allocation is first fit: the first block that can hold
//! the request (including alignment padding) is split. Freeing inserts the
//! block at its address position and merges it with adjacent neighbours.
//!
//! All block addresses and sizes are multiples of 16 bytes, so any leftover
//! piece is either empty or large enough to hold a free block header.
//!
//! The heap lives in its own virtual area and grows on demand by mapping
//! fresh frames at its end.

use super::paging::{self, Flags};
use super::{PAGE_SIZE, VirtAddr, align_up};
use crate::arch::x86_64::without_interrupts;
use core::alloc::{GlobalAlloc, Layout};
use core::mem::size_of;
use core::ptr::null_mut;
use spin::Mutex;

/// PML4 slot 508: 0xFFFF_FE00_0000_0000
pub const HEAP_START: u64 = 0xFFFF_FE00_0000_0000;
/// Upper limit for heap growth (virtual).
pub const HEAP_MAX_SIZE: u64 = 1 << 30;
pub const HEAP_INITIAL_SIZE: u64 = 1024 * 1024;
/// Minimum step when the heap grows.
const GROW_STEP: u64 = 64 * 1024;

const BLOCK_ALIGN: usize = 16;

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

const _: () = assert!(size_of::<FreeBlock>() == BLOCK_ALIGN);

/// Size actually reserved for a request of `layout`.
fn block_size(layout: Layout) -> usize {
    align_up(layout.size().max(BLOCK_ALIGN) as u64, BLOCK_ALIGN as u64) as usize
}

/// The allocator core: manages free blocks inside regions handed to it.
pub struct LinkedListHeap {
    head: *mut FreeBlock,
    total: usize,
    used: usize,
}

unsafe impl Send for LinkedListHeap {}

impl LinkedListHeap {
    pub const fn empty() -> LinkedListHeap {
        LinkedListHeap {
            head: null_mut(),
            total: 0,
            used: 0,
        }
    }

    /// Adds a memory region to the heap.
    ///
    /// # Safety
    /// The region must be valid, writable, unused, and 16-byte aligned with a
    /// size that is a multiple of 16.
    pub unsafe fn add_region(&mut self, start: usize, size: usize) {
        assert!(start % BLOCK_ALIGN == 0 && size % BLOCK_ALIGN == 0 && size >= BLOCK_ALIGN);
        self.total += size;
        unsafe { self.insert_free(start, size) };
    }

    /// Inserts a free block in address order and merges it with its neighbours.
    unsafe fn insert_free(&mut self, start: usize, size: usize) {
        let mut prev: *mut FreeBlock = null_mut();
        let mut cur = self.head;
        while !cur.is_null() && (cur as usize) < start {
            prev = cur;
            cur = unsafe { (*cur).next };
        }

        let block = start as *mut FreeBlock;
        unsafe { block.write(FreeBlock { size, next: cur }) };

        // merge with the following block
        if !cur.is_null() && start + size == cur as usize {
            unsafe {
                (*block).size += (*cur).size;
                (*block).next = (*cur).next;
            }
        }
        // merge with the preceding block, or link it in front
        if prev.is_null() {
            self.head = block;
        } else if prev as usize + unsafe { (*prev).size } == start {
            unsafe {
                (*prev).size += (*block).size;
                (*prev).next = (*block).next;
            }
        } else {
            unsafe { (*prev).next = block };
        }
    }

    pub fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let size = block_size(layout);
        let align = layout.align().max(BLOCK_ALIGN);

        let mut prev: *mut FreeBlock = null_mut();
        let mut cur = self.head;
        while !cur.is_null() {
            let block_start = cur as usize;
            let block_size = unsafe { (*cur).size };
            let block_end = block_start + block_size;
            let alloc_start = align_up(block_start as u64, align as u64) as usize;
            let alloc_end = alloc_start + size;

            if alloc_end <= block_end {
                // unlink the block
                let next = unsafe { (*cur).next };
                if prev.is_null() {
                    self.head = next;
                } else {
                    unsafe { (*prev).next = next };
                }
                // give back padding in front and the rest behind (both multiples of 16)
                unsafe {
                    if alloc_start > block_start {
                        self.insert_free(block_start, alloc_start - block_start);
                    }
                    if block_end > alloc_end {
                        self.insert_free(alloc_end, block_end - alloc_end);
                    }
                }
                self.used += size;
                return alloc_start as *mut u8;
            }
            prev = cur;
            cur = unsafe { (*cur).next };
        }
        null_mut()
    }

    /// # Safety
    /// `ptr` must come from `alloc` on this heap with the same `layout`.
    pub unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        let size = block_size(layout);
        self.used -= size;
        unsafe { self.insert_free(ptr as usize, size) };
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn used(&self) -> usize {
        self.used
    }

    /// Number of free blocks (fragmentation indicator, used by tests).
    pub fn free_block_count(&self) -> usize {
        let mut n = 0;
        let mut cur = self.head;
        while !cur.is_null() {
            n += 1;
            cur = unsafe { (*cur).next };
        }
        n
    }

    /// Size of the largest free block.
    pub fn largest_free_block(&self) -> usize {
        let mut largest = 0;
        let mut cur = self.head;
        while !cur.is_null() {
            largest = largest.max(unsafe { (*cur).size });
            cur = unsafe { (*cur).next };
        }
        largest
    }
}

/// The global kernel heap: allocator core + growth by mapping pages.
pub struct KernelHeap {
    inner: Mutex<LinkedListHeap>,
    /// Current end of the mapped heap area.
    end: Mutex<u64>,
}

impl KernelHeap {
    const fn new() -> KernelHeap {
        KernelHeap {
            inner: Mutex::new(LinkedListHeap::empty()),
            end: Mutex::new(HEAP_START),
        }
    }

    /// Maps at least `bytes` more memory at the end of the heap.
    fn grow(&self, heap: &mut LinkedListHeap, bytes: u64) -> bool {
        let mut end = self.end.lock();
        let bytes = align_up(bytes.max(GROW_STEP), PAGE_SIZE);
        if *end + bytes > HEAP_START + HEAP_MAX_SIZE {
            return false;
        }
        let pages = (bytes / PAGE_SIZE) as usize;
        if paging::map_fresh(VirtAddr(*end), pages, Flags::KERNEL_DATA).is_err() {
            return false;
        }
        unsafe { heap.add_region(*end as usize, bytes as usize) };
        *end += bytes;
        true
    }

    /// Bytes of virtual memory currently mapped for the heap.
    pub fn mapped(&self) -> u64 {
        *self.end.lock() - HEAP_START
    }
}

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| {
            let mut heap = self.inner.lock();
            let ptr = heap.alloc(layout);
            if !ptr.is_null() {
                return ptr;
            }
            // Not enough space: grow by enough to fit size + worst-case alignment padding
            let needed = (block_size(layout) + layout.align()) as u64;
            if self.grow(&mut heap, needed) {
                heap.alloc(layout)
            } else {
                null_mut()
            }
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| unsafe { self.inner.lock().dealloc(ptr, layout) })
    }
}

#[global_allocator]
pub static HEAP: KernelHeap = KernelHeap::new();

/// Maps the initial heap area. Requires the frame allocator and paging.
pub fn init() {
    without_interrupts(|| {
        let mut heap = HEAP.inner.lock();
        assert!(
            HEAP.grow(&mut heap, HEAP_INITIAL_SIZE),
            "cannot map the initial kernel heap"
        );
    });
}

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    /// Bytes handed to the allocator (mapped heap).
    pub total: usize,
    pub used: usize,
    pub free_blocks: usize,
    pub largest_free_block: usize,
}

pub fn stats() -> Stats {
    without_interrupts(|| {
        let heap = HEAP.inner.lock();
        Stats {
            total: heap.total(),
            used: heap.used(),
            free_blocks: heap.free_block_count(),
            largest_free_block: heap.largest_free_block(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::collections::BTreeMap;
    use alloc::string::String;
    use alloc::vec::Vec;

    #[repr(C, align(4096))]
    struct Arena([u8; 16384]);

    fn arena_heap(arena: &'static mut Arena) -> LinkedListHeap {
        let mut heap = LinkedListHeap::empty();
        unsafe { heap.add_region(arena.0.as_mut_ptr() as usize, arena.0.len()) };
        heap
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test_case]
    fn core_alloc_and_free_merge_back() {
        static mut A: Arena = Arena([0; 16384]);
        let mut h = arena_heap(unsafe { &mut *core::ptr::addr_of_mut!(A) });
        let a = h.alloc(layout(100, 8));
        let b = h.alloc(layout(200, 8));
        let c = h.alloc(layout(300, 8));
        assert!(!a.is_null() && !b.is_null() && !c.is_null());
        assert!((a as usize) < (b as usize) && (b as usize) < (c as usize));
        assert_eq!(h.used(), 112 + 208 + 304);
        unsafe {
            h.dealloc(b, layout(200, 8));
            h.dealloc(a, layout(100, 8));
            h.dealloc(c, layout(300, 8));
        }
        assert_eq!(h.used(), 0);
        assert_eq!(h.free_block_count(), 1, "free blocks were not coalesced");
        assert_eq!(h.largest_free_block(), 16384);
    }

    #[test_case]
    fn core_respects_alignment() {
        static mut A: Arena = Arena([0; 16384]);
        let mut h = arena_heap(unsafe { &mut *core::ptr::addr_of_mut!(A) });
        let small = h.alloc(layout(8, 8));
        let aligned = h.alloc(layout(64, 4096));
        assert_eq!(aligned as usize % 4096, 0);
        assert!(!small.is_null());
        unsafe {
            h.dealloc(aligned, layout(64, 4096));
            h.dealloc(small, layout(8, 8));
        }
        assert_eq!(h.free_block_count(), 1);
    }

    #[test_case]
    fn core_exhaustion_returns_null() {
        static mut A: Arena = Arena([0; 16384]);
        let mut h = arena_heap(unsafe { &mut *core::ptr::addr_of_mut!(A) });
        let big = h.alloc(layout(16384, 16));
        assert!(!big.is_null());
        assert!(h.alloc(layout(1, 1)).is_null());
        unsafe { h.dealloc(big, layout(16384, 16)) };
        assert!(!h.alloc(layout(1, 1)).is_null());
    }

    #[test_case]
    fn core_reuses_freed_hole() {
        static mut A: Arena = Arena([0; 16384]);
        let mut h = arena_heap(unsafe { &mut *core::ptr::addr_of_mut!(A) });
        let a = h.alloc(layout(1024, 16));
        let _b = h.alloc(layout(1024, 16));
        unsafe { h.dealloc(a, layout(1024, 16)) };
        let c = h.alloc(layout(512, 16));
        assert_eq!(c, a, "first fit should reuse the freed hole");
    }

    #[test_case]
    fn box_and_string() {
        let b = Box::new(41u64);
        assert_eq!(*b + 1, 42);
        let mut s = String::from("Kernel");
        s.push_str("Try");
        assert_eq!(alloc::format!("{}!", s), "KernelTry!");
    }

    #[test_case]
    fn heap_grows_when_full() {
        // Ask for more than the largest free block, independent of what earlier tests left behind
        let before = HEAP.mapped();
        let wanted = stats().largest_free_block + 256 * 1024;
        let mut v: Vec<u64> = Vec::with_capacity(wanted / 8);
        for i in 0..(wanted / 8) as u64 {
            v.push(i);
        }
        let n = v.len() as u64;
        assert_eq!(v.iter().sum::<u64>(), n * (n - 1) / 2);
        assert!(HEAP.mapped() > before, "heap did not grow");
    }

    #[test_case]
    fn many_small_allocations_are_reused() {
        // Other threads (net thread, other CPUs) allocate meanwhile, so the
        // byte count of the whole heap is not stable; but if freed blocks were
        // not reused, 10000 x 32 bytes would make the heap grow by 320 KiB.
        let total_before = stats().total;
        for i in 0..10_000u64 {
            let b = Box::new([i; 4]);
            core::hint::black_box(&b);
        }
        assert!(
            stats().total <= total_before + 64 * 1024,
            "the heap grew: freed blocks are not reused"
        );
    }

    #[test_case]
    fn btreemap_works() {
        let mut map = BTreeMap::new();
        for i in 0..1000u32 {
            map.insert(i, i * i);
        }
        assert_eq!(map.get(&31), Some(&961));
        assert_eq!(map.len(), 1000);
    }

    #[test_case]
    fn large_allocation() {
        let v = alloc::vec![0xABu8; 4 * 1024 * 1024];
        assert!(v.iter().all(|&b| b == 0xAB));
    }
}
