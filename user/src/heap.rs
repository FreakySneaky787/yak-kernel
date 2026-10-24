//! The heap of a user program: a first-fit allocator on memory obtained
//! from the kernel with `brk`.
//!
//! Free blocks form a singly linked list sorted by address; each stores its
//! size and the next pointer in its first 16 bytes. All sizes and addresses
//! are multiples of 16. Freeing merges a block with its neighbours, so the
//! heap does not fragment into tiny pieces. When no block fits, the heap is
//! grown by at least `GROW_MIN` bytes. Programs are single-threaded, so no
//! lock is needed.

use crate::sys;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;

const UNIT: usize = 16;
const GROW_MIN: usize = 64 * 1024;

struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

struct Inner {
    head: *mut FreeBlock,
    /// Current end of the heap (the program break); 0 = not initialized.
    end: usize,
}

pub struct Heap {
    inner: UnsafeCell<Inner>,
}

// A program has exactly one thread.
unsafe impl Sync for Heap {}

const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

impl Heap {
    pub const fn new() -> Heap {
        Heap {
            inner: UnsafeCell::new(Inner {
                head: ptr::null_mut(),
                end: 0,
            }),
        }
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Inner {
    /// Inserts `[addr, addr + size)` into the free list, merging neighbours.
    unsafe fn insert(&mut self, addr: usize, size: usize) {
        unsafe {
            let mut prev: *mut FreeBlock = ptr::null_mut();
            let mut next = self.head;
            while !next.is_null() && (next as usize) < addr {
                prev = next;
                next = (*next).next;
            }
            let block = addr as *mut FreeBlock;
            block.write(FreeBlock { size, next });
            if !next.is_null() && addr + size == next as usize {
                (*block).size += (*next).size;
                (*block).next = (*next).next;
            }
            if prev.is_null() {
                self.head = block;
            } else if prev as usize + (*prev).size == addr {
                (*prev).size += (*block).size;
                (*prev).next = (*block).next;
            } else {
                (*prev).next = block;
            }
        }
    }

    /// First fit: carves `size` bytes aligned to `align` out of a free block.
    unsafe fn take(&mut self, size: usize, align: usize) -> *mut u8 {
        unsafe {
            let mut prev: *mut FreeBlock = ptr::null_mut();
            let mut block = self.head;
            while !block.is_null() {
                let start = block as usize;
                let end = start + (*block).size;
                let aligned = align_up(start, align);
                if aligned + size <= end {
                    // unlink, then give back the unused front and rest
                    let next = (*block).next;
                    if prev.is_null() {
                        self.head = next;
                    } else {
                        (*prev).next = next;
                    }
                    if aligned > start {
                        self.insert(start, aligned - start);
                    }
                    if end > aligned + size {
                        self.insert(aligned + size, end - aligned - size);
                    }
                    return aligned as *mut u8;
                }
                prev = block;
                block = (*block).next;
            }
            ptr::null_mut()
        }
    }

    /// Asks the kernel for at least `bytes` more heap.
    unsafe fn grow(&mut self, bytes: usize) -> bool {
        if self.end == 0 {
            self.end = sys::brk(0) as usize;
        }
        let amount = align_up(bytes.max(GROW_MIN), 4096);
        let new_end = self.end + amount;
        if sys::brk(new_end as u64) != new_end as i64 {
            return false;
        }
        let old_end = self.end;
        self.end = new_end;
        unsafe { self.insert(old_end, amount) };
        true
    }
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let inner = unsafe { &mut *self.inner.get() };
        let size = align_up(layout.size().max(UNIT), UNIT);
        let align = layout.align().max(UNIT);
        unsafe {
            let p = inner.take(size, align);
            if !p.is_null() {
                return p;
            }
            if !inner.grow(size + align) {
                return ptr::null_mut();
            }
            inner.take(size, align)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let inner = unsafe { &mut *self.inner.get() };
        let size = align_up(layout.size().max(UNIT), UNIT);
        unsafe { inner.insert(ptr as usize, size) };
    }
}

/// Current program break (end of the heap area obtained from the kernel).
pub fn program_break() -> usize {
    sys::brk(0) as usize
}
