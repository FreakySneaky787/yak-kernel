//! ACPI: tables in memory in which the firmware describes the machine. We read one of them, the MADT - which CPUs
//! there are, where the interrupt controllers' registers lie, and how the old ISA interrupt lines are wired.
//!
//! The way there: Limine hands us the *RSDP*, which points to the *XSDT* (or, before ACPI 2.0, the RSDT), a list of
//! the physical addresses of all other tables. Every table starts with the same 36-byte header: a four-letter
//! signature ("APIC" for the MADT), its length, and a checksum - all its bytes add up to 0.

use crate::mem::{PAGE_SIZE, PhysAddr, align_down};
use crate::paging;
use alloc::vec::Vec;

const HEADER_LENGTH: usize = 36;

#[derive(Debug, PartialEq, Eq)]
pub enum AcpiError {
    NoRsdp,
    BadSignature(&'static str),
    BadChecksum(&'static str),
    NotMapped,
    NoMadt,
    NoIoApic,
}

/// What the MADT says - the parts this chapter uses.
#[derive(Debug)]
pub struct Madt {
    /// Physical address of the local APIC's registers - the same on every CPU.
    pub local_apic: PhysAddr,
    /// The APIC IDs of the CPUs the firmware lists as usable - one local APIC per CPU.
    pub apic_ids: Vec<u8>,
    /// Physical address of the (first) I/O APIC's registers, and the first interrupt input it handles.
    pub io_apic: PhysAddr,
    pub io_apic_gsi_base: u32,
    /// The ISA interrupt lines that are not wired the obvious way.
    pub overrides: Vec<Override>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Override {
    pub irq: u8,
    /// The *global system interrupt*: the I/O APIC input the line really arrives at.
    pub gsi: u32,
}

impl Madt {
    /// The I/O APIC input of ISA IRQ `irq`: the override's if there is one, else the same number.
    pub fn gsi(&self, irq: u8) -> u32 {
        self.overrides
            .iter()
            .find(|o| o.irq == irq)
            .map_or(irq as u32, |o| o.gsi)
    }
}

/// The RSDP's revision, the signatures of all tables, and the MADT.
#[derive(Debug)]
pub struct Acpi {
    pub revision: u8,
    pub tables: Vec<[u8; 4]>,
    pub madt: Madt,
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

/// ACPI's checksum: all bytes, added up with wrap-around, give 0.
pub fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)) == 0
}

/// `length` bytes of physical memory, through the HHDM. Limine maps the ACPI tables there; a page it left out is
/// mapped here.
fn bytes(phys: u64, length: usize) -> Result<&'static [u8], AcpiError> {
    let end = phys + length as u64;
    let mut page = align_down(phys, PAGE_SIZE);
    while page < end {
        paging::map_device(PhysAddr(page)).map_err(|_| AcpiError::NotMapped)?;
        page += PAGE_SIZE;
    }
    // Safety: mapped now, and ACPI tables are never written - by nobody, while the kernel runs
    Ok(unsafe { core::slice::from_raw_parts(PhysAddr(phys).as_mut_ptr::<u8>(), length) })
}

/// A whole table at `phys`: its header says how long it is; its checksum must be right.
fn table(phys: u64) -> Result<&'static [u8], AcpiError> {
    let length = u32_at(bytes(phys, HEADER_LENGTH)?, 4) as usize;
    let table = bytes(phys, length)?;
    if !checksum_ok(table) {
        return Err(AcpiError::BadChecksum("table"));
    }
    Ok(table)
}

/// Follows RSDP -> XSDT/RSDT -> every table, and reads the MADT.
pub fn read() -> Result<Acpi, AcpiError> {
    let rsdp_virt = crate::RSDP.response().ok_or(AcpiError::NoRsdp)?.address as u64;
    let rsdp_phys = rsdp_virt - crate::mem::hhdm_offset(); // Limine gives an HHDM address
    let rsdp = bytes(rsdp_phys, 20)?;
    if &rsdp[0..8] != b"RSD PTR " {
        return Err(AcpiError::BadSignature("RSDP"));
    }
    if !checksum_ok(rsdp) {
        return Err(AcpiError::BadChecksum("RSDP"));
    }
    let revision = rsdp[15];
    // ACPI 2.0 and later (revision 2): a longer RSDP with the XSDT's 64-bit address; before: the RSDT's 32-bit one
    let (root, signature, entry_size) = if revision >= 2 {
        let rsdp = bytes(rsdp_phys, 36)?;
        if !checksum_ok(rsdp) {
            return Err(AcpiError::BadChecksum("RSDP 2.0"));
        }
        (u64_at(rsdp, 24), b"XSDT", 8)
    } else {
        (u32_at(rsdp, 16) as u64, b"RSDT", 4)
    };
    let root = table(root)?;
    if &root[0..4] != signature {
        return Err(AcpiError::BadSignature("XSDT/RSDT"));
    }
    let mut tables = Vec::new();
    let mut madt = None;
    for at in (HEADER_LENGTH..root.len()).step_by(entry_size) {
        let phys = match entry_size {
            8 => u64_at(root, at),
            _ => u32_at(root, at) as u64,
        };
        let signature: [u8; 4] = bytes(phys, 4)?.try_into().unwrap();
        tables.push(signature);
        if &signature == b"APIC" {
            madt = Some(parse_madt(table(phys)?)?);
        }
    }
    Ok(Acpi {
        revision,
        tables,
        madt: madt.ok_or(AcpiError::NoMadt)?,
    })
}

/// The MADT's entries: after the header and two fields, a list of (type, length, ...) records.
pub fn parse_madt(table: &[u8]) -> Result<Madt, AcpiError> {
    let mut local_apic = PhysAddr(u32_at(table, 36) as u64);
    let mut apic_ids = Vec::new();
    let mut io_apic = None;
    let mut overrides = Vec::new();
    let mut at = HEADER_LENGTH + 8; // + local APIC address and flags
    while at + 2 <= table.len() {
        let (kind, length) = (table[at], table[at + 1] as usize);
        if length < 2 || at + length > table.len() {
            break; // a broken entry: stop rather than read past the table
        }
        let entry = &table[at..at + length];
        match kind {
            // a CPU's local APIC; flag bit 0: enabled, bit 1: can be enabled later
            0 if u32_at(entry, 4) & 0b11 != 0 => apic_ids.push(entry[3]),
            1 if io_apic.is_none() => io_apic = Some((PhysAddr(u32_at(entry, 4) as u64), u32_at(entry, 8))),
            // an override also says whether the line is active-low or level-triggered: not so for IRQ 0 and 1
            2 => overrides.push(Override {
                irq: entry[3],
                gsi: u32_at(entry, 4),
            }),
            5 => local_apic = PhysAddr(u64_at(entry, 4)), // a 64-bit address instead of the 32-bit one
            _ => {}                                       // NMI wiring, x2APIC CPUs, ...: not needed here
        }
        at += length;
    }
    let (io_apic, io_apic_gsi_base) = io_apic.ok_or(AcpiError::NoIoApic)?;
    Ok(Madt {
        local_apic,
        apic_ids,
        io_apic,
        io_apic_gsi_base,
        overrides,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn the_checksum_adds_up_to_zero() {
        assert!(checksum_ok(&[0x10, 0xF0]));
        assert!(!checksum_ok(&[0x10, 0xF1]));
        assert!(checksum_ok(&[]));
    }

    #[test_case]
    fn a_handmade_madt_is_parsed() {
        let mut table = [0u8; 44].to_vec();
        table[36..40].copy_from_slice(&0xFEE0_0000u32.to_le_bytes());
        table.extend_from_slice(&[0, 8, 0, 0, 1, 0, 0, 0]); // CPU 0, enabled
        table.extend_from_slice(&[0, 8, 1, 1, 0, 0, 0, 0]); // CPU 1, disabled: does not count
        table.extend_from_slice(&[1, 12, 0, 0, 0x00, 0x00, 0xC0, 0xFE, 0, 0, 0, 0]); // I/O APIC at 0xfec00000
        table.extend_from_slice(&[2, 10, 0, 0, 2, 0, 0, 0, 0, 0]); // ISA IRQ 0 -> GSI 2
        table.extend_from_slice(&[9, 4, 0, 0]); // a type we do not know: skipped
        let madt = parse_madt(&table).unwrap();
        assert_eq!(madt.local_apic, PhysAddr(0xFEE0_0000));
        assert_eq!(madt.apic_ids, [0]);
        assert_eq!((madt.io_apic, madt.io_apic_gsi_base), (PhysAddr(0xFEC0_0000), 0));
        assert_eq!((madt.gsi(0), madt.gsi(1)), (2, 1));
    }

    #[test_case]
    fn a_madt_without_io_apic_is_refused() {
        let mut table = [0u8; 44].to_vec();
        table.extend_from_slice(&[0, 8, 0, 0, 1, 0, 0, 0]);
        assert_eq!(parse_madt(&table).unwrap_err(), AcpiError::NoIoApic);
    }

    #[test_case]
    fn qemus_tables_describe_qemus_machine() {
        let acpi = read().unwrap();
        assert!(acpi.tables.contains(b"APIC") && acpi.tables.contains(b"FACP"));
        let madt = &acpi.madt;
        // run-qemu.sh gives QEMU 4 CPUs (KERNEL_SMP changes it); QEMU numbers their APICs from 0
        let expected: Vec<u8> = (0..madt.apic_ids.len() as u8).collect();
        assert_eq!(madt.apic_ids, expected);
        assert_eq!(madt.local_apic, PhysAddr(0xFEE0_0000));
        assert_eq!((madt.io_apic, madt.io_apic_gsi_base), (PhysAddr(0xFEC0_0000), 0));
        // like on most PCs, the timer's ISA IRQ 0 arrives at I/O APIC input 2; the keyboard's IRQ 1 at input 1
        assert_eq!((madt.gsi(0), madt.gsi(1)), (2, 1));
    }
}
