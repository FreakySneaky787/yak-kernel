//! ELF64 executables: parsing and loading into an address space.
//!
//! We accept statically linked x86_64 executables (`ET_EXEC`, no dynamic
//! linking, no relocations). Only the program headers matter for loading:
//! every `PT_LOAD` segment says "map `memsz` bytes at `vaddr`, fill the first
//! `filesz` of them from file offset `offset`, zero the rest" (the zeroed
//! tail is `.bss`). `p_flags` give the permissions (R/W/X).

use super::{USER_IMAGE_END, USER_IMAGE_START};
use crate::mem::address_space::AddressSpace;
use crate::mem::paging::{Flags, MapError};
use crate::mem::{PAGE_SIZE, VirtAddr, align_down, align_up};
use alloc::vec::Vec;
use core::fmt;

const PT_LOAD: u32 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 0x3E;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;
const MAX_PHDRS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    TooShort,
    BadMagic,
    /// Not a 64-bit little-endian x86_64 file.
    WrongArchitecture,
    /// Not a static executable (`ET_EXEC`).
    NotExecutable,
    BadProgramHeaders,
    NoLoadableSegment,
    /// A segment reaches outside the file.
    SegmentOutsideFile,
    /// A segment would be mapped outside the user image area.
    SegmentOutsideUserSpace,
    /// The entry point is not inside an executable segment.
    BadEntryPoint,
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self {
            ElfError::TooShort => "file too short",
            ElfError::BadMagic => "not an ELF file",
            ElfError::WrongArchitecture => "not a 64-bit x86_64 ELF file",
            ElfError::NotExecutable => "not a static executable",
            ElfError::BadProgramHeaders => "invalid program headers",
            ElfError::NoLoadableSegment => "no loadable segment",
            ElfError::SegmentOutsideFile => "segment reaches past the end of the file",
            ElfError::SegmentOutsideUserSpace => "segment outside the user address range",
            ElfError::BadEntryPoint => "entry point outside executable code",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    pub memsz: u64,
    pub offset: u64,
    pub filesz: u64,
    /// `PF_R` / `PF_W` / `PF_X`
    pub flags: u32,
}

impl Segment {
    fn page_flags(&self) -> Flags {
        let mut flags = Flags::USER;
        if self.flags & PF_W != 0 {
            flags = flags | Flags::WRITABLE;
        }
        if self.flags & PF_X == 0 {
            flags = flags | Flags::NO_EXECUTE;
        }
        flags
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfImage {
    pub entry: u64,
    pub segments: Vec<Segment>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Checks the headers and collects the loadable segments.
pub fn parse(data: &[u8]) -> Result<ElfImage, ElfError> {
    if data.len() < EHDR_SIZE {
        return Err(if data.starts_with(b"\x7fELF") || data.len() < 4 {
            ElfError::TooShort
        } else {
            ElfError::BadMagic
        });
    }
    if &data[0..4] != b"\x7fELF" {
        return Err(ElfError::BadMagic);
    }
    // EI_CLASS = 2 (64 bit), EI_DATA = 1 (little endian), machine x86_64
    if data[4] != 2 || data[5] != 1 || u16_at(data, 18) != EM_X86_64 {
        return Err(ElfError::WrongArchitecture);
    }
    if u16_at(data, 16) != ET_EXEC {
        return Err(ElfError::NotExecutable);
    }
    let entry = u64_at(data, 24);
    let phoff = u64_at(data, 32) as usize;
    let phentsize = u16_at(data, 54) as usize;
    let phnum = u16_at(data, 56) as usize;
    if phentsize != PHDR_SIZE || phnum == 0 || phnum > MAX_PHDRS {
        return Err(ElfError::BadProgramHeaders);
    }
    let table_end = phoff
        .checked_add(phnum * PHDR_SIZE)
        .ok_or(ElfError::BadProgramHeaders)?;
    if table_end > data.len() {
        return Err(ElfError::BadProgramHeaders);
    }

    let mut segments = Vec::new();
    for i in 0..phnum {
        let ph = &data[phoff + i * PHDR_SIZE..phoff + (i + 1) * PHDR_SIZE];
        if u32_at(ph, 0) != PT_LOAD {
            continue;
        }
        let seg = Segment {
            flags: u32_at(ph, 4),
            offset: u64_at(ph, 8),
            vaddr: u64_at(ph, 16),
            filesz: u64_at(ph, 32),
            memsz: u64_at(ph, 40),
        };
        if seg.memsz == 0 {
            continue;
        }
        if seg.filesz > seg.memsz {
            return Err(ElfError::BadProgramHeaders);
        }
        let file_end = seg.offset.checked_add(seg.filesz).ok_or(ElfError::SegmentOutsideFile)?;
        if file_end > data.len() as u64 {
            return Err(ElfError::SegmentOutsideFile);
        }
        let mem_end = seg
            .vaddr
            .checked_add(seg.memsz)
            .ok_or(ElfError::SegmentOutsideUserSpace)?;
        if seg.vaddr < USER_IMAGE_START || mem_end > USER_IMAGE_END {
            return Err(ElfError::SegmentOutsideUserSpace);
        }
        segments.push(seg);
    }
    if segments.is_empty() {
        return Err(ElfError::NoLoadableSegment);
    }
    let entry_ok = segments
        .iter()
        .any(|s| s.flags & PF_X != 0 && (s.vaddr..s.vaddr + s.memsz).contains(&entry));
    if !entry_ok {
        return Err(ElfError::BadEntryPoint);
    }
    Ok(ElfImage { entry, segments })
}

/// Maps and fills all segments of `image` (parsed from `data`) in `space`.
/// A page shared by two segments keeps the flags of the first one.
pub fn load(space: &mut AddressSpace, data: &[u8], image: &ElfImage) -> Result<(), MapError> {
    for seg in &image.segments {
        let start = align_down(seg.vaddr, PAGE_SIZE);
        let end = align_up(seg.vaddr + seg.memsz, PAGE_SIZE);
        let mut page = start;
        while page < end {
            space.page_frame(VirtAddr(page), seg.page_flags())?;
            page += PAGE_SIZE;
        }
        let file = &data[seg.offset as usize..(seg.offset + seg.filesz) as usize];
        space.write(VirtAddr(seg.vaddr), file); // the rest stays zero (.bss)
    }
    Ok(())
}

/// Builds a minimal executable around `code` (for tests): one RX segment
/// at `USER_IMAGE_START + 0x1000` with the entry point at its start, and an
/// RW `.bss`-like segment of `bss` bytes right after it.
#[cfg(test)]
pub fn build_test_elf(code: &[u8], bss: u64) -> Vec<u8> {
    let base = USER_IMAGE_START + 0x1000;
    let code_off = 0x1000u64;
    let mut out = alloc::vec![0u8; code_off as usize];
    out[0..4].copy_from_slice(b"\x7fELF");
    out[4] = 2; // 64 bit
    out[5] = 1; // little endian
    out[6] = 1; // version
    out[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
    out[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
    out[20..24].copy_from_slice(&1u32.to_le_bytes());
    out[24..32].copy_from_slice(&base.to_le_bytes()); // entry
    out[32..40].copy_from_slice(&(EHDR_SIZE as u64).to_le_bytes()); // phoff
    out[52..54].copy_from_slice(&(EHDR_SIZE as u16).to_le_bytes());
    out[54..56].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
    out[56..58].copy_from_slice(&2u16.to_le_bytes());
    let mut phdr = |i: usize, flags: u32, offset: u64, vaddr: u64, filesz: u64, memsz: u64| {
        let o = EHDR_SIZE + i * PHDR_SIZE;
        out[o..o + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        out[o + 4..o + 8].copy_from_slice(&flags.to_le_bytes());
        out[o + 8..o + 16].copy_from_slice(&offset.to_le_bytes());
        out[o + 16..o + 24].copy_from_slice(&vaddr.to_le_bytes());
        out[o + 24..o + 32].copy_from_slice(&vaddr.to_le_bytes()); // paddr
        out[o + 32..o + 40].copy_from_slice(&filesz.to_le_bytes());
        out[o + 40..o + 48].copy_from_slice(&memsz.to_le_bytes());
        out[o + 48..o + 56].copy_from_slice(&PAGE_SIZE.to_le_bytes());
    };
    let code_len = code.len() as u64;
    phdr(0, PF_R | PF_X, code_off, base, code_len, code_len);
    let data_addr = align_up(base + code_len, PAGE_SIZE);
    phdr(1, PF_R | PF_W, code_off + code_len, data_addr, 0, bss.max(1));
    out.extend_from_slice(code);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::paging;

    const CODE: &[u8] = &[0xEB, 0xFE]; // jmp $

    #[test_case]
    fn parses_test_elf() {
        let elf = build_test_elf(CODE, 100);
        let image = parse(&elf).unwrap();
        assert_eq!(image.entry, USER_IMAGE_START + 0x1000);
        assert_eq!(image.segments.len(), 2);
        assert_eq!(image.segments[0].flags, PF_R | PF_X);
        assert_eq!(image.segments[1].memsz, 100);
    }

    #[test_case]
    fn rejects_broken_files() {
        let good = build_test_elf(CODE, 0);
        assert_eq!(parse(b"\x7fEL"), Err(ElfError::TooShort));
        assert_eq!(parse(&[b'#'; 100]), Err(ElfError::BadMagic));

        let mut e = good.clone();
        e[18] = 0x28; // ARM
        assert_eq!(parse(&e), Err(ElfError::WrongArchitecture));

        let mut e = good.clone();
        e[16] = 3; // ET_DYN (PIE / shared object)
        assert_eq!(parse(&e), Err(ElfError::NotExecutable));

        let mut e = good.clone();
        e.truncate(good.len() - 1); // code segment now reaches past the end
        assert_eq!(parse(&e), Err(ElfError::SegmentOutsideFile));

        let mut e = good.clone();
        let vaddr = EHDR_SIZE + 16;
        e[vaddr..vaddr + 8].copy_from_slice(&0xFFFF_8000_0000_0000u64.to_le_bytes());
        assert_eq!(parse(&e), Err(ElfError::SegmentOutsideUserSpace));

        let mut e = good.clone();
        e[24..32].copy_from_slice(&(USER_IMAGE_START + 0x5000).to_le_bytes());
        assert_eq!(parse(&e), Err(ElfError::BadEntryPoint));

        let mut e = good;
        e[56] = 0; // no program headers
        assert_eq!(parse(&e), Err(ElfError::BadProgramHeaders));
    }

    #[test_case]
    fn load_maps_segments_with_permissions() {
        let elf = build_test_elf(CODE, 5000);
        let image = parse(&elf).unwrap();
        let mut space = AddressSpace::new().unwrap();
        load(&mut space, &elf, &image).unwrap();

        let code = space.page_flags(VirtAddr(image.entry)).unwrap();
        assert!(code.contains(Flags::USER) && !code.contains(Flags::WRITABLE) && !code.contains(Flags::NO_EXECUTE));
        let frame = paging::translate_in(space.root(), VirtAddr(image.entry)).unwrap();
        assert_eq!(unsafe { *frame.to_virt().as_ptr::<[u8; 2]>() }, [0xEB, 0xFE]);

        let bss = image.segments[1].vaddr;
        for addr in [bss, bss + 4999] {
            let flags = space.page_flags(VirtAddr(addr)).unwrap();
            assert!(flags.contains(Flags::WRITABLE | Flags::NO_EXECUTE | Flags::USER));
        }
        assert!(space.page_flags(VirtAddr(bss + 2 * PAGE_SIZE)).is_none());
    }
}
