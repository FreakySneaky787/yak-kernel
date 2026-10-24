//! I/O APIC: routes device interrupt lines ("global system interrupts", GSIs)
//! to interrupt vectors on a chosen CPU. Replaces the 8259 PIC.
//!
//! Two memory-mapped registers: IOREGSEL (select register index) and IOWIN
//! (read/write the selected register). Each input has a 64-bit redirection
//! entry at index 0x10 + 2 * n.

use crate::mem::PhysAddr;
use crate::mem::paging::{self, Flags};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const IOREGSEL: u64 = 0x00;
const IOWIN: u64 = 0x10;
const REG_ID: u32 = 0x00;
const REG_VERSION: u32 = 0x01;
const REG_REDIRECTION: u32 = 0x10;

const ENTRY_ACTIVE_LOW: u64 = 1 << 13;
const ENTRY_LEVEL: u64 = 1 << 15;
const ENTRY_MASKED: u64 = 1 << 16;

static BASE: AtomicU64 = AtomicU64::new(0);
static GSI_BASE: AtomicU32 = AtomicU32::new(0);

fn read(reg: u32) -> u32 {
    let base = BASE.load(Ordering::Relaxed);
    unsafe {
        ((base + IOREGSEL) as *mut u32).write_volatile(reg);
        ((base + IOWIN) as *const u32).read_volatile()
    }
}

fn write(reg: u32, value: u32) {
    let base = BASE.load(Ordering::Relaxed);
    unsafe {
        ((base + IOREGSEL) as *mut u32).write_volatile(reg);
        ((base + IOWIN) as *mut u32).write_volatile(value);
    }
}

/// Maps the registers and masks every input.
pub fn init(phys_base: u32, gsi_base: u32) {
    let virt = paging::ensure_hhdm_mapped(PhysAddr(phys_base as u64), 0x20, Flags::MMIO)
        .expect("cannot map I/O APIC registers");
    BASE.store(virt.as_u64(), Ordering::Relaxed);
    GSI_BASE.store(gsi_base, Ordering::Relaxed);
    for input in 0..input_count() {
        write_entry(input, ENTRY_MASKED);
    }
}

pub fn id() -> u8 {
    ((read(REG_ID) >> 24) & 0x0F) as u8
}

/// Number of interrupt inputs (redirection entries).
pub fn input_count() -> u32 {
    ((read(REG_VERSION) >> 16) & 0xFF) + 1
}

fn write_entry(input: u32, value: u64) {
    // high half first, so the entry is never unmasked with a stale destination
    write(REG_REDIRECTION + 2 * input + 1, (value >> 32) as u32);
    write(REG_REDIRECTION + 2 * input, value as u32);
}

pub fn read_entry(input: u32) -> u64 {
    let low = read(REG_REDIRECTION + 2 * input) as u64;
    let high = read(REG_REDIRECTION + 2 * input + 1) as u64;
    high << 32 | low
}

/// Routes `gsi` to `vector` on the CPU with APIC ID `dest` (fixed delivery,
/// physical destination) and unmasks it.
pub fn route(gsi: u32, vector: u8, dest: u8, active_low: bool, level: bool) {
    let input = gsi - GSI_BASE.load(Ordering::Relaxed);
    assert!(input < input_count(), "GSI {} not handled by this I/O APIC", gsi);
    let mut entry = vector as u64 | (dest as u64) << 56;
    if active_low {
        entry |= ENTRY_ACTIVE_LOW;
    }
    if level {
        entry |= ENTRY_LEVEL;
    }
    write_entry(input, entry);
}

pub fn mask(gsi: u32) {
    let input = gsi - GSI_BASE.load(Ordering::Relaxed);
    write_entry(input, read_entry(input) | ENTRY_MASKED);
}

pub fn is_masked(gsi: u32) -> bool {
    read_entry(gsi - GSI_BASE.load(Ordering::Relaxed)) & ENTRY_MASKED != 0
}

/// Vector a GSI is routed to.
pub fn vector_of(gsi: u32) -> u8 {
    read_entry(gsi - GSI_BASE.load(Ordering::Relaxed)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn ioapic_has_24_inputs() {
        assert_eq!(input_count(), 24); // QEMU's I/O APIC
    }
}
