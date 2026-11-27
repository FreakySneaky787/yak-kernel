//! An address space of its own for every program (chapter 21).
//!
//! Until chapter 20 all code ran on the one set of page tables Limine built, and the three pages of a ring 3
//! program lived in it. Now a program gets a PML4 of its own: the same virtual address - 0x400000, say - leads to
//! different frames in different programs, so one program cannot even *name* another's memory. The upper half of
//! every PML4 is the kernel's (`paging::new_root`), so the kernel is there whichever tables are loaded: interrupts,
//! system calls and the scheduler need no switch of tables to run.
//!
//! An `AddressSpace` owns everything mapped in its lower half: when it is dropped, every frame and every table
//! goes back to the frame allocator (`paging::free_user_half`).

use crate::mem::{PAGE_SIZE, PhysAddr};
use crate::paging::{self, Flags, MapError};
use crate::pmm;

/// Everything below this address is the lower half - the only part of the address space a program owns.
pub const USER_END: u64 = 0x0000_8000_0000_0000;

pub struct AddressSpace {
    root: PhysAddr,
}

impl AddressSpace {
    /// An empty address space, or `None` when no frame is free.
    pub fn new() -> Option<AddressSpace> {
        Some(AddressSpace {
            root: paging::new_root()?,
        })
    }

    /// The physical address of its PML4 - what goes into CR3 when a thread of this space runs.
    pub fn root(&self) -> PhysAddr {
        self.root
    }

    /// Maps a new, zeroed frame at `virt` (a multiple of the page size, in the lower half). The frame belongs to the
    /// address space from now on.
    pub fn map_fresh(&mut self, virt: u64, flags: Flags) -> Result<(), MapError> {
        if virt >= USER_END {
            return Err(MapError::OutsideUserSpace);
        }
        let frame = pmm::alloc_frame_zeroed().ok_or(MapError::OutOfFrames)?;
        paging::map_in(self.root, virt, frame, flags).inspect_err(|_| pmm::free_frame(frame))
    }

    /// Whether a page is mapped at `virt`.
    #[cfg(test)]
    pub fn is_mapped(&self, virt: u64) -> bool {
        virt < USER_END && paging::translate_in(self.root, virt).is_some()
    }

    /// Copies `bytes` to the addresses from `virt` on - over page boundaries, and into pages the program itself may
    /// not write (its code): the kernel reaches the frames through the HHDM, whatever the flags say.
    pub fn write(&mut self, virt: u64, bytes: &[u8]) -> Result<(), MapError> {
        let mut done = 0;
        while done < bytes.len() {
            let frame = self.frame_at(virt, done as u64)?;
            let n = (PAGE_SIZE as usize - frame.0 as usize % PAGE_SIZE as usize).min(bytes.len() - done);
            unsafe { core::ptr::copy_nonoverlapping(bytes[done..].as_ptr(), frame.as_mut_ptr::<u8>(), n) };
            done += n;
        }
        Ok(())
    }

    /// Copies bytes out of the address space - the kernel looking at what a program did.
    #[cfg(test)]
    pub fn read(&self, virt: u64, buf: &mut [u8]) -> Result<(), MapError> {
        let mut done = 0;
        while done < buf.len() {
            let frame = self.frame_at(virt, done as u64)?;
            let n = (PAGE_SIZE as usize - frame.0 as usize % PAGE_SIZE as usize).min(buf.len() - done);
            unsafe { core::ptr::copy_nonoverlapping(frame.as_mut_ptr::<u8>(), buf[done..].as_mut_ptr(), n) };
            done += n;
        }
        Ok(())
    }

    /// The physical address of the byte at `virt + offset`.
    fn frame_at(&self, virt: u64, offset: u64) -> Result<PhysAddr, MapError> {
        let at = virt.checked_add(offset).filter(|&at| at < USER_END);
        let at = at.ok_or(MapError::OutsideUserSpace)?;
        paging::translate_in(self.root, at).ok_or(MapError::NotMapped)
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        paging::free_user_half(self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: u64 = 0x40_0000;

    #[test_case]
    fn a_new_space_has_the_kernel_and_nothing_else() {
        let space = AddressSpace::new().unwrap();
        assert!(!space.is_mapped(PAGE), "the lower half is empty");
        // the kernel's code is mapped in the upper half - at the same frame as in the kernel's own tables
        let code = paging::translate as *const () as u64;
        assert!(paging::translate_in(space.root(), code).is_some());
        assert_eq!(paging::translate_in(space.root(), code), paging::translate(code));
    }

    #[test_case]
    fn the_same_address_leads_to_different_frames() {
        let mut a = AddressSpace::new().unwrap();
        let mut b = AddressSpace::new().unwrap();
        a.map_fresh(PAGE, Flags::USER_DATA).unwrap();
        b.map_fresh(PAGE, Flags::USER_DATA).unwrap();
        a.write(PAGE, b"in A").unwrap();
        b.write(PAGE, b"in B").unwrap();
        let (mut in_a, mut in_b) = ([0; 4], [0; 4]);
        a.read(PAGE, &mut in_a).unwrap();
        b.read(PAGE, &mut in_b).unwrap();
        assert_eq!((&in_a, &in_b), (b"in A", b"in B"));
        assert_ne!(
            paging::translate_in(a.root(), PAGE),
            paging::translate_in(b.root(), PAGE)
        );
        assert_eq!(
            paging::translate(PAGE),
            None,
            "and the kernel's own tables never saw it"
        );
    }

    #[test_case]
    fn what_the_kernel_maps_later_shows_up_in_every_space() {
        // the upper half is shared below the PML4, so a kernel mapping made *after* the space exists is in it too
        const KERNEL_PAGE: u64 = 0xFFFF_F000_0000_0000; // PML4 entry 480: no other test maps there
        let space = AddressSpace::new().unwrap();
        let frame = pmm::alloc_frame_zeroed().unwrap();
        assert_eq!(paging::translate_in(space.root(), KERNEL_PAGE), None);
        paging::map(KERNEL_PAGE, frame, Flags::DATA).unwrap();
        assert_eq!(paging::translate_in(space.root(), KERNEL_PAGE), Some(frame));
        assert_eq!(paging::unmap(KERNEL_PAGE), Some(frame));
        pmm::free_frame(frame);
    }

    #[test_case]
    fn writing_crosses_page_boundaries_and_stops_at_holes() {
        let mut space = AddressSpace::new().unwrap();
        space.map_fresh(PAGE, Flags::USER_DATA).unwrap();
        space.map_fresh(PAGE + PAGE_SIZE, Flags::USER_DATA).unwrap();
        let text = b"across the border";
        space.write(PAGE + PAGE_SIZE - 6, text).unwrap();
        let mut back = [0; 17];
        space.read(PAGE + PAGE_SIZE - 6, &mut back).unwrap();
        assert_eq!(&back, text);
        // a page that is not there: an error, and the bytes before it were written
        assert_eq!(space.write(PAGE + 2 * PAGE_SIZE - 2, b"abcd"), Err(MapError::NotMapped));
        assert_eq!(space.write(USER_END, b"abcd"), Err(MapError::OutsideUserSpace));
        assert_eq!(space.write(u64::MAX - 1, b"abcd"), Err(MapError::OutsideUserSpace));
        assert_eq!(
            space.map_fresh(USER_END, Flags::USER_DATA),
            Err(MapError::OutsideUserSpace)
        );
        assert_eq!(space.map_fresh(PAGE, Flags::USER_DATA), Err(MapError::AlreadyMapped));
    }

    #[test_case]
    fn dropping_a_space_gives_every_frame_back() {
        let free = pmm::free_frames();
        let mut space = AddressSpace::new().unwrap();
        for i in 0..40 {
            space.map_fresh(PAGE + i * PAGE_SIZE, Flags::USER_DATA).unwrap();
        }
        space.map_fresh(0x7FFF_FFFF_0000, Flags::USER_DATA).unwrap(); // far away: tables of its own
        // 41 pages + PML4 + two PDPT/PD/PT chains
        assert_eq!(free - pmm::free_frames(), 41 + 1 + 6);
        drop(space);
        assert_eq!(pmm::free_frames(), free);
    }
}
