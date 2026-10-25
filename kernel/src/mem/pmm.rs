//! Physical memory manager: hands out 4 KiB frames of physical RAM.
//!
//! One bit per frame (1 = used, 0 = free) in a bitmap that covers physical
//! memory up to the end of the highest usable region. The bitmap itself is
//! stored in the first usable region that is large enough and is accessed
//! through the HHDM.
//!
//! The bitmap logic (`FrameBitmap`) does not know about Limine; it gets a
//! buffer and a list of usable regions, so it can be unit-tested with fake
//! memory maps.

use super::{PAGE_SIZE, PhysAddr, align_down, align_up};
use crate::arch::x86_64::without_interrupts;
use core::sync::atomic::{AtomicU64, Ordering};
use limine::memmap::{MEMMAP_BOOTLOADER_RECLAIMABLE, MEMMAP_USABLE};
use spin::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub base: u64,
    pub length: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    NotAligned,
    OutOfRange,
    /// The frame was already free (double free).
    NotAllocated,
}

pub struct FrameBitmap {
    bits: &'static mut [u64],
    frame_count: usize,
    free_frames: usize,
    usable_frames: usize,
    /// Word index where the next search starts (speeds up sequential allocations).
    next_word: usize,
}

impl FrameBitmap {
    /// Number of `u64` words needed for a bitmap covering `frame_count` frames.
    pub const fn words_needed(frame_count: usize) -> usize {
        frame_count.div_ceil(64)
    }

    /// Builds a bitmap in `bits`: everything starts as used, then all frames
    /// fully inside `usable` regions are marked free. Frame 0 always stays
    /// reserved so that physical address 0 is never handed out.
    pub fn new(bits: &'static mut [u64], frame_count: usize, usable: &[Region]) -> FrameBitmap {
        assert!(bits.len() >= Self::words_needed(frame_count), "bitmap buffer too small");
        bits.fill(u64::MAX);
        let mut bitmap = FrameBitmap {
            bits,
            frame_count,
            free_frames: 0,
            usable_frames: 0,
            next_word: 0,
        };
        for region in usable {
            let start = align_up(region.base, PAGE_SIZE) / PAGE_SIZE;
            let end = align_down(region.base + region.length, PAGE_SIZE) / PAGE_SIZE;
            for frame in start..end.min(frame_count as u64) {
                if frame != 0 && bitmap.is_used(frame as usize) {
                    bitmap.set_free(frame as usize);
                    bitmap.free_frames += 1;
                    bitmap.usable_frames += 1;
                }
            }
        }
        bitmap
    }

    fn is_used(&self, frame: usize) -> bool {
        self.bits[frame / 64] & (1 << (frame % 64)) != 0
    }

    fn set_used(&mut self, frame: usize) {
        self.bits[frame / 64] |= 1 << (frame % 64);
    }

    fn set_free(&mut self, frame: usize) {
        self.bits[frame / 64] &= !(1 << (frame % 64));
    }

    /// Marks a range as used (e.g. the frames holding the bitmap itself).
    pub fn reserve(&mut self, base: u64, length: u64) {
        let start = align_down(base, PAGE_SIZE) / PAGE_SIZE;
        let end = align_up(base + length, PAGE_SIZE) / PAGE_SIZE;
        for frame in start..end.min(self.frame_count as u64) {
            if !self.is_used(frame as usize) {
                self.set_used(frame as usize);
                self.free_frames -= 1;
            }
        }
    }

    pub fn alloc(&mut self) -> Option<PhysAddr> {
        let words = Self::words_needed(self.frame_count);
        for i in 0..words {
            let word_index = (self.next_word + i) % words;
            let word = self.bits[word_index];
            if word == u64::MAX {
                continue;
            }
            let frame = word_index * 64 + word.trailing_ones() as usize;
            if frame >= self.frame_count {
                continue; // padding bits in the last word
            }
            self.set_used(frame);
            self.free_frames -= 1;
            self.next_word = word_index;
            return Some(PhysAddr(frame as u64 * PAGE_SIZE));
        }
        None
    }

    /// Takes the lowest free frame below `limit` (for hardware that needs low
    /// memory, like the real-mode start code of other CPUs).
    pub fn alloc_below(&mut self, limit: u64) -> Option<PhysAddr> {
        let end = ((limit / PAGE_SIZE) as usize).min(self.frame_count);
        let frame = (1..end).find(|&f| !self.is_used(f))?;
        self.set_used(frame);
        self.free_frames -= 1;
        Some(PhysAddr(frame as u64 * PAGE_SIZE))
    }

    pub fn free(&mut self, addr: PhysAddr) -> Result<(), FrameError> {
        if !addr.is_page_aligned() {
            return Err(FrameError::NotAligned);
        }
        let frame = (addr.0 / PAGE_SIZE) as usize;
        if frame == 0 || frame >= self.frame_count {
            return Err(FrameError::OutOfRange);
        }
        if !self.is_used(frame) {
            return Err(FrameError::NotAllocated);
        }
        self.set_free(frame);
        self.free_frames += 1;
        if frame / 64 < self.next_word {
            self.next_word = frame / 64;
        }
        Ok(())
    }

    pub fn free_frames(&self) -> usize {
        self.free_frames
    }

    /// Marks every used frame fully inside `region` as free and usable
    /// (memory that becomes available later, e.g. the bootloader's).
    /// Returns how many frames were added.
    pub fn release_region(&mut self, region: Region) -> usize {
        let start = align_up(region.base, PAGE_SIZE) / PAGE_SIZE;
        let end = align_down(region.base + region.length, PAGE_SIZE) / PAGE_SIZE;
        let mut added = 0;
        for frame in start.max(1)..end.min(self.frame_count as u64) {
            if self.is_used(frame as usize) {
                self.set_free(frame as usize);
                self.free_frames += 1;
                self.usable_frames += 1;
                added += 1;
            }
        }
        if start / 64 < self.next_word as u64 {
            self.next_word = (start / 64) as usize;
        }
        added
    }

    /// Frames that were usable at boot (including the ones holding the bitmap).
    pub fn usable_frames(&self) -> usize {
        self.usable_frames
    }
}

static PMM: Mutex<Option<FrameBitmap>> = Mutex::new(None);

/// Builds the global frame allocator from the Limine memory map.
pub(super) fn init() {
    let memmap = crate::boot::MEMMAP.response().expect("Limine provided no memory map");

    // Collect usable regions (the memory map is small; a fixed array is enough).
    let mut usable = [Region { base: 0, length: 0 }; 64];
    let mut count = 0;
    let mut highest = 0;
    let mut copy = BOOT_MEMMAP.lock();
    for entry in memmap.entries() {
        if copy.1 < copy.0.len() {
            let i = copy.1;
            copy.0[i] = (entry.base, entry.length, entry.type_);
            copy.1 += 1;
        }
        // the bitmap also covers bootloader memory, so it can be added later
        if entry.type_ == MEMMAP_USABLE || entry.type_ == MEMMAP_BOOTLOADER_RECLAIMABLE {
            highest = highest.max(entry.base + entry.length);
        }
        if entry.type_ == MEMMAP_USABLE && count < usable.len() {
            usable[count] = Region {
                base: entry.base,
                length: entry.length,
            };
            count += 1;
        }
    }
    drop(copy);
    let usable = &usable[..count];
    let frame_count = (align_up(highest, PAGE_SIZE) / PAGE_SIZE) as usize;
    let bitmap_bytes = align_up(FrameBitmap::words_needed(frame_count) as u64 * 8, PAGE_SIZE);

    // Put the bitmap into the first usable region that can hold it.
    let home = usable
        .iter()
        .find(|r| align_up(r.base, PAGE_SIZE) + bitmap_bytes <= r.base + r.length && r.base >= PAGE_SIZE)
        .expect("no usable region large enough for the frame bitmap");
    let bitmap_phys = PhysAddr(align_up(home.base, PAGE_SIZE));
    let words = (bitmap_bytes / 8) as usize;
    let bits = unsafe { core::slice::from_raw_parts_mut(bitmap_phys.to_virt().as_mut_ptr::<u64>(), words) };

    let mut bitmap = FrameBitmap::new(bits, frame_count, usable);
    bitmap.reserve(bitmap_phys.0, bitmap_bytes);
    // Before anything else takes them: one frame below 1 MiB for starting the other CPUs
    if let Some(frame) = bitmap.alloc_below(0x10_0000) {
        LOW_FRAME.store(frame.0, Ordering::Relaxed);
    }
    *PMM.lock() = Some(bitmap);
}

/// For leak tests on a running SMP system: runs `f` (which must give back
/// every frame it takes) until the free-frame count afterwards equals the
/// count before, at most three times. Other threads may allocate in the
/// meantime (heap growth, new stacks), but not three times in a row; a real
/// leak shows up every time. Returns the frames missing in the last attempt.
#[cfg(test)]
pub fn frames_lost(f: impl Fn()) -> usize {
    let mut lost = 0;
    for _ in 0..3 {
        let before = stats().free_frames;
        f();
        lost = before.saturating_sub(stats().free_frames);
        if lost == 0 {
            break;
        }
    }
    lost
}

/// A copy of the bootloader's memory map (base, length, type), taken at
/// boot: the original lives in bootloader memory, which may be reclaimed.
static BOOT_MEMMAP: Mutex<([(u64, u64, u64); 64], usize)> = Mutex::new(([(0, 0, 0); 64], 0));

pub fn boot_memmap() -> alloc::vec::Vec<(u64, u64, u64)> {
    let copy = BOOT_MEMMAP.lock();
    copy.0[..copy.1].to_vec()
}

/// The regions the bootloader may still use until we reclaim them.
pub fn bootloader_regions() -> alloc::vec::Vec<Region> {
    boot_memmap()
        .into_iter()
        .filter(|&(_, _, t)| t == MEMMAP_BOOTLOADER_RECLAIMABLE)
        .map(|(base, length, _)| Region { base, length })
        .collect()
}

/// Hands `region` to the allocator; returns the number of frames added.
pub fn release_region(region: Region) -> usize {
    with_pmm(|p| p.release_region(region))
}

/// The frame below 1 MiB reserved at boot (for the SMP trampoline), if any.
static LOW_FRAME: AtomicU64 = AtomicU64::new(0);

pub fn low_frame() -> Option<PhysAddr> {
    match LOW_FRAME.load(Ordering::Relaxed) {
        0 => None,
        f => Some(PhysAddr(f)),
    }
}

fn with_pmm<R>(f: impl FnOnce(&mut FrameBitmap) -> R) -> R {
    without_interrupts(|| f(PMM.lock().as_mut().expect("frame allocator not initialized")))
}

/// Allocates one physical 4 KiB frame (contents undefined).
pub fn alloc_frame() -> Option<PhysAddr> {
    with_pmm(|p| p.alloc())
}

/// Allocates one physical frame and fills it with zeros.
pub fn alloc_frame_zeroed() -> Option<PhysAddr> {
    let frame = alloc_frame()?;
    unsafe { core::ptr::write_bytes(frame.to_virt().as_mut_ptr::<u8>(), 0, PAGE_SIZE as usize) };
    Some(frame)
}

/// Returns a frame to the allocator. Panics on double free or invalid address,
/// because that always means a bug somewhere in the kernel.
pub fn free_frame(frame: PhysAddr) {
    if let Err(e) = with_pmm(|p| p.free(frame)) {
        panic!("free_frame({:?}) failed: {:?}", frame, e);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub usable_frames: usize,
    pub free_frames: usize,
}

impl Stats {
    pub fn usable_bytes(&self) -> u64 {
        self.usable_frames as u64 * PAGE_SIZE
    }

    pub fn free_bytes(&self) -> u64 {
        self.free_frames as u64 * PAGE_SIZE
    }
}

pub fn stats() -> Stats {
    with_pmm(|p| Stats {
        usable_frames: p.usable_frames(),
        free_frames: p.free_frames(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bitmap for a fake machine with 256 frames (1 MiB) and two usable regions.
    fn fake_bitmap(storage: &'static mut [u64; 4]) -> FrameBitmap {
        let regions = [
            Region {
                base: 0x0000,
                length: 0x4000,
            }, // frames 0..4 (frame 0 stays reserved)
            Region {
                base: 0x10800,
                length: 0x3000,
            }, // unaligned: only frames 0x11, 0x12 fully inside
        ];
        FrameBitmap::new(storage, 256, &regions)
    }

    #[test_case]
    fn fake_map_counts_only_full_frames() {
        static mut S: [u64; 4] = [0; 4];
        let bm = fake_bitmap(unsafe { &mut *core::ptr::addr_of_mut!(S) });
        assert_eq!(bm.free_frames(), 3 + 2);
    }

    #[test_case]
    fn fake_map_alloc_until_exhausted() {
        static mut S: [u64; 4] = [0; 4];
        let mut bm = fake_bitmap(unsafe { &mut *core::ptr::addr_of_mut!(S) });
        let mut got = [0u64; 5];
        for slot in got.iter_mut() {
            *slot = bm.alloc().unwrap().0;
        }
        assert_eq!(got, [0x1000, 0x2000, 0x3000, 0x11000, 0x12000]);
        assert_eq!(bm.alloc(), None);
        assert_eq!(bm.free_frames(), 0);
    }

    #[test_case]
    fn fake_map_free_and_reuse() {
        static mut S: [u64; 4] = [0; 4];
        let mut bm = fake_bitmap(unsafe { &mut *core::ptr::addr_of_mut!(S) });
        let a = bm.alloc().unwrap();
        let b = bm.alloc().unwrap();
        assert_eq!(bm.free(a), Ok(()));
        assert_eq!(bm.alloc(), Some(a)); // lowest free frame is reused
        assert_eq!(bm.free(b), Ok(()));
        assert_eq!(bm.free(b), Err(FrameError::NotAllocated));
        assert_eq!(bm.free(PhysAddr(0x1234)), Err(FrameError::NotAligned));
        assert_eq!(bm.free(PhysAddr(0)), Err(FrameError::OutOfRange));
        assert_eq!(bm.free(PhysAddr(0x100_0000)), Err(FrameError::OutOfRange));
    }

    #[test_case]
    fn fake_map_reserve() {
        static mut S: [u64; 4] = [0; 4];
        let mut bm = fake_bitmap(unsafe { &mut *core::ptr::addr_of_mut!(S) });
        bm.reserve(0x1000, 0x2000);
        assert_eq!(bm.free_frames(), 3);
        assert_eq!(bm.alloc(), Some(PhysAddr(0x3000)));
    }

    #[test_case]
    fn real_pmm_has_memory() {
        let s = stats();
        // QEMU runs with -m 256M; most of it must be usable
        assert!(
            s.usable_bytes() > 200 * 1024 * 1024,
            "only {} bytes usable",
            s.usable_bytes()
        );
        assert!(s.free_frames <= s.usable_frames);
    }

    #[test_case]
    fn real_frames_are_distinct_aligned_and_writable() {
        const N: usize = 64;
        let lost = frames_lost(|| frames_round_trip::<N>());
        assert_eq!(lost, 0, "frames were not given back");
    }

    fn frames_round_trip<const N: usize>() {
        let mut frames = [PhysAddr(0); N];
        for (i, slot) in frames.iter_mut().enumerate() {
            let f = alloc_frame_zeroed().expect("out of memory");
            assert!(f.is_page_aligned());
            // write a pattern through the HHDM
            unsafe { f.to_virt().as_mut_ptr::<u64>().write_volatile(0xC0FFEE00 + i as u64) };
            *slot = f;
        }
        for i in 0..N {
            for j in i + 1..N {
                assert_ne!(frames[i], frames[j], "frame handed out twice");
            }
            let v = unsafe { frames[i].to_virt().as_ptr::<u64>().read_volatile() };
            assert_eq!(v, 0xC0FFEE00 + i as u64);
        }
        for f in frames {
            free_frame(f);
        }
    }

    #[test_case]
    fn zeroed_frame_is_zero() {
        let f = alloc_frame().unwrap();
        unsafe { core::ptr::write_bytes(f.to_virt().as_mut_ptr::<u8>(), 0xAB, PAGE_SIZE as usize) };
        free_frame(f);
        let g = alloc_frame_zeroed().unwrap();
        let words = unsafe { core::slice::from_raw_parts(g.to_virt().as_ptr::<u64>(), 512) };
        assert!(words.iter().all(|&w| w == 0));
        free_frame(g);
    }
}
