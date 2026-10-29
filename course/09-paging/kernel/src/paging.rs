//! Paging: how the CPU turns a virtual address into a physical one, and how we change that.
//!
//! x86-64 uses four levels of tables. Each table is one 4 KiB frame with 512 entries of 8 bytes; bits 47-39 of a
//! virtual address pick the entry in the top table (PML4), bits 38-30 the one in the next (PDPT), then 29-21 (PD)
//! and 20-12 (PT). The PT entry names the frame, and bits 11-0 are the offset inside it. CR3 holds the physical
//! address of the PML4.
//!
//! The kernel keeps running on the tables Limine built. We reach every table through the HHDM - a table entry
//! holds a physical address, and `PhysAddr::as_mut_ptr` turns it into one we can use.

use crate::mem::{PAGE_SIZE, PhysAddr};
use crate::pmm;
use core::arch::asm;
use spin::Mutex;

/// The flags of a table entry (Intel SDM Vol. 3A, section 4.5) - the ones we use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags(u64);

impl Flags {
    pub const PRESENT: Flags = Flags(1 << 0);
    pub const WRITABLE: Flags = Flags(1 << 1);
    /// In a PDPT or PD entry: this entry maps a whole 1 GiB or 2 MiB page itself, there is no table below it.
    pub const HUGE: Flags = Flags(1 << 7);
    pub const NO_EXECUTE: Flags = Flags(1 << 63);
    /// Kernel data: present, writable, not executable.
    pub const DATA: Flags = Flags(Self::PRESENT.0 | Self::WRITABLE.0 | Self::NO_EXECUTE.0);

    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }
}

/// Bits 12-51 of an entry: the physical address of the next table or of the frame. The rest are flags.
const ADDRESS_BITS: u64 = 0x000F_FFFF_FFFF_F000;

#[derive(Clone, Copy)]
#[repr(transparent)]
struct Entry(u64);

impl Entry {
    fn new(address: PhysAddr, flags: Flags) -> Entry {
        Entry(address.0 & ADDRESS_BITS | flags.0)
    }

    fn is(self, flag: Flags) -> bool {
        Flags(self.0).contains(flag)
    }

    fn address(self) -> PhysAddr {
        PhysAddr(self.0 & ADDRESS_BITS)
    }

    fn flags(self) -> Flags {
        Flags(self.0 & !ADDRESS_BITS)
    }
}

#[repr(C, align(4096))]
struct Table([Entry; 512]);

#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    NotAligned,
    AlreadyMapped,
    /// A 2 MiB or 1 GiB page covers the address already.
    HugePageInTheWay,
    OutOfFrames,
}

/// The entry numbers of `virt` in the four tables, PML4 first.
pub fn indices(virt: u64) -> [usize; 4] {
    [39, 30, 21, 12].map(|shift| ((virt >> shift) & 0x1FF) as usize)
}

/// The physical address of the PML4 in use.
pub fn read_cr3() -> PhysAddr {
    let cr3: u64;
    unsafe { asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
    PhysAddr(cr3 & ADDRESS_BITS) // the low bits are flags
}

/// The table in the frame at `address`, through the HHDM.
fn table(address: PhysAddr) -> &'static mut Table {
    // Safety: only called with addresses taken from CR3 or from a present entry - a page table
    unsafe { &mut *address.as_mut_ptr::<Table>() }
}

/// Only one change to the tables at a time. No interrupt handler maps pages, so interrupts may stay on.
static LOCK: Mutex<()> = Mutex::new(());

/// The physical address `virt` is mapped to, or `None` - what the CPU does on every memory access, in software.
pub fn translate(virt: u64) -> Option<PhysAddr> {
    let mut entries = &table(read_cr3()).0;
    for (level, index) in indices(virt).into_iter().enumerate() {
        let entry = entries[index];
        if !entry.is(Flags::PRESENT) {
            return None;
        }
        // a huge page in the PDPT (level 1) covers 1 GiB, in the PD (level 2) 2 MiB; a PT entry 4 KiB
        let page_size = match level {
            1 if entry.is(Flags::HUGE) => 1 << 30,
            2 if entry.is(Flags::HUGE) => 1 << 21,
            3 => PAGE_SIZE,
            _ => {
                entries = &table(entry.address()).0;
                continue;
            }
        };
        // in a huge page entry, bit 12 is no address bit but PAT (the caching type) - round it away
        let start = entry.address().0 & !(page_size - 1);
        return Some(PhysAddr(start + (virt & (page_size - 1))));
    }
    unreachable!("level 3 always returns")
}

/// The flags of the 4 KiB page at `virt`, if it is mapped as one.
pub fn flags(virt: u64) -> Option<Flags> {
    let [i4, i3, i2, i1] = indices(virt);
    let mut entries = &table(read_cr3()).0;
    for index in [i4, i3, i2] {
        let entry = entries[index];
        if !entry.is(Flags::PRESENT) || entry.is(Flags::HUGE) {
            return None;
        }
        entries = &table(entry.address()).0;
    }
    let leaf = entries[i1];
    leaf.is(Flags::PRESENT).then(|| leaf.flags())
}

/// Maps the 4 KiB page at `virt` to the frame `frame`. Missing tables on the way come from the frame allocator.
pub fn map(virt: u64, frame: PhysAddr, flags: Flags) -> Result<(), MapError> {
    if virt % PAGE_SIZE != 0 || !frame.is_page_aligned() {
        return Err(MapError::NotAligned);
    }
    let _lock = LOCK.lock();
    let [i4, i3, i2, i1] = indices(virt);
    let mut entries = &mut table(read_cr3()).0;
    for index in [i4, i3, i2] {
        let entry = &mut entries[index];
        if !entry.is(Flags::PRESENT) {
            // a new, empty table: zeroed, or its garbage would be read as entries
            let new = pmm::alloc_frame_zeroed().ok_or(MapError::OutOfFrames)?;
            // the tables above allow everything; the last entry decides what the page may do
            *entry = Entry::new(new, Flags(Flags::PRESENT.0 | Flags::WRITABLE.0));
        } else if entry.is(Flags::HUGE) {
            return Err(MapError::HugePageInTheWay);
        }
        entries = &mut table(entry.address()).0;
    }
    if entries[i1].is(Flags::PRESENT) {
        return Err(MapError::AlreadyMapped);
    }
    entries[i1] = Entry::new(frame, Flags(flags.0 | Flags::PRESENT.0));
    Ok(())
}

/// Removes the mapping of the page at `virt` and returns its frame - the caller decides whether to free it.
pub fn unmap(virt: u64) -> Option<PhysAddr> {
    let _lock = LOCK.lock();
    let [i4, i3, i2, i1] = indices(virt);
    let mut entries = &mut table(read_cr3()).0;
    for index in [i4, i3, i2] {
        let entry = entries[index];
        if !entry.is(Flags::PRESENT) || entry.is(Flags::HUGE) {
            return None;
        }
        entries = &mut table(entry.address()).0;
    }
    let leaf = entries[i1];
    if !leaf.is(Flags::PRESENT) {
        return None;
    }
    entries[i1] = Entry(0);
    // the CPU keeps recent translations in its TLB; without this it could go on using the old one
    unsafe { asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
    Some(leaf.address())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An address nobody maps: the area `stack.rs` uses starts here, the tests stay below it.
    const SCRATCH: u64 = 0xFFFF_FE80_0000_0000;

    #[test_case]
    fn an_address_is_four_indices_and_an_offset() {
        // 0xffffffff80000000, where the kernel is linked: entry 511, 510, 0, 0
        assert_eq!(indices(0xFFFF_FFFF_8000_0000), [511, 510, 0, 0]);
        assert_eq!(indices(0x0000_0080_4020_1000), [1, 1, 1, 1]);
    }

    #[test_case]
    fn the_kernel_is_where_limine_put_it() {
        let kernel = crate::EXECUTABLE_ADDRESS
            .response()
            .expect("Limine gave the kernel's address");
        let code = translate as *const () as u64;
        let expected = kernel.physical_base + (code - kernel.virtual_base);
        assert_eq!(translate(code), Some(PhysAddr(expected)));
    }

    #[test_case]
    fn the_hhdm_maps_every_frame_to_itself() {
        let frame = pmm::alloc_frame().unwrap();
        assert_eq!(translate(frame.to_virt() + 123), Some(PhysAddr(frame.0 + 123)));
        pmm::free_frame(frame);
        assert_eq!(translate(0x62000), None); // the lower half is empty (chapter 8's page fault)
    }

    #[test_case]
    fn a_mapped_frame_appears_at_its_new_address() {
        let frame = pmm::alloc_frame_zeroed().unwrap();
        let free = pmm::free_frames();
        map(SCRATCH, frame, Flags::DATA).unwrap();
        // the first page in an empty region of 512 GiB: a new PDPT, PD and PT
        assert_eq!(pmm::free_frames(), free - 3);
        assert_eq!(translate(SCRATCH + 8), Some(PhysAddr(frame.0 + 8)));
        assert_eq!(flags(SCRATCH), Some(Flags::DATA));
        unsafe { (SCRATCH as *mut u64).write_volatile(0x5EED) };
        assert_eq!(unsafe { frame.as_mut_ptr::<u64>().read() }, 0x5EED); // one frame, two addresses

        assert_eq!(map(SCRATCH, frame, Flags::DATA), Err(MapError::AlreadyMapped));
        assert_eq!(map(SCRATCH + 1, frame, Flags::DATA), Err(MapError::NotAligned));
        assert_eq!(unmap(SCRATCH), Some(frame));
        assert_eq!(translate(SCRATCH), None);
        assert_eq!(unmap(SCRATCH), None);
        pmm::free_frame(frame);
    }

    #[test_case]
    fn the_framebuffer_translates_exactly() {
        // Limine maps the framebuffer with 2 MiB pages whose PAT bit (bit 12) is set: it must not shift the result
        let fb = crate::FRAMEBUFFER.response().unwrap().framebuffers().first().unwrap();
        let virt = fb.address() as u64;
        assert_eq!(translate(virt), Some(PhysAddr(virt - crate::mem::hhdm_offset())));
    }

    #[test_case]
    fn huge_pages_are_not_split() {
        // Limine builds the HHDM from 2 MiB pages wherever a range allows it (4 KiB pages only at the edges): the
        // HHDM address of physical 4 MiB lies inside one, and a 4 KiB page cannot be put into it
        let virt = PhysAddr(0x40_0000).to_virt();
        assert_eq!(
            map(virt, PhysAddr(0x40_0000), Flags::DATA),
            Err(MapError::HugePageInTheWay)
        );
        assert_eq!(translate(virt + 0x1234), Some(PhysAddr(0x40_1234)));
    }
}
