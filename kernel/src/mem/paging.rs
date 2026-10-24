//! 4-level x86_64 paging: walking, mapping and unmapping 4 KiB pages.
//!
//! A virtual address is split into four 9-bit table indices plus a 12-bit
//! offset: PML4 -> PDPT -> PD -> PT -> page. Each table is one 4 KiB frame with
//! 512 entries. We reach every table through the HHDM, so no recursive
//! mapping trick is needed.
//!
//! We operate on the page tables Limine created (CR3). Intermediate tables we
//! add come from the frame allocator. Empty tables are not freed on unmap
//! (keeps the code simple; the waste is at most a few frames).

use super::pmm::{alloc_frame_zeroed, free_frame};
use super::{PAGE_SIZE, PhysAddr, VirtAddr};
use crate::arch::x86_64::without_interrupts;
use core::arch::asm;
use core::fmt;
use spin::Mutex;

/// Page table entry flags (Intel SDM Vol. 3, 4.5).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Flags(u64);

impl Flags {
    pub const PRESENT: Flags = Flags(1 << 0);
    pub const WRITABLE: Flags = Flags(1 << 1);
    pub const USER: Flags = Flags(1 << 2);
    pub const WRITE_THROUGH: Flags = Flags(1 << 3);
    pub const NO_CACHE: Flags = Flags(1 << 4);
    pub const ACCESSED: Flags = Flags(1 << 5);
    pub const DIRTY: Flags = Flags(1 << 6);
    /// In a PDPT/PD entry: maps a 1 GiB / 2 MiB page directly.
    pub const HUGE: Flags = Flags(1 << 7);
    pub const GLOBAL: Flags = Flags(1 << 8);
    pub const NO_EXECUTE: Flags = Flags(1 << 63);

    pub const fn empty() -> Flags {
        Flags(0)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Typical flags for kernel data: present, writable, not executable.
    pub const KERNEL_DATA: Flags = Flags(Self::PRESENT.0 | Self::WRITABLE.0 | Self::NO_EXECUTE.0);
    /// Flags for memory-mapped device registers: like data, but uncached.
    pub const MMIO: Flags = Flags(Self::KERNEL_DATA.0 | Self::NO_CACHE.0 | Self::WRITE_THROUGH.0);
}

impl core::ops::BitOr for Flags {
    type Output = Flags;
    fn bitor(self, rhs: Flags) -> Flags {
        Flags(self.0 | rhs.0)
    }
}

impl fmt::Debug for Flags {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let names = [
            (Flags::PRESENT, "P"),
            (Flags::WRITABLE, "W"),
            (Flags::USER, "U"),
            (Flags::WRITE_THROUGH, "WT"),
            (Flags::NO_CACHE, "NC"),
            (Flags::HUGE, "H"),
            (Flags::GLOBAL, "G"),
            (Flags::NO_EXECUTE, "NX"),
        ];
        let mut first = true;
        for (flag, name) in names {
            if self.contains(flag) {
                if !first {
                    write!(f, "|")?;
                }
                write!(f, "{}", name)?;
                first = false;
            }
        }
        if first {
            write!(f, "-")?;
        }
        Ok(())
    }
}

const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
/// Flags that may appear in an entry (everything except the address bits).
const FLAG_MASK: u64 = !ADDR_MASK;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Entry(u64);

impl Entry {
    pub fn is_present(self) -> bool {
        self.0 & Flags::PRESENT.0 != 0
    }

    pub fn is_huge(self) -> bool {
        self.0 & Flags::HUGE.0 != 0
    }

    pub fn addr(self) -> PhysAddr {
        PhysAddr(self.0 & ADDR_MASK)
    }

    pub fn flags(self) -> Flags {
        Flags(self.0 & FLAG_MASK)
    }

    fn new(addr: PhysAddr, flags: Flags) -> Entry {
        Entry((addr.0 & ADDR_MASK) | flags.0)
    }
}

#[repr(C, align(4096))]
pub struct PageTable {
    entries: [Entry; 512],
}

#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    NotAligned,
    AlreadyMapped,
    /// The address is covered by a 2 MiB / 1 GiB page we cannot split.
    HugePageInTheWay,
    OutOfMemory,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UnmapError {
    NotAligned,
    NotMapped,
    HugePage,
}

/// Table indices for each level, PML4 first.
pub fn table_indices(virt: VirtAddr) -> [usize; 4] {
    let v = virt.0;
    [((v >> 39) & 0x1FF) as usize, ((v >> 30) & 0x1FF) as usize, ((v >> 21) & 0x1FF) as usize, ((v >> 12) & 0x1FF) as usize]
}

pub fn read_cr3() -> PhysAddr {
    let value: u64;
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags)) };
    PhysAddr(value & ADDR_MASK) // lower bits are flags / PCID
}

/// Removes a single page from the TLB (the CPU's translation cache).
pub fn flush_tlb(virt: VirtAddr) {
    unsafe { asm!("invlpg [{}]", in(reg) virt.0, options(nostack, preserves_flags)) };
}

fn table_at(phys: PhysAddr) -> &'static mut PageTable {
    unsafe { &mut *phys.to_virt().as_mut_ptr::<PageTable>() }
}

/// Serializes all page table modifications.
static PAGING_LOCK: Mutex<()> = Mutex::new(());

/// Returns the physical address `virt` is mapped to (handles huge pages).
pub fn translate(virt: VirtAddr) -> Option<PhysAddr> {
    let idx = table_indices(virt);
    let mut table = table_at(read_cr3());
    for (level, &i) in idx.iter().enumerate() {
        let entry = table.entries[i];
        if !entry.is_present() {
            return None;
        }
        // Level 1 (PDPT) huge = 1 GiB, level 2 (PD) huge = 2 MiB
        if entry.is_huge() && (level == 1 || level == 2) {
            let page_size = if level == 1 { 1 << 30 } else { 1 << 21 };
            return Some(PhysAddr(entry.addr().0 + (virt.0 & (page_size - 1))));
        }
        if level == 3 {
            return Some(PhysAddr(entry.addr().0 + (virt.0 & (PAGE_SIZE - 1))));
        }
        table = table_at(entry.addr());
    }
    None
}

/// Flags of the final (4 KiB) entry mapping `virt`, if any.
pub fn page_flags(virt: VirtAddr) -> Option<Flags> {
    let idx = table_indices(virt);
    let mut table = table_at(read_cr3());
    for &i in &idx[..3] {
        let entry = table.entries[i];
        if !entry.is_present() || entry.is_huge() {
            return None;
        }
        table = table_at(entry.addr());
    }
    let entry = table.entries[idx[3]];
    entry.is_present().then(|| entry.flags())
}

/// Maps the 4 KiB page at `virt` to the frame `phys`.
pub fn map_page(virt: VirtAddr, phys: PhysAddr, flags: Flags) -> Result<(), MapError> {
    if virt.0 % PAGE_SIZE != 0 || !phys.is_page_aligned() {
        return Err(MapError::NotAligned);
    }
    without_interrupts(|| {
        let _guard = PAGING_LOCK.lock();
        let idx = table_indices(virt);
        let mut table = table_at(read_cr3());
        // Intermediate tables get the most permissive flags; the leaf decides.
        let mut parent_flags = Flags::PRESENT | Flags::WRITABLE;
        if flags.contains(Flags::USER) {
            parent_flags = parent_flags | Flags::USER;
        }
        for &i in &idx[..3] {
            let entry = &mut table.entries[i];
            if !entry.is_present() {
                let frame = alloc_frame_zeroed().ok_or(MapError::OutOfMemory)?;
                *entry = Entry::new(frame, parent_flags);
            } else if entry.is_huge() {
                return Err(MapError::HugePageInTheWay);
            } else if flags.contains(Flags::USER) && !entry.flags().contains(Flags::USER) {
                *entry = Entry(entry.0 | Flags::USER.0);
            }
            table = table_at(entry.addr());
        }
        let leaf = &mut table.entries[idx[3]];
        if leaf.is_present() {
            return Err(MapError::AlreadyMapped);
        }
        *leaf = Entry::new(phys, flags | Flags::PRESENT);
        Ok(())
    })
}

/// Unmaps the 4 KiB page at `virt` and returns the frame it pointed to
/// (the caller decides whether to free it).
pub fn unmap_page(virt: VirtAddr) -> Result<PhysAddr, UnmapError> {
    if virt.0 % PAGE_SIZE != 0 {
        return Err(UnmapError::NotAligned);
    }
    without_interrupts(|| {
        let _guard = PAGING_LOCK.lock();
        let idx = table_indices(virt);
        let mut table = table_at(read_cr3());
        for &i in &idx[..3] {
            let entry = table.entries[i];
            if !entry.is_present() {
                return Err(UnmapError::NotMapped);
            }
            if entry.is_huge() {
                return Err(UnmapError::HugePage);
            }
            table = table_at(entry.addr());
        }
        let leaf = &mut table.entries[idx[3]];
        if !leaf.is_present() {
            return Err(UnmapError::NotMapped);
        }
        let frame = leaf.addr();
        *leaf = Entry(0);
        flush_tlb(virt);
        Ok(frame)
    })
}

/// Allocates fresh zeroed frames and maps `pages` pages starting at `virt`.
/// On failure, everything mapped so far is rolled back.
pub fn map_fresh(virt: VirtAddr, pages: usize, flags: Flags) -> Result<(), MapError> {
    for i in 0..pages {
        let page = VirtAddr(virt.0 + i as u64 * PAGE_SIZE);
        let result = alloc_frame_zeroed().ok_or(MapError::OutOfMemory).and_then(|frame| {
            map_page(page, frame, flags).inspect_err(|_| free_frame(frame))
        });
        if let Err(e) = result {
            unmap_and_free(virt, i);
            return Err(e);
        }
    }
    Ok(())
}

/// Unmaps `pages` pages starting at `virt` and frees their frames.
pub fn unmap_and_free(virt: VirtAddr, pages: usize) {
    for i in 0..pages {
        if let Ok(frame) = unmap_page(VirtAddr(virt.0 + i as u64 * PAGE_SIZE)) {
            free_frame(frame);
        }
    }
}

/// Maps a physical MMIO range (e.g. device registers) uncached at `virt`.
pub fn map_mmio(virt: VirtAddr, phys: PhysAddr, size: u64) -> Result<(), MapError> {
    let pages = size.div_ceil(PAGE_SIZE);
    for i in 0..pages {
        let offset = i * PAGE_SIZE;
        map_page(VirtAddr(virt.0 + offset), PhysAddr(phys.0 + offset), Flags::MMIO)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::hhdm_offset;

    /// Free virtual area for tests (PML4 slot 509, unused by Limine and us).
    const TEST_AREA: u64 = 0xFFFF_FE80_0000_0000;

    #[test_case]
    fn indices_split_address() {
        let v = VirtAddr(0xFFFF_FFFF_8000_1234);
        assert_eq!(table_indices(v), [511, 510, 0, 1]);
    }

    #[test_case]
    fn translate_hhdm_and_kernel() {
        assert_eq!(translate(VirtAddr(hhdm_offset() + 0x10_0000)), Some(PhysAddr(0x10_0000)));
        let code = crate::init as fn() as usize as u64;
        assert!(translate(VirtAddr(code)).is_some(), "kernel code not mapped?");
        assert_eq!(translate(VirtAddr(0x0000_4000_0000_0000)), None);
    }

    #[test_case]
    fn map_write_read_unmap() {
        let virt = VirtAddr(TEST_AREA);
        let frame = alloc_frame_zeroed().unwrap();
        map_page(virt, frame, Flags::KERNEL_DATA).unwrap();
        assert_eq!(translate(virt), Some(frame));
        assert_eq!(page_flags(virt), Some(Flags::KERNEL_DATA));

        // write through the new mapping, read through the HHDM
        unsafe { virt.as_mut_ptr::<u64>().write_volatile(0x1122_3344_5566_7788) };
        let seen = unsafe { frame.to_virt().as_ptr::<u64>().read_volatile() };
        assert_eq!(seen, 0x1122_3344_5566_7788);

        assert_eq!(map_page(virt, frame, Flags::KERNEL_DATA), Err(MapError::AlreadyMapped));
        assert_eq!(unmap_page(virt), Ok(frame));
        assert_eq!(translate(virt), None);
        assert_eq!(unmap_page(virt), Err(UnmapError::NotMapped));
        free_frame(frame);
    }

    #[test_case]
    fn map_fresh_and_free_restores_memory() {
        let virt = VirtAddr(TEST_AREA + 0x10_0000);
        map_fresh(virt, 8, Flags::KERNEL_DATA).unwrap(); // may allocate page tables once
        unmap_and_free(virt, 8);
        let before = crate::mem::pmm::stats().free_frames;
        map_fresh(virt, 8, Flags::KERNEL_DATA).unwrap();
        for i in 0..8u64 {
            let p = VirtAddr(virt.0 + i * PAGE_SIZE).as_mut_ptr::<u64>();
            unsafe { assert_eq!(p.read_volatile(), 0) }; // zeroed
            unsafe { p.write_volatile(i) };
        }
        assert_eq!(crate::mem::pmm::stats().free_frames, before - 8);
        unmap_and_free(virt, 8);
        assert_eq!(crate::mem::pmm::stats().free_frames, before);
    }

    #[test_case]
    fn misaligned_requests_rejected() {
        assert_eq!(map_page(VirtAddr(TEST_AREA + 1), PhysAddr(0x1000), Flags::KERNEL_DATA), Err(MapError::NotAligned));
        assert_eq!(unmap_page(VirtAddr(TEST_AREA + 1)), Err(UnmapError::NotAligned));
    }

    #[test_case]
    fn flags_debug_format() {
        use core::fmt::Write;
        struct Buf([u8; 32], usize);
        impl Write for Buf {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.0[self.1..self.1 + s.len()].copy_from_slice(s.as_bytes());
                self.1 += s.len();
                Ok(())
            }
        }
        let mut b = Buf([0; 32], 0);
        write!(b, "{:?}", Flags::KERNEL_DATA).unwrap();
        assert_eq!(core::str::from_utf8(&b.0[..b.1]).unwrap(), "P|W|NX");
    }
}
