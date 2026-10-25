//! Memory management: address types, the higher-half direct map (HHDM),
//! and the physical frame allocator.

pub mod address_space;
pub mod heap;
pub mod paging;
pub mod pmm;
pub mod stack;

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

pub const PAGE_SIZE: u64 = 4096;

/// A physical memory address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PhysAddr(pub u64);

/// A virtual memory address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct VirtAddr(pub u64);

impl PhysAddr {
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn is_page_aligned(self) -> bool {
        self.0 % PAGE_SIZE == 0
    }

    /// Where this physical address is visible through the HHDM.
    pub fn to_virt(self) -> VirtAddr {
        VirtAddr(self.0 + hhdm_offset())
    }
}

impl VirtAddr {
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn as_ptr<T>(self) -> *const T {
        self.0 as *const T
    }

    pub fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }
}

impl fmt::Debug for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "PhysAddr({:#x})", self.0)
    }
}

impl fmt::Debug for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "VirtAddr({:#x})", self.0)
    }
}

pub const fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

pub const fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Offset at which Limine maps all physical memory into the higher half.
pub fn hhdm_offset() -> u64 {
    HHDM_OFFSET.load(Ordering::Relaxed)
}

/// Gives the bootloader's memory to the frame allocator: moves the kernel off
/// Limine's page tables (copies of the tables that live there), switches
/// every CPU and thread to the copy, then frees the regions. Afterwards no
/// Limine response may be used (they lived there too). Must run on a kernel
/// stack of our own, before user processes exist. Returns the bytes gained.
pub fn reclaim_bootloader_memory() -> u64 {
    let regions = pmm::bootloader_regions();
    let inside = |f: PhysAddr| regions.iter().any(|r| f.0 >= r.base && f.0 < r.base + r.length);
    let old = paging::kernel_root();
    let new = match paging::copy_kernel_tables(&inside) {
        Ok(root) => root,
        Err(e) => {
            crate::serial_println!(
                "mem: cannot copy the page tables ({:?}), bootloader memory stays reserved",
                e
            );
            return 0;
        }
    };
    paging::set_kernel_root(new);
    crate::task::retarget_cr3(old, new);
    use crate::arch::x86_64::smp;
    for cpu in 0..smp::online_count() {
        smp::run_on(cpu, move || unsafe { paging::write_cr3(new) });
    }
    // every CPU and thread is on the new tables now: nothing uses the old memory
    let frames: usize = regions.into_iter().map(pmm::release_region).sum();
    frames as u64 * PAGE_SIZE
}

/// Reads the HHDM offset from Limine, sets up the frame allocator and the kernel heap.
pub fn init() {
    let hhdm = crate::boot::HHDM.response().expect("Limine provided no HHDM response");
    HHDM_OFFSET.store(hhdm.offset, Ordering::Relaxed);
    pmm::init();
    paging::init();
    heap::init();
}

#[test_case]
fn alignment_helpers() {
    assert_eq!(align_down(0x1234, PAGE_SIZE), 0x1000);
    assert_eq!(align_up(0x1234, PAGE_SIZE), 0x2000);
    assert_eq!(align_up(0x2000, PAGE_SIZE), 0x2000);
}

#[test_case]
fn phys_to_virt_uses_hhdm() {
    assert!(hhdm_offset() >= 0xffff_8000_0000_0000);
    assert_eq!(PhysAddr(0x1000).to_virt().as_u64(), hhdm_offset() + 0x1000);
}
