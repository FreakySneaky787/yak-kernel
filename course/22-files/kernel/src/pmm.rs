//! The physical memory manager: hands out RAM in frames of 4 KiB.
//!
//! One bit per frame - 1 = used, 0 = free - in a bitmap that covers physical memory up to the end of the highest
//! usable region. The bitmap needs a home before any allocator exists, so it takes the start of the first usable
//! region that is big enough, and marks its own frames as used.
//!
//! `FrameBitmap` itself knows nothing about Limine: it gets a slice for its bits and a list of usable regions, so
//! the tests can give it a fake machine.

use crate::mem::{PAGE_SIZE, PhysAddr, align_down, align_up};
use limine::memmap::MEMMAP_USABLE;
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
    /// The frame was free already: freed twice.
    NotAllocated,
}

pub struct FrameBitmap<'a> {
    bits: &'a mut [u64],
    frame_count: usize,
    free_frames: usize,
    /// The word where the next search starts: the ones before it are full.
    next_word: usize,
}

impl<'a> FrameBitmap<'a> {
    /// How many `u64` a bitmap for `frame_count` frames needs.
    pub const fn words_needed(frame_count: usize) -> usize {
        frame_count.div_ceil(64)
    }

    /// A bitmap in `bits` in which every frame is used, except those that lie *wholly* inside one of `usable`.
    /// Frame 0 stays used anyway, so that the physical address 0 is never handed out.
    pub fn new(bits: &'a mut [u64], frame_count: usize, usable: &[Region]) -> FrameBitmap<'a> {
        assert!(bits.len() >= Self::words_needed(frame_count), "bitmap too small");
        bits.fill(u64::MAX);
        let mut bitmap = FrameBitmap {
            bits,
            frame_count,
            free_frames: 0,
            next_word: 0,
        };
        for region in usable {
            // a frame half inside the region is not ours: round the start up and the end down
            let start = align_up(region.base, PAGE_SIZE) / PAGE_SIZE;
            let end = align_down(region.base + region.length, PAGE_SIZE) / PAGE_SIZE;
            for frame in start.max(1)..end.min(frame_count as u64) {
                if bitmap.is_used(frame as usize) {
                    bitmap.set_free(frame as usize);
                    bitmap.free_frames += 1;
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

    /// Marks the frames that touch `base..base + length` as used - for memory that is taken already.
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

    /// The lowest free frame, marked as used.
    pub fn alloc(&mut self) -> Option<PhysAddr> {
        for word in self.next_word..Self::words_needed(self.frame_count) {
            // a word with a 0 bit has a free frame: 64 frames checked at once
            if self.bits[word] == u64::MAX {
                continue;
            }
            let frame = word * 64 + self.bits[word].trailing_ones() as usize;
            if frame >= self.frame_count {
                break; // the bits after the last frame
            }
            self.set_used(frame);
            self.free_frames -= 1;
            self.next_word = word;
            return Some(PhysAddr(frame as u64 * PAGE_SIZE));
        }
        None
    }

    pub fn free(&mut self, frame: PhysAddr) -> Result<(), FrameError> {
        if !frame.is_page_aligned() {
            return Err(FrameError::NotAligned);
        }
        let index = (frame.0 / PAGE_SIZE) as usize;
        if index == 0 || index >= self.frame_count {
            return Err(FrameError::OutOfRange);
        }
        if !self.is_used(index) {
            return Err(FrameError::NotAllocated);
        }
        self.set_free(index);
        self.free_frames += 1;
        self.next_word = self.next_word.min(index / 64);
        Ok(())
    }

    pub fn free_frames(&self) -> usize {
        self.free_frames
    }
}

/// The frame allocator of the kernel. No interrupt handler allocates, so - unlike the keyboard's buffer - its lock
/// may be taken with interrupts on.
static PMM: Mutex<Option<FrameBitmap<'static>>> = Mutex::new(None);

/// Where the bitmap lives (physical start, bytes) and how many frames were free at the start - for `kmain`.
#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub bitmap: Region,
    pub usable_frames: usize,
}

static INFO: Mutex<Option<Info>> = Mutex::new(None);

/// The usable regions of Limine's memory map, and the end of the highest one.
fn usable_regions() -> ([Region; 32], usize, u64) {
    let memmap = crate::MEMMAP.response().expect("Limine gave no memory map");
    let mut usable = [Region { base: 0, length: 0 }; 32];
    let (mut count, mut highest) = (0, 0);
    for entry in memmap.entries().iter().filter(|e| e.type_ == MEMMAP_USABLE) {
        if count < usable.len() {
            usable[count] = Region {
                base: entry.base,
                length: entry.length,
            };
            count += 1;
        }
        highest = highest.max(entry.base + entry.length);
    }
    (usable, count, highest)
}

/// Builds the frame allocator from Limine's memory map. Needs the HHDM offset (`mem::init` calls it).
pub fn init() {
    let (usable, count, highest) = usable_regions();
    let usable = &usable[..count];
    let frame_count = (align_up(highest, PAGE_SIZE) / PAGE_SIZE) as usize;
    let bitmap_bytes = align_up(FrameBitmap::words_needed(frame_count) as u64 * 8, PAGE_SIZE);

    // the bitmap's home: the start of the first usable region it fits into (not frame 0)
    let home = usable
        .iter()
        .map(|r| (align_up(r.base.max(PAGE_SIZE), PAGE_SIZE), r.base + r.length))
        .find(|&(start, end)| start + bitmap_bytes <= end)
        .expect("no usable region is big enough for the frame bitmap");
    let bitmap = PhysAddr(home.0);
    let words = (bitmap_bytes / 8) as usize;
    // Safety: usable RAM that nobody uses yet, reached through the HHDM; from now on only the bitmap uses it.
    let bits = unsafe { core::slice::from_raw_parts_mut(bitmap.as_mut_ptr::<u64>(), words) };

    let mut frames = FrameBitmap::new(bits, frame_count, usable);
    let usable_frames = frames.free_frames();
    frames.reserve(bitmap.0, bitmap_bytes); // or the first frames handed out would overwrite the bitmap itself
    *PMM.lock() = Some(frames);
    *INFO.lock() = Some(Info {
        bitmap: Region {
            base: bitmap.0,
            length: bitmap_bytes,
        },
        usable_frames,
    });
}

pub fn info() -> Info {
    INFO.lock().expect("pmm::init ran")
}

/// One frame of 4 KiB - its contents are whatever was there before.
pub fn alloc_frame() -> Option<PhysAddr> {
    PMM.lock().as_mut().expect("pmm::init ran").alloc()
}

/// One frame, filled with zeros.
pub fn alloc_frame_zeroed() -> Option<PhysAddr> {
    let frame = alloc_frame()?;
    unsafe { core::ptr::write_bytes(frame.as_mut_ptr::<u8>(), 0, PAGE_SIZE as usize) };
    Some(frame)
}

/// Gives a frame back. Freeing a frame twice, or one that never was one, is always a bug in the kernel: panic.
pub fn free_frame(frame: PhysAddr) {
    if let Err(e) = PMM.lock().as_mut().expect("pmm::init ran").free(frame) {
        panic!("free_frame({:#x}): {:?}", frame.0, e);
    }
}

pub fn free_frames() -> usize {
    PMM.lock().as_ref().expect("pmm::init ran").free_frames()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake machine of 256 frames (1 MiB) with two usable regions.
    fn fake(bits: &mut [u64; 4]) -> FrameBitmap<'_> {
        let usable = [
            Region {
                base: 0x0000,
                length: 0x4000,
            }, // frames 0-3, but frame 0 stays used
            Region {
                base: 0x10800,
                length: 0x3000,
            }, // not aligned: only frames 0x11 and 0x12 lie wholly inside
        ];
        FrameBitmap::new(bits, 256, &usable)
    }

    #[test_case]
    fn only_whole_frames_are_free() {
        let mut bits = [0; 4];
        assert_eq!(fake(&mut bits).free_frames(), 3 + 2);
    }

    #[test_case]
    fn the_lowest_frame_comes_first_until_none_is_left() {
        let mut bits = [0; 4];
        let mut frames = fake(&mut bits);
        let mut got = [0; 5];
        for slot in got.iter_mut() {
            *slot = frames.alloc().unwrap().0;
        }
        assert_eq!(got, [0x1000, 0x2000, 0x3000, 0x11000, 0x12000]);
        assert_eq!(frames.alloc(), None);
    }

    #[test_case]
    fn freed_frames_come_back_and_nonsense_is_refused() {
        let mut bits = [0; 4];
        let mut frames = fake(&mut bits);
        let (a, b) = (frames.alloc().unwrap(), frames.alloc().unwrap());
        assert_eq!(frames.free(a), Ok(()));
        assert_eq!(frames.alloc(), Some(a)); // the lowest free frame again
        assert_eq!(frames.free(b), Ok(()));
        assert_eq!(frames.free(b), Err(FrameError::NotAllocated));
        assert_eq!(frames.free(PhysAddr(0x1234)), Err(FrameError::NotAligned));
        assert_eq!(frames.free(PhysAddr(0)), Err(FrameError::OutOfRange));
        assert_eq!(frames.free(PhysAddr(0x100_0000)), Err(FrameError::OutOfRange));
    }

    #[test_case]
    fn reserved_frames_are_not_handed_out() {
        let mut bits = [0; 4];
        let mut frames = fake(&mut bits);
        frames.reserve(0x1000, 0x2000);
        assert_eq!(frames.free_frames(), 3);
        assert_eq!(frames.alloc(), Some(PhysAddr(0x3000)));
    }

    #[test_case]
    fn most_of_the_ram_is_usable() {
        // run-qemu.sh gives the machine 128 MiB; firmware, Limine and the kernel take a few
        let mib = info().usable_frames as u64 * PAGE_SIZE / (1024 * 1024);
        assert!((100..128).contains(&mib), "{} MiB usable", mib);
    }

    #[test_case]
    fn every_frame_is_usable_ram_and_none_is_the_bitmap() {
        let before = free_frames();
        let bitmap = info().bitmap;
        let (usable, count, _) = usable_regions();
        // take every frame there is; each one stores the address of the one before - a chain through the frames
        let mut last = PhysAddr(0);
        let mut taken = 0;
        while let Some(frame) = alloc_frame() {
            let inside = |r: &Region| frame.0 >= r.base && frame.0 + PAGE_SIZE <= r.base + r.length;
            assert!(usable[..count].iter().any(inside), "{:#x} is not usable RAM", frame.0);
            assert!(!inside(&bitmap), "{:#x} belongs to the bitmap", frame.0);
            unsafe { frame.as_mut_ptr::<u64>().write(last.0) };
            (last, taken) = (frame, taken + 1);
        }
        assert_eq!(taken, before);
        // walk the chain back: every frame still holds what we wrote, and goes back to the allocator
        while last != PhysAddr(0) {
            let previous = unsafe { last.as_mut_ptr::<u64>().read() };
            free_frame(last);
            last = PhysAddr(previous);
        }
        assert_eq!(free_frames(), before);
    }

    #[test_case]
    fn a_zeroed_frame_is_zero() {
        let frame = alloc_frame().unwrap();
        unsafe { core::ptr::write_bytes(frame.as_mut_ptr::<u8>(), 0xAB, PAGE_SIZE as usize) };
        free_frame(frame);
        let again = alloc_frame_zeroed().unwrap();
        assert_eq!(again, frame); // the same frame: the lowest free one
        let words = unsafe { core::slice::from_raw_parts(again.as_mut_ptr::<u64>(), 512) };
        assert!(words.iter().all(|&w| w == 0));
        free_frame(again);
    }
}
