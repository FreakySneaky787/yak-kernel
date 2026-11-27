//! The I/O APIC: where the interrupt lines of the devices end. It replaces the PIC: for each of its inputs a
//! *redirection entry* says which vector to raise on which CPU - or that the input is masked.
//!
//! It has only two registers in memory: write a register number to SELECT, then read or write that register
//! through WINDOW. The redirection entry of input `n` is 64 bits, registers 0x10 + 2n (low half) and 0x11 + 2n.

use crate::mem::PhysAddr;
use crate::paging;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const SELECT: u64 = 0x00;
const WINDOW: u64 = 0x10;
const VERSION: u32 = 0x01;
const REDIRECTION: u32 = 0x10;

/// In a redirection entry: this input raises nothing.
const MASKED: u64 = 1 << 16;

static BASE: AtomicU64 = AtomicU64::new(0);
/// The global system interrupt (GSI) of input 0 - with more than one I/O APIC, the second one starts higher.
static GSI_BASE: AtomicU32 = AtomicU32::new(0);

// SELECT and WINDOW belong together: two CPUs, or an interrupt handler, using them at once would mix them up. Only
// `interrupts::start` uses them, once.
fn read(register: u32) -> u32 {
    let base = BASE.load(Ordering::Relaxed);
    unsafe {
        ((base + SELECT) as *mut u32).write_volatile(register);
        ((base + WINDOW) as *const u32).read_volatile()
    }
}

fn write(register: u32, value: u32) {
    let base = BASE.load(Ordering::Relaxed);
    unsafe {
        ((base + SELECT) as *mut u32).write_volatile(register);
        ((base + WINDOW) as *mut u32).write_volatile(value);
    }
}

/// Maps the registers and masks every input: nothing arrives until `route` says where.
pub fn init(registers: PhysAddr, gsi_base: u32) {
    let virt = paging::map_device(registers).expect("the I/O APIC's registers can be mapped");
    BASE.store(virt, Ordering::Relaxed);
    GSI_BASE.store(gsi_base, Ordering::Relaxed);
    for input in 0..inputs() {
        set_entry(input, MASKED);
    }
}

/// How many inputs it has: bits 16-23 of the version register hold the number of the last one.
pub fn inputs() -> u32 {
    (read(VERSION) >> 16 & 0xFF) + 1
}

fn input(gsi: u32) -> u32 {
    let input = gsi - GSI_BASE.load(Ordering::Relaxed);
    assert!(input < inputs(), "GSI {} is not on this I/O APIC", gsi);
    input
}

fn entry(input: u32) -> u64 {
    let low = read(REDIRECTION + 2 * input) as u64;
    let high = read(REDIRECTION + 2 * input + 1) as u64;
    high << 32 | low
}

fn set_entry(input: u32, value: u64) {
    // the high half (the destination) first: the low half may unmask, and then the destination must be right
    write(REDIRECTION + 2 * input + 1, (value >> 32) as u32);
    write(REDIRECTION + 2 * input, value as u32);
}

/// Lets `gsi` raise `vector` on the CPU with the APIC ID `cpu`. All other bits 0: delivered as a normal ("fixed")
/// interrupt, edge-triggered, active high - what ISA lines are.
pub fn route(gsi: u32, vector: u8, cpu: u8) {
    set_entry(input(gsi), vector as u64 | (cpu as u64) << 56);
}

pub fn mask(gsi: u32) {
    let input = input(gsi);
    set_entry(input, entry(input) | MASKED);
}

/// The vector `gsi` raises, or `None` while it is masked - for the tests and the boot output.
pub fn routed_to(gsi: u32) -> Option<u8> {
    let entry = entry(input(gsi));
    (entry & MASKED == 0).then_some(entry as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn qemus_io_apic_has_24_inputs() {
        assert_eq!(inputs(), 24);
    }

    #[test_case]
    fn only_the_keyboard_is_routed() {
        // input 2 - the PIT's - carried the clock until the local APIC timer took over; now it is masked again
        let routed: alloc::vec::Vec<(u32, u8)> = (0..inputs())
            .filter_map(|gsi| routed_to(gsi).map(|vector| (gsi, vector)))
            .collect();
        assert_eq!(routed, [(1, 33)]);
    }
}
