//! Programs as files: the ELF format (chapter 21).
//!
//! A compiler and a linker turn a program into an *ELF file* (Executable and Linkable Format, the format of
//! Linux, the BSDs and most others). For loading we need only a small part of it: the file starts with a header
//! (what kind of file, which machine, where the program starts), followed by a table of *program headers*. Each
//! program header of type LOAD says: "the `filesz` bytes at `offset` in this file belong at the virtual address
//! `vaddr`, in a region of `memsz` bytes - the bytes beyond `filesz` are zeros (`.bss`) - and the region may be
//! read, written and/or executed". Loading is mapping those regions and copying the bytes. (Section headers, symbols
//! and debug information exist for tools; the loader ignores them.)
//!
//! We load only what the programs of this course are: statically linked, 64-bit, for x86-64, at fixed addresses.

use crate::address_space::AddressSpace;
use crate::mem::{PAGE_SIZE, align_down, align_up};
use crate::paging::{Flags, MapError};
use alloc::vec::Vec;

/// A program may live between these addresses: not in the first 64 KiB (a null pointer must fault), and below the
/// stack and the rest of the lower half.
pub const IMAGE_START: u64 = 0x1_0000;
pub const IMAGE_END: u64 = 0x7000_0000_0000;

const FLAG_X: u32 = 1;
const FLAG_W: u32 = 2;

/// Why a file is no program we can load.
#[derive(Debug, PartialEq, Eq)]
pub enum ElfError {
    /// Shorter than the ELF header.
    TooShort,
    /// Does not start with 0x7F "ELF", or is not 64-bit little endian.
    BadMagic,
    /// Not for x86-64.
    WrongArchitecture,
    /// Not a fixed-address executable (a shared library or a relocatable object, say).
    NotExecutable,
    /// The program header table does not fit in the file or has the wrong entry size, or a segment is larger on
    /// disk than in memory.
    BadProgramHeaders,
    /// Nothing to load.
    NoLoadableSegment,
    /// The bytes of a segment are not all in the file.
    SegmentOutsideFile,
    /// A segment would be loaded outside `IMAGE_START..IMAGE_END` - into the kernel's half, say.
    SegmentOutsideUserSpace,
    /// A segment that may be written *and* executed: code that can change itself is what an attacker wants.
    WritableAndExecutable,
    /// The entry point is not in a segment that may be executed.
    BadEntryPoint,
}

/// One LOAD entry of the program header table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    /// Where its bytes start in the file, and how many there are.
    pub offset: u64,
    pub filesz: u64,
    /// The size in memory: `filesz` bytes from the file, the rest zeros.
    pub memsz: u64,
    flags: u32,
}

impl Segment {
    /// What the pages of this segment may do: readable always, writable if the segment says W, executable if X.
    fn page_flags(&self) -> Flags {
        match (self.flags & FLAG_W != 0, self.flags & FLAG_X != 0) {
            (false, true) => Flags::USER_CODE,
            (true, false) => Flags::USER_DATA,
            (false, false) => Flags::USER_READ_ONLY,
            (true, true) => unreachable!("`parse` rejects segments that are both"),
        }
    }
}

#[derive(Debug)]
pub struct ElfImage {
    /// The address of the first instruction.
    pub entry: u64,
    pub segments: Vec<Segment>,
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(data[at..at + 2].try_into().unwrap())
}
fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// Reads the header and the program headers of `data` and checks that this is a program we can load. Nothing is
/// trusted: every size and offset in a file is somebody's claim, and the checks use `checked_add` - a segment at
/// 0xFFFF_FFFF_FFFF_FFF0 must not wrap around into the low addresses.
pub fn parse(data: &[u8]) -> Result<ElfImage, ElfError> {
    const HEADER: usize = 64;
    const PROGRAM_HEADER: usize = 56;
    if data.len() < HEADER {
        return Err(ElfError::TooShort);
    }
    // 0x7F 'E' 'L' 'F', class 2 (64-bit), data 1 (little endian)
    if data[..4] != *b"\x7FELF" || data[4] != 2 || data[5] != 1 {
        return Err(ElfError::BadMagic);
    }
    if u16_at(data, 18) != 0x3E {
        return Err(ElfError::WrongArchitecture);
    }
    if u16_at(data, 16) != 2 {
        return Err(ElfError::NotExecutable); // 2 = ET_EXEC; 3 would be a position-independent file
    }
    let entry = u64_at(data, 24);
    let table = u64_at(data, 32);
    let (entry_size, count) = (u16_at(data, 54) as usize, u16_at(data, 56) as usize);
    let table_end = (count * PROGRAM_HEADER) as u64;
    if entry_size != PROGRAM_HEADER
        || count > 64
        || table.checked_add(table_end).is_none_or(|end| end > data.len() as u64)
    {
        return Err(ElfError::BadProgramHeaders);
    }
    let mut segments = Vec::new();
    for i in 0..count {
        let at = table as usize + i * PROGRAM_HEADER;
        if u32_at(data, at) != 1 {
            continue; // not LOAD: notes, stack flags ... nothing for us to do
        }
        let segment = Segment {
            flags: u32_at(data, at + 4),
            offset: u64_at(data, at + 8),
            vaddr: u64_at(data, at + 16),
            filesz: u64_at(data, at + 32),
            memsz: u64_at(data, at + 40),
        };
        if segment.memsz == 0 {
            continue;
        }
        if segment.filesz > segment.memsz {
            return Err(ElfError::BadProgramHeaders);
        }
        if segment
            .offset
            .checked_add(segment.filesz)
            .is_none_or(|end| end > data.len() as u64)
        {
            return Err(ElfError::SegmentOutsideFile);
        }
        let in_range = segment.vaddr >= IMAGE_START
            && segment
                .vaddr
                .checked_add(segment.memsz)
                .is_some_and(|end| end <= IMAGE_END);
        if !in_range {
            return Err(ElfError::SegmentOutsideUserSpace);
        }
        if segment.flags & FLAG_W != 0 && segment.flags & FLAG_X != 0 {
            return Err(ElfError::WritableAndExecutable);
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        return Err(ElfError::NoLoadableSegment);
    }
    let entry_in_code = segments
        .iter()
        .any(|s| s.flags & FLAG_X != 0 && (s.vaddr..s.vaddr + s.memsz).contains(&entry));
    if !entry_in_code {
        return Err(ElfError::BadEntryPoint);
    }
    Ok(ElfImage { entry, segments })
}

/// Maps the pages of every segment into `space` and copies the file's bytes there. The rest of each segment stays
/// zero: a fresh frame is zeroed, and that is what `.bss` is. Two segments that share a page are an error here
/// (`AlreadyMapped`): the page could not have the flags of both.
pub fn load(space: &mut AddressSpace, data: &[u8], image: &ElfImage) -> Result<(), MapError> {
    for segment in &image.segments {
        let first = align_down(segment.vaddr, PAGE_SIZE);
        let end = align_up(segment.vaddr + segment.memsz, PAGE_SIZE);
        for page in (first..end).step_by(PAGE_SIZE as usize) {
            space.map_fresh(page, segment.page_flags())?;
        }
        let file_bytes = &data[segment.offset as usize..(segment.offset + segment.filesz) as usize];
        space.write(segment.vaddr, file_bytes)?;
    }
    Ok(())
}

/// A segment for `build_test_elf`.
#[cfg(test)]
pub struct TestSegment<'a> {
    pub vaddr: u64,
    /// 1 = X, 2 = W, 4 = R
    pub flags: u32,
    pub bytes: &'a [u8],
    pub memsz: u64,
}

/// An ELF file made of `segments`, for the tests - the same bytes a linker would write, minus everything the loader
/// ignores. Header 64 bytes, then one 56-byte program header per segment, then the segments' bytes one after the
/// other.
#[cfg(test)]
pub fn build_test_elf(entry: u64, segments: &[TestSegment]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(b"\x7FELF\x02\x01\x01\0\0\0\0\0\0\0\0\0");
    file.extend_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    file.extend_from_slice(&0x3Eu16.to_le_bytes()); // x86-64
    file.extend_from_slice(&1u32.to_le_bytes()); // version
    file.extend_from_slice(&entry.to_le_bytes());
    file.extend_from_slice(&64u64.to_le_bytes()); // program headers right after this header
    file.extend_from_slice(&0u64.to_le_bytes()); // no section headers
    file.extend_from_slice(&0u32.to_le_bytes()); // flags
    file.extend_from_slice(&64u16.to_le_bytes()); // header size
    file.extend_from_slice(&56u16.to_le_bytes()); // program header size
    file.extend_from_slice(&(segments.len() as u16).to_le_bytes());
    file.extend_from_slice(&[0; 6]); // section header size, count, names
    let mut offset = 64 + 56 * segments.len() as u64;
    for s in segments {
        file.extend_from_slice(&1u32.to_le_bytes()); // LOAD
        file.extend_from_slice(&s.flags.to_le_bytes());
        file.extend_from_slice(&offset.to_le_bytes());
        file.extend_from_slice(&s.vaddr.to_le_bytes());
        file.extend_from_slice(&s.vaddr.to_le_bytes()); // physical address: unused
        file.extend_from_slice(&(s.bytes.len() as u64).to_le_bytes());
        file.extend_from_slice(&s.memsz.to_le_bytes());
        file.extend_from_slice(&PAGE_SIZE.to_le_bytes());
        offset += s.bytes.len() as u64;
    }
    for s in segments {
        file.extend_from_slice(s.bytes);
    }
    file
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address_space::USER_END;
    use crate::pmm;

    const CODE: u64 = 0x40_0000;
    const DATA: u64 = 0x40_1000;

    /// code (R X) at 0x400000 and a page of data (R W, no bytes in the file: all `.bss`) at 0x401000
    fn valid() -> Vec<u8> {
        build_test_elf(
            CODE,
            &[
                TestSegment {
                    vaddr: CODE,
                    flags: 5,
                    bytes: b"\x90\x90\xC3",
                    memsz: 3,
                },
                TestSegment {
                    vaddr: DATA,
                    flags: 6,
                    bytes: b"",
                    memsz: 4096,
                },
            ],
        )
    }

    /// `valid()` with some bytes of the file overwritten.
    fn patched(at: usize, bytes: &[u8]) -> Vec<u8> {
        let mut file = valid();
        file[at..at + bytes.len()].copy_from_slice(bytes);
        file
    }

    // where the fields are: the header, then the first program header at 64 (type, flags, offset, vaddr, paddr,
    // filesz, memsz, align at 0, 4, 8, 16, 24, 32, 40, 48)
    const PH0: usize = 64;

    #[test_case]
    fn a_valid_file_is_understood() {
        let image = parse(&valid()).unwrap();
        assert_eq!(image.entry, CODE);
        assert_eq!(image.segments.len(), 2);
        assert_eq!((image.segments[0].vaddr, image.segments[0].filesz), (CODE, 3));
        assert_eq!((image.segments[1].vaddr, image.segments[1].memsz), (DATA, 4096));
        // R X -> code; R W -> data (no execute); R only -> read-only
        assert_eq!(image.segments[0].page_flags(), Flags::USER_CODE);
        assert_eq!(image.segments[1].page_flags(), Flags::USER_DATA);
        let read_only = build_test_elf(
            CODE,
            &[
                TestSegment {
                    vaddr: CODE,
                    flags: 5,
                    bytes: b"\xC3",
                    memsz: 1,
                },
                TestSegment {
                    vaddr: DATA,
                    flags: 4,
                    bytes: b"x",
                    memsz: 1,
                },
            ],
        );
        assert_eq!(
            parse(&read_only).unwrap().segments[1].page_flags(),
            Flags::USER_READ_ONLY
        );
    }

    #[test_case]
    fn damaged_headers_are_refused() {
        assert_eq!(parse(&valid()[..63]).unwrap_err(), ElfError::TooShort);
        assert_eq!(parse(&patched(0, b"\x7FELG")).unwrap_err(), ElfError::BadMagic);
        assert_eq!(parse(&patched(4, &[1])).unwrap_err(), ElfError::BadMagic, "32-bit");
        assert_eq!(parse(&patched(5, &[2])).unwrap_err(), ElfError::BadMagic, "big endian");
        assert_eq!(
            parse(&patched(18, &[0x28, 0])).unwrap_err(),
            ElfError::WrongArchitecture,
            "ARM"
        );
        assert_eq!(
            parse(&patched(16, &[3, 0])).unwrap_err(),
            ElfError::NotExecutable,
            "ET_DYN"
        );
        assert_eq!(parse(&patched(54, &[32, 0])).unwrap_err(), ElfError::BadProgramHeaders);
        assert_eq!(
            parse(&patched(56, &[9, 0])).unwrap_err(),
            ElfError::BadProgramHeaders,
            "table past the end"
        );
        assert_eq!(parse(&patched(56, &[0, 0])).unwrap_err(), ElfError::NoLoadableSegment);
        assert_eq!(
            parse(&patched(32, &[0xFF; 8])).unwrap_err(),
            ElfError::BadProgramHeaders,
            "wraps around"
        );
        assert_eq!(
            parse(&patched(PH0, &[4, 0, 0, 0])).unwrap_err(),
            ElfError::BadEntryPoint,
            "no LOAD with X left"
        );
    }

    #[test_case]
    fn segments_must_stay_in_the_file_and_in_the_lower_half() {
        // memsz smaller than filesz
        assert_eq!(
            parse(&patched(PH0 + 40, &[1, 0])).unwrap_err(),
            ElfError::BadProgramHeaders
        );
        // the bytes start past the end of the file; and a length that would wrap around
        assert_eq!(
            parse(&patched(PH0 + 8, &[0xFF; 8])).unwrap_err(),
            ElfError::SegmentOutsideFile
        );
        assert_eq!(
            parse(&patched(PH0 + 32, &[0xFF; 8])).unwrap_err(),
            ElfError::BadProgramHeaders
        );
        // in the kernel's half; below the first 64 KiB; crossing the end of the image area; wrapping around
        for (vaddr, memsz) in [
            (0xFFFF_FFFF_8000_0000u64, 3u64),
            (0x8000, 3),
            (IMAGE_END - 2, 3),
            (USER_END - 8, 3),
            (u64::MAX - 1, 3),
            (CODE, u64::MAX),
        ] {
            let file = patched(PH0 + 16, &vaddr.to_le_bytes());
            let mut file = file;
            file[PH0 + 40..PH0 + 48].copy_from_slice(&memsz.to_le_bytes());
            assert_eq!(
                parse(&file).unwrap_err(),
                ElfError::SegmentOutsideUserSpace,
                "{vaddr:#x} + {memsz:#x}"
            );
        }
    }

    #[test_case]
    fn code_that_can_change_itself_is_refused() {
        // flags 7 = R W X
        assert_eq!(
            parse(&patched(PH0 + 4, &[7, 0, 0, 0])).unwrap_err(),
            ElfError::WritableAndExecutable
        );
        // the entry point in the data segment, which cannot execute
        assert_eq!(
            parse(&patched(24, &DATA.to_le_bytes())).unwrap_err(),
            ElfError::BadEntryPoint
        );
    }

    #[test_case]
    fn loading_copies_the_bytes_and_zeros_the_rest() {
        let file = build_test_elf(
            CODE,
            &[
                TestSegment {
                    vaddr: CODE,
                    flags: 5,
                    bytes: b"\xB8\x2A\0\0\0\xC3",
                    memsz: 6,
                },
                // 5000 bytes in memory, 5 in the file: a page and a bit, the rest is zero
                TestSegment {
                    vaddr: DATA,
                    flags: 6,
                    bytes: b"hello",
                    memsz: 5000,
                },
            ],
        );
        let image = parse(&file).unwrap();
        let free = pmm::free_frames();
        let mut space = AddressSpace::new().unwrap();
        load(&mut space, &file, &image).unwrap();
        let mut code = [0; 6];
        space.read(CODE, &mut code).unwrap();
        assert_eq!(&code, b"\xB8\x2A\0\0\0\xC3");
        let mut data = [0xFF; 5000];
        space.read(DATA, &mut data).unwrap();
        assert_eq!(&data[..5], b"hello");
        assert!(data[5..].iter().all(|&b| b == 0));
        assert!(space.is_mapped(DATA + PAGE_SIZE), "5000 bytes need a second page");
        assert!(!space.is_mapped(DATA + 2 * PAGE_SIZE));
        drop(space);
        assert_eq!(pmm::free_frames(), free);
    }

    #[test_case]
    fn two_segments_on_one_page_are_an_error() {
        let file = build_test_elf(
            CODE,
            &[
                TestSegment {
                    vaddr: CODE,
                    flags: 5,
                    bytes: b"\xC3",
                    memsz: 1,
                },
                TestSegment {
                    vaddr: CODE + 0x100,
                    flags: 6,
                    bytes: b"x",
                    memsz: 1,
                },
            ],
        );
        let image = parse(&file).unwrap();
        let mut space = AddressSpace::new().unwrap();
        assert_eq!(load(&mut space, &file, &image), Err(MapError::AlreadyMapped));
    }
}
