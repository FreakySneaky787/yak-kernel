//! Address spaces of user processes.
//!
//! Each one has its own PML4. The lower half (0 .. 0x0000_8000_0000_0000)
//! belongs to the process; the upper half is shared with the kernel (see
//! `paging`), so the kernel stays mapped while a process runs and system
//! calls can access user memory directly.
//!
//! Frames are filled through the HHDM, so building an address space does not
//! require switching to it.

use super::paging::{self, Flags, MapError};
use super::pmm::{alloc_frame_zeroed, free_frame};
use super::{PAGE_SIZE, PhysAddr, VirtAddr, align_down};

/// End of the user half (exclusive). Higher addresses belong to the kernel.
pub const USER_END: u64 = 0x0000_8000_0000_0000;

pub struct AddressSpace {
    root: PhysAddr,
}

impl AddressSpace {
    /// A new address space: empty user half, shared kernel half.
    pub fn new() -> Result<AddressSpace, MapError> {
        let root = alloc_frame_zeroed().ok_or(MapError::OutOfMemory)?;
        paging::share_kernel_half(root);
        Ok(AddressSpace { root })
    }

    /// Physical address of the PML4 (the value for CR3).
    pub fn root(&self) -> PhysAddr {
        self.root
    }

    /// Returns the frame behind the user page `virt`, allocating, zeroing and
    /// mapping one with `flags` (plus USER) if the page is not mapped yet.
    pub fn page_frame(&mut self, virt: VirtAddr, flags: Flags) -> Result<PhysAddr, MapError> {
        assert!(virt.0 < USER_END, "not a user address: {:?}", virt);
        let page = VirtAddr(align_down(virt.0, PAGE_SIZE));
        if let Some(frame) = paging::translate_in(self.root, page) {
            return Ok(frame);
        }
        let frame = alloc_frame_zeroed().ok_or(MapError::OutOfMemory)?;
        paging::map_page_in(self.root, page, frame, flags | Flags::USER).inspect_err(|_| free_frame(frame))?;
        Ok(frame)
    }

    /// Maps `pages` fresh zeroed pages starting at `virt`. On failure the
    /// pages mapped so far are released again.
    pub fn map_fresh(&mut self, virt: VirtAddr, pages: usize, flags: Flags) -> Result<(), MapError> {
        for i in 0..pages as u64 {
            let page = VirtAddr(virt.0 + i * PAGE_SIZE);
            let result = if paging::translate_in(self.root, page).is_some() {
                Err(MapError::AlreadyMapped)
            } else {
                self.page_frame(page, flags).map(|_| ())
            };
            if let Err(e) = result {
                self.unmap(virt, i as usize);
                return Err(e);
            }
        }
        Ok(())
    }

    /// Unmaps `pages` pages starting at `virt` and frees their frames
    /// (pages that are not mapped are skipped).
    pub fn unmap(&mut self, virt: VirtAddr, pages: usize) {
        for i in 0..pages as u64 {
            if let Ok(frame) = paging::unmap_page_in(self.root, VirtAddr(virt.0 + i * PAGE_SIZE)) {
                free_frame(frame);
            }
        }
    }

    /// Copies `data` to the user address `virt` (pages must be mapped).
    pub fn write(&mut self, virt: VirtAddr, data: &[u8]) {
        let mut done = 0;
        while done < data.len() {
            let addr = virt.0 + done as u64;
            let offset = (addr % PAGE_SIZE) as usize;
            let chunk = (PAGE_SIZE as usize - offset).min(data.len() - done);
            let frame = paging::translate_in(self.root, VirtAddr(align_down(addr, PAGE_SIZE)))
                .expect("AddressSpace::write to an unmapped page");
            let dst = (frame.to_virt().0 + offset as u64) as *mut u8;
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), dst, chunk) };
            done += chunk;
        }
    }

    /// Flags of the user page containing `virt`.
    pub fn page_flags(&self, virt: VirtAddr) -> Option<Flags> {
        paging::page_flags_in(self.root, VirtAddr(align_down(virt.0, PAGE_SIZE)))
    }

    /// Whether `[addr, addr + len)` lies in user space and every page of it is
    /// mapped for user access (and writable, if `writable`). An empty range is
    /// always fine (Rust passes dangling pointers like 0x1 for empty slices).
    pub fn is_accessible(&self, addr: u64, len: u64, writable: bool) -> bool {
        if len == 0 {
            return true;
        }
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        if end > USER_END {
            return false;
        }
        let mut need = Flags::PRESENT | Flags::USER;
        if writable {
            need = need | Flags::WRITABLE;
        }
        let mut page = align_down(addr, PAGE_SIZE);
        while page < end {
            if !self.page_flags(VirtAddr(page)).is_some_and(|f| f.contains(need)) {
                return false;
            }
            page += PAGE_SIZE;
        }
        true
    }
}

impl Drop for AddressSpace {
    /// Frees all user pages and page tables. The address space must not be active.
    fn drop(&mut self) {
        paging::free_user_half(self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::pmm;

    #[test_case]
    fn new_space_shares_the_kernel_half() {
        let space = AddressSpace::new().unwrap();
        let code = VirtAddr(crate::init as fn() as usize as u64);
        assert_eq!(paging::translate_in(space.root(), code), paging::translate(code));
        assert_eq!(paging::translate_in(space.root(), VirtAddr(0x40_0000)), None);
    }

    #[test_case]
    fn mappings_are_private_and_freed() {
        let lost = pmm::frames_lost(|| {
            let mut a = AddressSpace::new().unwrap();
            let b = AddressSpace::new().unwrap();
            let virt = VirtAddr(0x40_0000);
            a.map_fresh(virt, 3, Flags::WRITABLE).unwrap();
            a.write(VirtAddr(virt.0 + 4090), b"hello world"); // crosses a page boundary
            let frame = paging::translate_in(a.root(), VirtAddr(virt.0 + PAGE_SIZE)).unwrap();
            let page2 = unsafe { *frame.to_virt().as_ptr::<[u8; 5]>() };
            assert_eq!(&page2, b"world");
            assert!(
                paging::translate_in(b.root(), virt).is_none(),
                "mapping leaked into another space"
            );
            assert!(
                paging::translate(virt).is_none(),
                "mapping leaked into the kernel space"
            );
            assert!(a.is_accessible(virt.0, 3 * PAGE_SIZE, true));
            assert!(!a.is_accessible(virt.0, 3 * PAGE_SIZE + 1, false));
            assert!(!a.is_accessible(u64::MAX - 4, 10, false));
            assert!(!b.is_accessible(virt.0, 1, false));
            assert!(b.is_accessible(1, 0, true), "empty slices have dangling pointers");
            assert_eq!(a.map_fresh(virt, 1, Flags::WRITABLE), Err(MapError::AlreadyMapped));
            // a failing map_fresh rolls back: pages before the conflict are released
            let frames = pmm::stats().free_frames;
            assert_eq!(
                a.map_fresh(VirtAddr(virt.0 - 2 * PAGE_SIZE), 3, Flags::WRITABLE),
                Err(MapError::AlreadyMapped)
            );
            assert!(a.page_flags(VirtAddr(virt.0 - 2 * PAGE_SIZE)).is_none());
            assert!(pmm::stats().free_frames >= frames - 1, "rollback leaked frames"); // maybe one new page table
            a.unmap(VirtAddr(virt.0 + PAGE_SIZE), 2);
            assert!(a.page_flags(VirtAddr(virt.0 + PAGE_SIZE)).is_none());
            assert!(a.page_flags(virt).is_some());
        });
        assert_eq!(lost, 0, "address space leaked frames");
    }

    #[test_case]
    fn kernel_pages_are_not_user_accessible() {
        let space = AddressSpace::new().unwrap();
        let code = crate::init as fn() as usize as u64;
        assert!(!space.is_accessible(code, 1, false));
        assert!(!space.is_accessible(USER_END - 8, 16, false));
    }
}
