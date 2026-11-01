//! The local APIC: the interrupt controller inside every CPU core. Every interrupt reaches the core through it, it
//! wants an *end of interrupt* (EOI) after each one, like the PIC - and it has a timer of its own.
//!
//! Its registers are memory: 4 KiB at a physical address the MADT names (on PCs 0xFEE00000), one 32-bit register
//! every 16 bytes. Reads and writes go to the APIC, not to RAM - so the page is mapped uncached, and every access
//! is `volatile`, or the compiler could merge or drop it.

use crate::mem::PhysAddr;
use crate::paging;
use crate::time;
use core::sync::atomic::{AtomicU64, Ordering};

// Register offsets (Intel SDM Vol. 3A, table 11-1)
const ID: u64 = 0x20;
#[cfg(test)]
const VERSION: u64 = 0x30;
const TASK_PRIORITY: u64 = 0x80;
const END_OF_INTERRUPT: u64 = 0xB0;
const SPURIOUS: u64 = 0xF0;
const LVT_TIMER: u64 = 0x320;
const TIMER_INITIAL: u64 = 0x380;
const TIMER_CURRENT: u64 = 0x390;
const TIMER_DIVIDE: u64 = 0x3E0;

/// Bit 8 of the spurious register: the APIC is switched on. Without it, it delivers nothing.
const SOFTWARE_ENABLE: u32 = 1 << 8;
/// In a "local vector table" (LVT) entry: no interrupt from this source.
const MASKED: u32 = 1 << 16;
/// In the timer's LVT entry: count down, raise the interrupt, start again - not just once.
const PERIODIC: u32 = 1 << 17;
const DIVIDE_BY_16: u32 = 0b0011;

/// The APIC raises this vector when an interrupt vanished before it could deliver it. Such a *spurious* interrupt
/// gets no EOI.
pub const SPURIOUS_VECTOR: u8 = 0xFF;

/// The virtual address of the registers (0: not mapped yet).
static BASE: AtomicU64 = AtomicU64::new(0);

fn read(register: u64) -> u32 {
    let address = BASE.load(Ordering::Relaxed) + register;
    unsafe { (address as *const u32).read_volatile() }
}

fn write(register: u64, value: u32) {
    let address = BASE.load(Ordering::Relaxed) + register;
    unsafe { (address as *mut u32).write_volatile(value) };
}

/// Maps the registers and switches the APIC on: all priorities accepted, spurious interrupts on `SPURIOUS_VECTOR`.
pub fn init(registers: PhysAddr) {
    let virt = paging::map_device(registers).expect("the local APIC's registers can be mapped");
    BASE.store(virt, Ordering::Relaxed);
    write(TASK_PRIORITY, 0);
    write(SPURIOUS, SOFTWARE_ENABLE | SPURIOUS_VECTOR as u32);
}

/// The APIC ID of this CPU - how the I/O APIC addresses it.
pub fn id() -> u8 {
    (read(ID) >> 24) as u8
}

/// "Done with this interrupt" - the last thing every IRQ handler does.
pub fn end_of_interrupt() {
    write(END_OF_INTERRUPT, 0);
}

/// How far the timer counts down during `ticks` ticks of the clock that is running now (the PIT's). The timer's
/// speed depends on the machine, so we measure it instead of trusting a number. Needs interrupts on.
pub fn measure_timer(ticks: u64) -> u32 {
    write(TIMER_DIVIDE, DIVIDE_BY_16);
    write(LVT_TIMER, MASKED); // it counts, but raises nothing
    // start right after a tick, so that we measure whole ticks
    let now = time::ticks();
    while time::ticks() == now {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
    write(TIMER_INITIAL, u32::MAX);
    let start = time::ticks();
    while time::ticks() < start + ticks {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
    let counted = u32::MAX - read(TIMER_CURRENT);
    write(TIMER_INITIAL, 0); // stop it
    counted / ticks as u32
}

/// Starts the timer: an interrupt on `vector` every `counts` counts, again and again.
pub fn start_timer(vector: u8, counts: u32) {
    write(TIMER_DIVIDE, DIVIDE_BY_16);
    write(LVT_TIMER, vector as u32 | PERIODIC);
    write(TIMER_INITIAL, counts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::arch::x86_64::__cpuid;

    #[test_case]
    fn the_cpu_has_a_local_apic() {
        // CPUID leaf 1, EDX bit 9 - chapters 6 to 11 ran QEMU with `-cpu qemu64,-apic`, where it was 0
        #[allow(unused_unsafe)] // `__cpuid` is safe on newer toolchains
        let edx = unsafe { __cpuid(1) }.edx;
        assert_ne!(edx & (1 << 9), 0);
    }

    #[test_case]
    fn the_registers_are_mapped_uncached() {
        let registers = crate::acpi::read().unwrap().madt.local_apic;
        assert_eq!(BASE.load(Ordering::Relaxed), registers.to_virt());
        let flags = paging::flags(registers.to_virt()).expect("mapped as a 4 KiB page");
        // contains, not equals: the CPU sets bit 5 (accessed) and 6 (dirty) itself once the page is read and written
        assert!(flags.contains(paging::Flags::DEVICE), "{:?}", flags);
    }

    #[test_case]
    fn the_apic_is_on_and_built_into_the_cpu() {
        assert_eq!(read(SPURIOUS), SOFTWARE_ENABLE | SPURIOUS_VECTOR as u32);
        // version 0x10 and up: an APIC inside the CPU, not the separate 82489DX chip of the 486 era
        assert!(read(VERSION) & 0xFF >= 0x10);
    }

    #[test_case]
    fn its_id_is_the_one_cpuid_reports() {
        #[allow(unused_unsafe)]
        let ebx = unsafe { __cpuid(1) }.ebx;
        assert_eq!(id(), (ebx >> 24) as u8);
    }

    #[test_case]
    fn the_timer_was_measured_against_a_100_hz_clock() {
        // QEMU's local APIC counts at 1 GHz: divided by 16, that is 625 000 counts per 10 ms. Half of it would mean
        // that the PIT's interrupts came twice as often as they should (bug #27)
        let counts = read(TIMER_INITIAL);
        assert!((600_000..650_000).contains(&counts), "{} counts per tick", counts);
    }

    #[test_case]
    fn the_timer_runs_periodically_on_vector_32() {
        assert_eq!(read(LVT_TIMER), 32 | PERIODIC);
        assert!(read(TIMER_INITIAL) > 0);
        // it counts down from there, and starts again: two reads a moment apart differ
        let first = read(TIMER_CURRENT);
        let second = read(TIMER_CURRENT);
        assert_ne!(first, second);
    }
}
