//! Local APIC: the per-CPU interrupt controller. It receives interrupts from
//! the I/O APIC (and other CPUs), has its own timer, and must be told when an
//! interrupt has been handled (EOI).
//!
//! Its registers are memory-mapped (default physical address 0xFEE0_0000) and
//! accessed with 32-bit reads/writes through the HHDM.

use super::msr::{self, IA32_APIC_BASE};
use super::pit;
use crate::mem::PhysAddr;
use crate::mem::paging::{self, Flags};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const APIC_GLOBAL_ENABLE: u64 = 1 << 11;

// Register offsets (Intel SDM Vol. 3, 11.4.1)
const REG_ID: u32 = 0x20;
const REG_VERSION: u32 = 0x30;
const REG_TPR: u32 = 0x80;
const REG_EOI: u32 = 0xB0;
const REG_SPURIOUS: u32 = 0xF0;
const REG_ERROR_STATUS: u32 = 0x280;
/// Interrupt command register (low word triggers the IPI, high word = destination).
const REG_ICR_LOW: u32 = 0x300;
const REG_ICR_HIGH: u32 = 0x310;
const REG_LVT_TIMER: u32 = 0x320;
const REG_LVT_LINT0: u32 = 0x350;
const REG_LVT_LINT1: u32 = 0x360;
const REG_LVT_ERROR: u32 = 0x370;
const REG_TIMER_INITIAL: u32 = 0x380;
const REG_TIMER_CURRENT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3E0;

const LVT_MASKED: u32 = 1 << 16;
const LVT_TIMER_PERIODIC: u32 = 1 << 17;
const LVT_DELIVERY_NMI: u32 = 0b100 << 8;
const SPURIOUS_APIC_ENABLE: u32 = 1 << 8;
const TIMER_DIVIDE_BY_16: u32 = 0b0011;

pub const SPURIOUS_VECTOR: u8 = 0xFF;

/// Virtual address of the register block (0 = not initialized).
static BASE: AtomicU64 = AtomicU64::new(0);
/// LAPIC timer counts per timer tick, measured at boot.
static TICKS_PER_PERIOD: AtomicU32 = AtomicU32::new(0);

fn read(reg: u32) -> u32 {
    let base = BASE.load(Ordering::Relaxed);
    debug_assert!(base != 0, "local APIC not initialized");
    unsafe { ((base + reg as u64) as *const u32).read_volatile() }
}

fn write(reg: u32, value: u32) {
    let base = BASE.load(Ordering::Relaxed);
    debug_assert!(base != 0, "local APIC not initialized");
    unsafe { ((base + reg as u64) as *mut u32).write_volatile(value) };
}

pub fn is_enabled() -> bool {
    msr::read(IA32_APIC_BASE) & APIC_GLOBAL_ENABLE != 0
        && BASE.load(Ordering::Relaxed) != 0
        && read(REG_SPURIOUS) & SPURIOUS_APIC_ENABLE != 0
}

/// Maps the registers and enables the local APIC of the current CPU.
/// `phys_base` comes from the MADT (it normally matches the MSR).
pub fn init(phys_base: u64) {
    let virt =
        paging::ensure_hhdm_mapped(PhysAddr(phys_base), 0x1000, Flags::MMIO).expect("cannot map local APIC registers");
    BASE.store(virt.as_u64(), Ordering::Relaxed);
    init_local();
}

/// Whether `init` has run (the register block is mapped).
pub fn is_initialized() -> bool {
    BASE.load(Ordering::Relaxed) != 0
}

/// Enables and configures the local APIC of the CPU this runs on (every CPU
/// has its own; the registers are at the same address on all of them).
pub fn init_local() {
    let msr_value = msr::read(IA32_APIC_BASE);
    unsafe { msr::write(IA32_APIC_BASE, msr_value | APIC_GLOBAL_ENABLE) };

    write(REG_TPR, 0); // accept all priorities
    write(REG_LVT_TIMER, LVT_MASKED);
    write(REG_LVT_LINT0, LVT_MASKED); // legacy PIC input: unused, the 8259 is masked
    write(REG_LVT_LINT1, LVT_DELIVERY_NMI); // LINT1 is wired to NMI on PCs
    write(REG_LVT_ERROR, LVT_MASKED);
    write(REG_ERROR_STATUS, 0);
    write(REG_ERROR_STATUS, 0); // ESR must be written twice to clear it
    write(REG_SPURIOUS, SPURIOUS_APIC_ENABLE | SPURIOUS_VECTOR as u32);
    eoi(); // clear anything left over
}

/// APIC ID of the current CPU.
// ICR low word: delivery mode (bits 8-10), level assert (bit 14), delivery status (bit 12)
const ICR_FIXED: u32 = 0b000 << 8;
const ICR_INIT: u32 = 0b101 << 8;
const ICR_STARTUP: u32 = 0b110 << 8;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_DELIVERY_PENDING: u32 = 1 << 12;

fn send_ipi(apic_id: u8, low: u32) {
    // Two writes: an interrupt in between that sends an IPI itself would
    // replace the destination, and ours would go to the wrong CPU.
    super::without_interrupts(|| {
        write(REG_ICR_HIGH, (apic_id as u32) << 24);
        write(REG_ICR_LOW, low);
        while read(REG_ICR_LOW) & ICR_DELIVERY_PENDING != 0 {
            core::hint::spin_loop();
        }
    });
}

/// INIT IPI: resets the target CPU into its "wait for startup" state.
pub fn send_init(apic_id: u8) {
    send_ipi(apic_id, ICR_INIT | ICR_LEVEL_ASSERT);
}

/// Startup IPI: the target starts in real mode at physical `page << 12`.
pub fn send_startup(apic_id: u8, page: u8) {
    send_ipi(apic_id, ICR_STARTUP | ICR_LEVEL_ASSERT | page as u32);
}

/// A normal interrupt on `vector` at another CPU.
pub fn send_fixed(apic_id: u8, vector: u8) {
    send_ipi(apic_id, ICR_FIXED | ICR_LEVEL_ASSERT | vector as u32);
}

pub fn id() -> u8 {
    (read(REG_ID) >> 24) as u8
}

pub fn version() -> u32 {
    read(REG_VERSION) & 0xFF
}

/// Signals "interrupt handled". Must be the last thing an IRQ handler does.
pub fn eoi() {
    write(REG_EOI, 0);
}

/// Measures how fast the LAPIC timer counts, using PIT channel 2 as a
/// reference clock (works with interrupts disabled), then starts the timer
/// in periodic mode: `hz` interrupts per second on `vector`.
/// Starts this CPU's timer with the period measured by `start_timer` on the
/// boot CPU (all local APIC timers run at the same bus frequency).
pub fn start_timer_calibrated(vector: u8) {
    let per_period = TICKS_PER_PERIOD.load(Ordering::Relaxed);
    assert!(per_period != 0, "the timer was not calibrated yet");
    write(REG_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
    write(REG_LVT_TIMER, vector as u32 | LVT_TIMER_PERIODIC);
    write(REG_TIMER_INITIAL, per_period);
}

pub fn start_timer(vector: u8, hz: u32) {
    const CALIBRATION_MS: u32 = 50;
    write(REG_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
    write(REG_LVT_TIMER, LVT_MASKED);

    pit::channel2_start_oneshot(CALIBRATION_MS);
    write(REG_TIMER_INITIAL, u32::MAX);
    pit::channel2_wait();
    let elapsed = u32::MAX - read(REG_TIMER_CURRENT);
    write(REG_TIMER_INITIAL, 0);

    let per_second = elapsed as u64 * 1000 / CALIBRATION_MS as u64;
    let per_period = (per_second / hz as u64).max(1) as u32;
    TICKS_PER_PERIOD.store(per_period, Ordering::Relaxed);

    write(REG_LVT_TIMER, vector as u32 | LVT_TIMER_PERIODIC);
    write(REG_TIMER_INITIAL, per_period);
}

/// LAPIC timer counts per period (0 if the timer is not running).
pub fn timer_period_counts() -> u32 {
    TICKS_PER_PERIOD.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn lapic_is_enabled() {
        assert!(is_enabled());
        assert!(version() >= 0x10, "not an integrated APIC? version {:#x}", version());
    }

    #[test_case]
    #[allow(unused_unsafe)] // `__cpuid` is unsafe on older toolchains
    fn id_matches_cpuid() {
        let initial_apic_id = (unsafe { core::arch::x86_64::__cpuid(1) }.ebx >> 24) as u8;
        assert_eq!(id(), initial_apic_id);
    }

    #[test_case]
    fn timer_is_periodic_and_calibrated() {
        assert!(timer_period_counts() > 0);
        let lvt = read(REG_LVT_TIMER);
        assert_eq!(lvt & LVT_MASKED, 0);
        assert_ne!(lvt & LVT_TIMER_PERIODIC, 0);
    }
}
