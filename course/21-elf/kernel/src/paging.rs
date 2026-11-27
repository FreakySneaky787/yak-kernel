//! Paging: how the CPU turns a virtual address into a physical one, and how we change that.
//!
//! x86-64 uses four levels of tables. Each table is one 4 KiB frame with 512 entries of 8 bytes; bits 47-39 of a
//! virtual address pick the entry in the top table (PML4), bits 38-30 the one in the next (PDPT), then 29-21 (PD)
//! and 20-12 (PT). The PT entry names the frame, and bits 11-0 are the offset inside it. CR3 holds the physical
//! address of the PML4.
//!
//! The kernel keeps running on the tables Limine built. We reach every table through the HHDM - a table entry
//! holds a physical address, and `PhysAddr::as_mut_ptr` turns it into one we can use.
//!
//! Since chapter 21 there is more than one set of tables: every program gets a PML4 of its own, with the lower half
//! (the program's) empty at first and the upper half (the kernel's) copied from the kernel's PML4. The copy holds
//! the *addresses of the same lower-level tables*, so a mapping the kernel adds later - a heap page, a stack - shows
//! up in every address space at once - provided the PML4 entry above it existed when the copy was made. `init`
//! makes sure every entry of the upper half exists, and an empty table behind each.

use crate::mem::{PAGE_SIZE, PhysAddr};
use crate::pmm;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// The flags of a table entry (Intel SDM Vol. 3A, section 4.5) - the ones we use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags(u64);

impl Flags {
    pub const PRESENT: Flags = Flags(1 << 0);
    pub const WRITABLE: Flags = Flags(1 << 1);
    /// PCD, "page cache disable": every access goes to memory - or to the device behind the address.
    pub const NO_CACHE: Flags = Flags(1 << 4);
    /// In a PDPT or PD entry: this entry maps a whole 1 GiB or 2 MiB page itself, there is no table below it.
    pub const HUGE: Flags = Flags(1 << 7);
    /// U/S: ring 3 may use the page (chapter 20). Without it, only the kernel may.
    pub const USER: Flags = Flags(1 << 2);
    pub const NO_EXECUTE: Flags = Flags(1 << 63);
    /// Kernel data: present, writable, not executable.
    pub const DATA: Flags = Flags(Self::PRESENT.0 | Self::WRITABLE.0 | Self::NO_EXECUTE.0);
    /// Code that runs before the CPU knows about "no execute" - the start code of the other CPUs (smp.rs).
    pub const CODE: Flags = Flags(Self::PRESENT.0 | Self::WRITABLE.0);
    /// A page of a ring 3 program's code: present and USER - not writable, and executable (chapter 20).
    pub const USER_CODE: Flags = Flags(Self::PRESENT.0 | Self::USER.0);
    /// A page of its data or stack: writable, not executable.
    pub const USER_DATA: Flags = Flags(Self::DATA.0 | Self::USER.0);
    /// A page it may only read: constants (chapter 21 - an ELF segment with neither W nor X).
    pub const USER_READ_ONLY: Flags = Flags(Self::PRESENT.0 | Self::USER.0 | Self::NO_EXECUTE.0);
    /// Device registers: like data, but never cached - a cached read would return an old value, not the device's.
    pub const DEVICE: Flags = Flags(Self::DATA.0 | Self::NO_CACHE.0);

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
    /// An address space of a program covers only the lower half (chapter 21).
    OutsideUserSpace,
    /// Nothing is mapped at an address a program's memory was to be read or written at.
    NotMapped,
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

/// Switches to the tables whose PML4 is at `root` (chapter 21). Writing CR3 also flushes the TLB: the CPU forgets
/// every translation it cached from the old tables.
pub fn write_cr3(root: PhysAddr) {
    unsafe { asm!("mov cr3, {}", in(reg) root.0, options(nostack, preserves_flags)) };
}

/// The first PML4 entry of the upper half: entries 0-255 cover the lower half (a program's), 256-511 the kernel's.
const KERNEL_HALF: usize = 256;

/// The PML4 the kernel itself runs on - the one Limine built - once `init` has run.
static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);

pub fn kernel_root() -> PhysAddr {
    let root = KERNEL_ROOT.load(Ordering::Relaxed);
    assert!(root != 0, "paging::init has not run");
    PhysAddr(root)
}

/// Remembers the kernel's PML4, and gives every entry of its upper half a table behind it (empty, if nothing used
/// that part of the address space yet). From now on a mapping in the upper half only changes tables *below* the
/// PML4, which every copy of the upper half shares - see the top of this file. Runs once, before the first address
/// space is made.
///
/// Limine sets the USER bit on some of the entries it made (the kernel's and the HHDM's). The pages below them are
/// supervisor-only, so ring 3 could not use them anyway - but nothing of the kernel's half should look open to ring
/// 3, and every program's PML4 gets a copy: the bit is cleared.
pub fn init() {
    let root = read_cr3();
    KERNEL_ROOT.store(root.0, Ordering::Relaxed);
    for entry in &mut table(root).0[KERNEL_HALF..] {
        if entry.is(Flags::PRESENT) {
            entry.0 &= !Flags::USER.0;
        } else {
            let pdpt = pmm::alloc_frame_zeroed().expect("a frame for a table of the kernel half");
            *entry = Entry::new(pdpt, Flags(Flags::PRESENT.0 | Flags::WRITABLE.0));
        }
    }
}

/// A new PML4 for a program: the lower half empty, the upper half the kernel's (the same entries, so the same
/// tables below them). `None` when no frame is free.
pub fn new_root() -> Option<PhysAddr> {
    let root = pmm::alloc_frame_zeroed()?;
    let kernel = &table(kernel_root()).0;
    table(root).0[KERNEL_HALF..].copy_from_slice(&kernel[KERNEL_HALF..]);
    Some(root)
}

/// Gives back everything a program's address space owns: every frame mapped in its lower half, every table below
/// the PML4, and the PML4 itself. The upper half is not touched - its tables belong to the kernel. Every page of a
/// program owns its frame (`AddressSpace::map_fresh`), so nothing else is using one. Must not be the PML4 in use.
pub fn free_user_half(root: PhysAddr) {
    assert!(root != read_cr3(), "the tables in use cannot be freed");
    assert!(root != kernel_root(), "the kernel's tables are not a program's");
    free_tables(root, 4);
}

/// Frees the table at `frame` (level 4 = PML4 ... level 1 = PT) with everything below it.
fn free_tables(frame: PhysAddr, level: u32) {
    // in the PML4 only the lower half is ours
    let count = if level == 4 { KERNEL_HALF } else { 512 };
    for entry in &table(frame).0[..count] {
        if !entry.is(Flags::PRESENT) {
            continue;
        }
        if level == 1 {
            pmm::free_frame(entry.address()); // a page of the program
        } else {
            assert!(!entry.is(Flags::HUGE), "a program's address space has no huge pages");
            free_tables(entry.address(), level - 1);
        }
    }
    pmm::free_frame(frame);
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
    translate_in(read_cr3(), virt)
}

/// The same for the address space whose PML4 is at `root`, active or not (chapter 21).
pub fn translate_in(root: PhysAddr, virt: u64) -> Option<PhysAddr> {
    let mut entries = &table(root).0;
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
    map_in(read_cr3(), virt, frame, flags)
}

/// The same in the address space whose PML4 is at `root` - the one of a program that is not running (chapter 21).
pub fn map_in(root: PhysAddr, virt: u64, frame: PhysAddr, flags: Flags) -> Result<(), MapError> {
    if virt % PAGE_SIZE != 0 || !frame.is_page_aligned() {
        return Err(MapError::NotAligned);
    }
    let _lock = LOCK.lock();
    let [i4, i3, i2, i1] = indices(virt);
    let mut entries = &mut table(root).0;
    for index in [i4, i3, i2] {
        let entry = &mut entries[index];
        // ring 3 may use a page only if every table on the way says USER too (chapter 20)
        let user = flags.0 & Flags::USER.0;
        if !entry.is(Flags::PRESENT) {
            // a new, empty table: zeroed, or its garbage would be read as entries
            let new = pmm::alloc_frame_zeroed().ok_or(MapError::OutOfFrames)?;
            // the tables above allow everything; the last entry decides what the page may do
            *entry = Entry::new(new, Flags(Flags::PRESENT.0 | Flags::WRITABLE.0 | user));
        } else if entry.is(Flags::HUGE) {
            return Err(MapError::HugePageInTheWay);
        } else {
            entry.0 |= user;
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

/// Makes the 4 KiB at `frame` reachable at their HHDM address, if they are not already. Limine maps RAM there, but
/// not the registers of devices such as the APICs - those we map ourselves, uncached.
pub fn map_device(frame: PhysAddr) -> Result<u64, MapError> {
    let virt = frame.to_virt();
    if translate(virt) == Some(frame) {
        return Ok(virt);
    }
    map(virt, frame, Flags::DEVICE)?;
    Ok(virt)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An address nobody maps: the area `stack.rs` uses starts here, the tests stay below it.
    const SCRATCH: u64 = 0xFFFF_FE80_0000_0000;

    #[test_case]
    fn every_entry_of_the_upper_half_exists_and_is_closed_to_ring_3() {
        let pml4 = &table(kernel_root()).0;
        for (i, entry) in pml4.iter().enumerate().skip(KERNEL_HALF) {
            assert!(
                entry.is(Flags::PRESENT),
                "entry {} is empty: a later mapping would not be shared",
                i
            );
            assert!(!entry.is(Flags::USER), "entry {} has the USER bit", i);
        }
    }

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
        // the first page in an empty region of 512 GiB: a new PD and PT - the PDPT above them was made by `init`
        assert_eq!(pmm::free_frames(), free - 2);
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
