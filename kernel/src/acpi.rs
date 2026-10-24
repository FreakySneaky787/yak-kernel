//! Minimal ACPI table parser: finds the MADT ("APIC" table), which lists the
//! CPUs (local APICs), the I/O APICs and how legacy ISA IRQs are wired to
//! I/O APIC inputs ("interrupt source overrides").
//!
//! Chain: RSDP (from Limine) -> XSDT (64-bit pointers) or RSDT (32-bit, ACPI 1.0)
//! -> every table starts with a 36-byte SDT header -> MADT.
//! All tables are accessed through the HHDM (missing pages are mapped on demand).

use crate::mem::paging::{self, Flags};
use crate::mem::{PhysAddr, hhdm_offset};
use alloc::string::String;
use alloc::vec::Vec;
use spin::Once;

const SDT_HEADER_LEN: usize = 36;

#[derive(Debug, Clone)]
pub struct TableInfo {
    pub signature: [u8; 4],
    pub phys: u64,
    pub length: u32,
}

impl TableInfo {
    pub fn signature_str(&self) -> &str {
        core::str::from_utf8(&self.signature).unwrap_or("????")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LocalApic {
    pub processor_id: u8,
    pub apic_id: u8,
    /// Bit 0: enabled, bit 1: online capable
    pub flags: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct IoApic {
    pub id: u8,
    pub address: u32,
    /// First global system interrupt (GSI) this I/O APIC handles.
    pub gsi_base: u32,
}

/// "Legacy ISA IRQ `source` is wired to GSI `gsi` with these MPS flags".
#[derive(Debug, Clone, Copy)]
pub struct InterruptOverride {
    pub source: u8,
    pub gsi: u32,
    /// Bits 0-1: polarity (01 active high, 11 active low), bits 2-3: trigger (01 edge, 11 level)
    pub flags: u16,
}

impl InterruptOverride {
    pub fn active_low(&self) -> bool {
        self.flags & 0b11 == 0b11
    }

    pub fn level_triggered(&self) -> bool {
        (self.flags >> 2) & 0b11 == 0b11
    }
}

#[derive(Debug, Clone)]
pub struct Madt {
    pub local_apic_address: u64,
    /// The system also has legacy 8259 PICs (which must be masked when using the APIC).
    pub has_8259: bool,
    pub local_apics: Vec<LocalApic>,
    pub io_apics: Vec<IoApic>,
    pub overrides: Vec<InterruptOverride>,
}

impl Madt {
    /// GSI and override for a legacy ISA IRQ (identity mapping if no override exists).
    pub fn isa_irq(&self, irq: u8) -> (u32, Option<InterruptOverride>) {
        match self.overrides.iter().find(|o| o.source == irq) {
            Some(o) => (o.gsi, Some(*o)),
            None => (irq as u32, None),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcpiInfo {
    pub revision: u8,
    pub oem_id: String,
    pub tables: Vec<TableInfo>,
    pub madt: Madt,
}

static ACPI: Once<AcpiInfo> = Once::new();

pub fn info() -> Option<&'static AcpiInfo> {
    ACPI.get()
}

#[derive(Debug)]
pub enum AcpiError {
    NoRsdp,
    BadSignature(&'static str),
    BadChecksum(&'static str),
    NoMadt,
    MapFailed,
}

// ---- raw memory helpers -----------------------------------------------------------

/// Maps `len` bytes at physical `phys` (if needed) and returns them as a slice.
fn phys_bytes(phys: u64, len: usize) -> Result<&'static [u8], AcpiError> {
    let virt = paging::ensure_hhdm_mapped(PhysAddr(phys), len as u64, Flags::KERNEL_DATA)
        .map_err(|_| AcpiError::MapFailed)?;
    Ok(unsafe { core::slice::from_raw_parts(virt.as_ptr::<u8>(), len) })
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) == 0
}

/// Reads a complete SDT (header + body) and verifies its checksum.
fn read_table(phys: u64) -> Result<&'static [u8], AcpiError> {
    let header = phys_bytes(phys, SDT_HEADER_LEN)?;
    let length = u32_at(header, 4) as usize;
    let table = phys_bytes(phys, length)?;
    if !checksum_ok(table) {
        return Err(AcpiError::BadChecksum("SDT"));
    }
    Ok(table)
}

// ---- parsing ----------------------------------------------------------------------

/// Parses RSDP, root table and MADT. Requires the frame allocator and heap.
pub fn init() -> Result<&'static AcpiInfo, AcpiError> {
    if let Some(info) = ACPI.get() {
        return Ok(info);
    }
    let resp = crate::boot::RSDP.response().ok_or(AcpiError::NoRsdp)?;
    let addr = resp.address as u64;
    // Base revision 3 reports a physical address, newer revisions an HHDM address
    let rsdp_phys = if addr >= hhdm_offset() { addr - hhdm_offset() } else { addr };

    let rsdp = phys_bytes(rsdp_phys, 20)?;
    if &rsdp[0..8] != b"RSD PTR " {
        return Err(AcpiError::BadSignature("RSDP"));
    }
    if !checksum_ok(rsdp) {
        return Err(AcpiError::BadChecksum("RSDP"));
    }
    let revision = rsdp[15];
    let oem_id = String::from_utf8_lossy(&rsdp[9..15]).trim().into();

    // ACPI 2.0+: prefer the XSDT with 64-bit table pointers
    let (root_phys, entry_size) = if revision >= 2 {
        let rsdp2 = phys_bytes(rsdp_phys, 36)?;
        if !checksum_ok(rsdp2) {
            return Err(AcpiError::BadChecksum("RSDP v2"));
        }
        (u64_at(rsdp2, 24), 8)
    } else {
        (u32_at(rsdp, 16) as u64, 4)
    };

    let root = read_table(root_phys)?;
    let expected = if entry_size == 8 { b"XSDT" } else { b"RSDT" };
    if &root[0..4] != expected {
        return Err(AcpiError::BadSignature("XSDT/RSDT"));
    }

    let mut tables = Vec::new();
    let mut madt = None;
    for off in (SDT_HEADER_LEN..root.len()).step_by(entry_size) {
        let phys = if entry_size == 8 { u64_at(root, off) } else { u32_at(root, off) as u64 };
        let header = phys_bytes(phys, SDT_HEADER_LEN)?;
        let signature: [u8; 4] = header[0..4].try_into().unwrap();
        tables.push(TableInfo { signature, phys, length: u32_at(header, 4) });
        if &signature == b"APIC" {
            madt = Some(parse_madt(read_table(phys)?));
        }
    }

    let info = AcpiInfo { revision, oem_id, tables, madt: madt.ok_or(AcpiError::NoMadt)? };
    Ok(ACPI.call_once(|| info))
}

fn parse_madt(table: &[u8]) -> Madt {
    let mut madt = Madt {
        local_apic_address: u32_at(table, 36) as u64,
        has_8259: u32_at(table, 40) & 1 != 0,
        local_apics: Vec::new(),
        io_apics: Vec::new(),
        overrides: Vec::new(),
    };
    let mut off = 44;
    while off + 2 <= table.len() {
        let (kind, len) = (table[off], table[off + 1] as usize);
        if len < 2 || off + len > table.len() {
            break; // malformed entry
        }
        let e = &table[off..off + len];
        match kind {
            0 => madt.local_apics.push(LocalApic { processor_id: e[2], apic_id: e[3], flags: u32_at(e, 4) }),
            1 => madt.io_apics.push(IoApic { id: e[2], address: u32_at(e, 4), gsi_base: u32_at(e, 8) }),
            2 => madt.overrides.push(InterruptOverride { source: e[3], gsi: u32_at(e, 4), flags: u16_at(e, 8) }),
            5 => madt.local_apic_address = u64_at(e, 4), // 64-bit LAPIC address override
            _ => {} // NMI sources, x2APIC entries, ... not needed yet
        }
        off += len;
    }
    madt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn finds_madt_with_cpu_and_ioapic() {
        let info = init().expect("ACPI init failed");
        assert!(info.tables.iter().any(|t| t.signature_str() == "APIC"));
        assert!(info.tables.iter().any(|t| t.signature_str() == "FACP"), "no FADT?");
        let madt = &info.madt;
        assert!(!madt.local_apics.is_empty());
        assert_eq!(madt.io_apics.len(), 1);
        assert_eq!(madt.local_apic_address, 0xFEE0_0000);
    }

    #[test_case]
    fn qemu_routes_timer_irq_to_gsi2() {
        // QEMU (like most PCs) wires the PIT (ISA IRQ 0) to I/O APIC input 2
        let madt = &init().unwrap().madt;
        assert_eq!(madt.isa_irq(0).0, 2);
        assert_eq!(madt.isa_irq(1).0, 1); // keyboard: identity
    }

    #[test_case]
    fn checksum_helper() {
        assert!(checksum_ok(&[0x10, 0xF0]));
        assert!(!checksum_ok(&[0x10, 0xF1]));
    }
}
