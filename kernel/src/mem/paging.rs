//! 4-level x86_64 paging: walking, mapping and unmapping 4 KiB pages.
//!
//! A virtual address is split into four 9-bit table indices plus a 12-bit
//! offset: PML4 -> PDPT -> PD -> PT -> page. Each table is one 4 KiB frame with
//! 512 entries. We reach every table through the HHDM, so no recursive
//! mapping trick is needed.
//!
//! The kernel runs on the page tables Limine created (`kernel_root`).
//! Intermediate tables we add come from the frame allocator. Empty tables are
//! not freed on unmap (keeps the code simple; the waste is at most a few frames).
//!
//! User processes get their own PML4 (`mem::address_space`). Its upper half
//! (entries 256..511) is a copy of the kernel's, so all address spaces share
//! the kernel's lower-level tables. For that to stay true, `init` creates
//! every upper-half PML4 entry up front: later kernel mappings only change
//! shared tables below the PML4, never the PML4 itself.

use super::pmm::{alloc_frame_zeroed, free_frame};
use super::{PAGE_SIZE, PhysAddr, VirtAddr};
use crate::arch::x86_64::without_interrupts;
use core::arch::asm;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};
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
    [
        ((v >> 39) & 0x1FF) as usize,
        ((v >> 30) & 0x1FF) as usize,
        ((v >> 21) & 0x1FF) as usize,
        ((v >> 12) & 0x1FF) as usize,
    ]
}

pub fn read_cr3() -> PhysAddr {
    let value: u64;
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags)) };
    PhysAddr(value & ADDR_MASK) // lower bits are flags / PCID
}

/// Activates the address space whose PML4 is at `root` (flushes the TLB).
///
/// # Safety
/// `root` must map the kernel (its upper half must be the kernel's).
pub unsafe fn write_cr3(root: PhysAddr) {
    unsafe { asm!("mov cr3, {}", in(reg) root.0, options(nostack, preserves_flags)) };
}

/// First PML4 index of the higher half (kernel space).
pub const KERNEL_PML4_START: usize = 256;

static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);

/// PML4 of the kernel address space (the one Limine built).
pub fn kernel_root() -> PhysAddr {
    match KERNEL_ROOT.load(Ordering::Relaxed) {
        0 => read_cr3(),
        root => PhysAddr(root),
    }
}

/// Remembers the kernel PML4 and creates all missing upper-half PML4
/// entries (one empty PDPT each), see the module docs. Needs the frame allocator.
pub fn init() {
    let root = read_cr3();
    KERNEL_ROOT.store(root.0, Ordering::Relaxed);
    let pml4 = table_at(root);
    for entry in &mut pml4.entries[KERNEL_PML4_START..] {
        if !entry.is_present() {
            let frame = alloc_frame_zeroed().expect("out of memory for kernel page tables");
            *entry = Entry::new(frame, Flags::PRESENT | Flags::WRITABLE);
        }
        // Limine sets USER on its intermediate tables (the leaves are
        // supervisor-only). Clearing it here locks ring 3 out of the whole
        // upper half, whatever the lower levels say.
        *entry = Entry(entry.0 & !Flags::USER.0);
    }
    unsafe { write_cr3(root) }; // flush cached translations
}

/// Builds a new kernel PML4 in which every page table that lies in memory
/// for which `moving(frame)` is true is replaced by a copy in a fresh frame
/// (the mappings stay exactly the same). Tables elsewhere are kept and only
/// get their entries pointed at the copies of their children - that is safe
/// while they are live, because old and new children map the same thing.
/// Used to get the kernel off the bootloader's page tables.
pub fn copy_kernel_tables(moving: &dyn Fn(PhysAddr) -> bool) -> Result<PhysAddr, MapError> {
    fn copy(
        table: PhysAddr,
        level: usize,
        force: bool,
        moving: &dyn Fn(PhysAddr) -> bool,
    ) -> Result<PhysAddr, MapError> {
        let target = if force || moving(table) {
            let frame = alloc_frame_zeroed().ok_or(MapError::OutOfMemory)?;
            table_at(frame).entries = table_at(table).entries;
            frame
        } else {
            table
        };
        if level < 3 {
            for i in 0..512 {
                let entry = table_at(target).entries[i];
                if !entry.is_present() || (level > 0 && entry.is_huge()) {
                    continue;
                }
                let child = copy(entry.addr(), level + 1, false, moving)?;
                if child != entry.addr() {
                    table_at(target).entries[i] = Entry::new(child, entry.flags());
                }
            }
        }
        Ok(target)
    }
    without_interrupts(|| {
        let _guard = PAGING_LOCK.lock();
        copy(kernel_root(), 0, true, moving)
    })
}

/// TLB shootdown: every CPU that currently runs in the address space `root`
/// reloads CR3, dropping stale entries for pages unmapped there (needed when
/// threads of one process run on several CPUs). Blocks until all are done;
/// call with interrupts enabled.
pub fn shoot_down(root: PhysAddr) {
    crate::arch::x86_64::smp::shoot_down_tlb(root);
}

/// Makes `root` the kernel's PML4 (it must map exactly what the old one maps).
pub fn set_kernel_root(root: PhysAddr) {
    KERNEL_ROOT.store(root.0, Ordering::Relaxed);
}

/// Copies the kernel half of the kernel PML4 into the (zeroed) table at `root`.
pub fn share_kernel_half(root: PhysAddr) {
    let src = table_at(kernel_root());
    let dst = table_at(root);
    dst.entries[KERNEL_PML4_START..].copy_from_slice(&src.entries[KERNEL_PML4_START..]);
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

/// Returns the physical address `virt` is mapped to in the active address
/// space (handles huge pages).
pub fn translate(virt: VirtAddr) -> Option<PhysAddr> {
    translate_in(read_cr3(), virt)
}

/// Like `translate`, in the address space whose PML4 is at `root`.
pub fn translate_in(root: PhysAddr, virt: VirtAddr) -> Option<PhysAddr> {
    let idx = table_indices(virt);
    let mut table = table_at(root);
    for (level, &i) in idx.iter().enumerate() {
        let entry = table.entries[i];
        if !entry.is_present() {
            return None;
        }
        // Level 1 (PDPT) huge = 1 GiB, level 2 (PD) huge = 2 MiB
        if entry.is_huge() && (level == 1 || level == 2) {
            let page_size = if level == 1 { 1 << 30 } else { 1 << 21 };
            // bit 12 of a huge page entry is PAT (caching type), not an address bit (bug #26)
            let start = entry.addr().0 & !(page_size - 1);
            return Some(PhysAddr(start + (virt.0 & (page_size - 1))));
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
    page_flags_in(read_cr3(), virt)
}

/// Like `page_flags`, in the address space whose PML4 is at `root`.
pub fn page_flags_in(root: PhysAddr, virt: VirtAddr) -> Option<Flags> {
    let idx = table_indices(virt);
    let mut table = table_at(root);
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

/// Maps the 4 KiB page at `virt` to the frame `phys` in the active address space.
pub fn map_page(virt: VirtAddr, phys: PhysAddr, flags: Flags) -> Result<(), MapError> {
    map_page_in(read_cr3(), virt, phys, flags)
}

/// Like `map_page`, in the address space whose PML4 is at `root`.
pub fn map_page_in(root: PhysAddr, virt: VirtAddr, phys: PhysAddr, flags: Flags) -> Result<(), MapError> {
    if virt.0 % PAGE_SIZE != 0 || !phys.is_page_aligned() {
        return Err(MapError::NotAligned);
    }
    without_interrupts(|| {
        let _guard = PAGING_LOCK.lock();
        let idx = table_indices(virt);
        let mut table = table_at(root);
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
    unmap_page_in(read_cr3(), virt)
}

/// Like `unmap_page`, in the address space whose PML4 is at `root`.
pub fn unmap_page_in(root: PhysAddr, virt: VirtAddr) -> Result<PhysAddr, UnmapError> {
    if virt.0 % PAGE_SIZE != 0 {
        return Err(UnmapError::NotAligned);
    }
    without_interrupts(|| {
        let _guard = PAGING_LOCK.lock();
        let idx = table_indices(virt);
        let mut table = table_at(root);
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

/// Frees the lower (user) half of the address space at `root`: every mapped
/// frame and every page table, then the PML4 itself. All user mappings own
/// their frames. `root` must not be active and must not be the kernel's.
pub fn free_user_half(root: PhysAddr) {
    assert!(
        root != read_cr3() && root != kernel_root(),
        "freeing an active address space"
    );
    fn free_table(table: PhysAddr, level: usize) {
        for &entry in table_at(table).entries.iter() {
            if !entry.is_present() {
                continue;
            }
            if level < 3 {
                assert!(!entry.is_huge(), "huge page in a user address space");
                free_table(entry.addr(), level + 1);
            } else {
                free_frame(entry.addr());
            }
        }
        free_frame(table);
    }
    for entry in &table_at(root).entries[..KERNEL_PML4_START] {
        if entry.is_present() {
            free_table(entry.addr(), 1);
        }
    }
    free_frame(root);
}

/// Allocates fresh zeroed frames and maps `pages` pages starting at `virt`.
/// On failure, everything mapped so far is rolled back.
pub fn map_fresh(virt: VirtAddr, pages: usize, flags: Flags) -> Result<(), MapError> {
    for i in 0..pages {
        let page = VirtAddr(virt.0 + i as u64 * PAGE_SIZE);
        let result = alloc_frame_zeroed()
            .ok_or(MapError::OutOfMemory)
            .and_then(|frame| map_page(page, frame, flags).inspect_err(|_| free_frame(frame)));
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

/// Makes sure the physical range `[phys, phys + size)` is reachable through
/// the HHDM and returns its virtual address there. Limine's HHDM only covers
/// RAM and the framebuffer; ACPI tables and device registers (MMIO) may be
/// missing, so absent pages are mapped with `flags` (use `Flags::MMIO` for devices).
pub fn ensure_hhdm_mapped(phys: PhysAddr, size: u64, flags: Flags) -> Result<VirtAddr, MapError> {
    let start = super::align_down(phys.0, PAGE_SIZE);
    let end = super::align_up(phys.0 + size.max(1), PAGE_SIZE);
    let mut page = start;
    while page < end {
        let virt = PhysAddr(page).to_virt();
        if translate(virt).is_none() {
            map_page(virt, PhysAddr(page), flags)?;
        }
        page += PAGE_SIZE;
    }
    Ok(phys.to_virt())
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
    fn kernel_half_is_fully_populated() {
        let pml4 = table_at(kernel_root());
        assert!(pml4.entries[KERNEL_PML4_START..].iter().all(|e| e.is_present()));
        assert!(
            pml4.entries[KERNEL_PML4_START..]
                .iter()
                .all(|e| !e.flags().contains(Flags::USER))
        );
    }

    #[test_case]
    fn indices_split_address() {
        let v = VirtAddr(0xFFFF_FFFF_8000_1234);
        assert_eq!(table_indices(v), [511, 510, 0, 1]);
    }

    #[test_case]
    fn translate_ignores_the_pat_bit_of_huge_pages() {
        // Limine maps the framebuffer with 2 MiB pages that have PAT (bit 12) set (bug #26)
        let fb = crate::boot::FRAMEBUFFER.response().unwrap().framebuffers().first().unwrap();
        let virt = fb.address() as u64;
        assert_eq!(translate(VirtAddr(virt)), Some(PhysAddr(virt - hhdm_offset())));
    }

    #[test_case]
    fn translate_hhdm_and_kernel() {
        assert_eq!(
            translate(VirtAddr(hhdm_offset() + 0x10_0000)),
            Some(PhysAddr(0x10_0000))
        );
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
        let lost = crate::mem::pmm::frames_lost(|| {
            map_fresh(virt, 8, Flags::KERNEL_DATA).unwrap();
            for i in 0..8u64 {
                let p = VirtAddr(virt.0 + i * PAGE_SIZE).as_mut_ptr::<u64>();
                unsafe { assert_eq!(p.read_volatile(), 0) }; // zeroed
                unsafe { p.write_volatile(i) };
            }
            unmap_and_free(virt, 8);
        });
        assert_eq!(lost, 0, "unmap_and_free did not give the frames back");
    }

    #[test_case]
    fn ensure_hhdm_mapped_maps_missing_pages() {
        // 0xFEC0_0000 (I/O APIC) is MMIO and not part of Limine's HHDM
        let phys = PhysAddr(0xFEC0_0000);
        let virt = ensure_hhdm_mapped(phys, 0x20, Flags::MMIO).unwrap();
        assert_eq!(virt, phys.to_virt());
        assert_eq!(translate(virt), Some(phys));
        // already mapped RAM is left alone
        assert!(ensure_hhdm_mapped(PhysAddr(0x10_0000), 4096, Flags::KERNEL_DATA).is_ok());
    }

    #[test_case]
    fn misaligned_requests_rejected() {
        assert_eq!(
            map_page(VirtAddr(TEST_AREA + 1), PhysAddr(0x1000), Flags::KERNEL_DATA),
            Err(MapError::NotAligned)
        );
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
