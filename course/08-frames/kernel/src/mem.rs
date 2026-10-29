//! Physical memory: addresses of RAM itself, as opposed to the virtual addresses our code uses.
//!
//! Paging is on from the first instruction (Limine turned it on), so a physical address cannot simply be used as a
//! pointer. Limine maps all of physical memory once more at a fixed offset in the higher half - the *higher-half
//! direct map* (HHDM): physical address `p` is visible at virtual `p + offset`.

use core::sync::atomic::{AtomicU64, Ordering};

/// The size of a page and of a frame: 4 KiB.
pub const PAGE_SIZE: u64 = 4096;

/// A physical address. A type of its own, so that it cannot be used as a pointer by mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(pub u64);

impl PhysAddr {
    pub const fn is_page_aligned(self) -> bool {
        self.0 % PAGE_SIZE == 0
    }

    /// The virtual address at which this physical address is visible, through the HHDM.
    pub fn to_virt(self) -> u64 {
        self.0 + hhdm_offset()
    }

    /// A pointer to this physical address, through the HHDM.
    pub fn as_mut_ptr<T>(self) -> *mut T {
        self.to_virt() as *mut T
    }
}

pub const fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

pub const fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

pub fn hhdm_offset() -> u64 {
    HHDM_OFFSET.load(Ordering::Relaxed)
}

/// Reads the HHDM offset from Limine and builds the frame allocator.
pub fn init() {
    let hhdm = crate::HHDM.response().expect("Limine gave no HHDM");
    HHDM_OFFSET.store(hhdm.offset, Ordering::Relaxed);
    crate::pmm::init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn alignment_rounds_to_whole_pages() {
        assert_eq!(align_down(0x1234, PAGE_SIZE), 0x1000);
        assert_eq!(align_up(0x1234, PAGE_SIZE), 0x2000);
        assert_eq!(align_up(0x2000, PAGE_SIZE), 0x2000);
    }

    #[test_case]
    fn the_hhdm_is_in_the_higher_half() {
        assert!(hhdm_offset() >= 0xffff_8000_0000_0000);
        assert_eq!(PhysAddr(0x1000).to_virt(), hhdm_offset() + 0x1000);
    }
}
