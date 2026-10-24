//! Global Descriptor Table + Task State Segment.
//!
//! In 64-bit mode segmentation is mostly disabled, but the CPU still wants to
//! see valid code/data descriptors. The important part is the
//! TSS: it contains the interrupt stack table (IST) - separate stacks the
//! CPU switches to for certain exceptions (e.g. a double fault caused by a
//! stack overflow, where the normal stack is unusable) - and RSP0, the
//! kernel stack the CPU switches to when an interrupt arrives in ring 3.
//!
//! The order of the user segments is dictated by `sysret`: it loads
//! SS = STAR.base + 8 and CS = STAR.base + 16 (see `syscall.rs`), so user
//! data must come directly before user code.

use super::smp::{self, MAX_CPUS};
use core::arch::asm;
use core::mem::size_of;
use spin::Once;

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
/// Ring-3 selectors include the requested privilege level (RPL = 3).
pub const USER_DATA_SELECTOR: u16 = 0x18 | 3;
pub const USER_CODE_SELECTOR: u16 = 0x20 | 3;
pub const TSS_SELECTOR: u16 = 0x28;

/// IST index (1-based, as in the IDT entry) of the double fault stack.
pub const DOUBLE_FAULT_IST: u8 = 1;
const IST_STACK_SIZE: usize = 5 * 4096;

/// 64-bit TSS layout according to Intel SDM Vol. 3, 8.7.
#[repr(C, packed(4))]
pub struct TaskStateSegment {
    _reserved0: u32,
    /// Stacks for switching to ring 0/1/2; RSP0 = kernel stack of the running thread
    pub privilege_stacks: [u64; 3],
    _reserved1: u64,
    /// IST1..IST7 (Index 0 = IST1)
    pub interrupt_stacks: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    pub iomap_base: u16,
}

const _: () = assert!(size_of::<TaskStateSegment>() == 104);

impl TaskStateSegment {
    const fn new() -> Self {
        TaskStateSegment {
            _reserved0: 0,
            privilege_stacks: [0; 3],
            _reserved1: 0,
            interrupt_stacks: [0; 7],
            _reserved2: 0,
            _reserved3: 0,
            iomap_base: size_of::<TaskStateSegment>() as u16, // no I/O permission bitmap
        }
    }
}

#[repr(C, align(16))]
struct Stack([u8; IST_STACK_SIZE]);

static mut DOUBLE_FAULT_STACK: Stack = Stack([0; IST_STACK_SIZE]);

/// Null, kernel code, kernel data, user data, user code, TSS (16 bytes = 2 entries)
const GDT_ENTRIES: usize = 7;

#[repr(C, align(8))]
pub struct Gdt([u64; GDT_ENTRIES]);

// Descriptor bits: P=present, S=code/data, E=executable, RW, L=64-bit, G=4K granularity
const KERNEL_CODE: u64 = 0x00AF_9A00_0000_FFFF; // P|DPL0|S|E|RW, L=1, G=1
const KERNEL_DATA: u64 = 0x00CF_9200_0000_FFFF; // P|DPL0|S|RW, D/B=1, G=1
const USER_DATA: u64 = 0x00CF_F200_0000_FFFF; // like kernel data, DPL3
const USER_CODE: u64 = 0x00AF_FA00_0000_FFFF; // like kernel code, DPL3

fn tss_descriptor(tss: &'static TaskStateSegment) -> (u64, u64) {
    let base = tss as *const _ as u64;
    let limit = (size_of::<TaskStateSegment>() - 1) as u64;
    let mut low = 0u64;
    low |= limit & 0xFFFF;
    low |= (base & 0xFF_FFFF) << 16;
    low |= 0x89 << 40; // P=1, DPL=0, type 0x9 = available 64-bit TSS
    low |= ((limit >> 16) & 0xF) << 48;
    low |= ((base >> 24) & 0xFF) << 56;
    let high = base >> 32;
    (low, high)
}

/// One TSS and one GDT per CPU (each CPU needs its own TSS: RSP0 and the
/// IST stacks differ, and a TSS descriptor is marked busy once loaded).
/// Not behind a lock: RSP0 is rewritten on every context switch (with
/// interrupts disabled) by the CPU that owns it, and the CPU reads it on its own.
static mut TSS: [TaskStateSegment; MAX_CPUS] = [const { TaskStateSegment::new() }; MAX_CPUS];
static GDT: [Once<Gdt>; MAX_CPUS] = [const { Once::new() }; MAX_CPUS];

fn tss(cpu: usize) -> &'static TaskStateSegment {
    unsafe { &*core::ptr::addr_of!(TSS[cpu]) }
}

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// GDT and TSS of the boot CPU (its double fault stack is static).
pub fn init() {
    // Stacks grow downwards -> store the end of the array
    let stack_top = core::ptr::addr_of!(DOUBLE_FAULT_STACK) as u64 + IST_STACK_SIZE as u64;
    init_cpu(0, stack_top);
}

/// Builds and loads GDT + TSS of CPU `cpu`, with its own double fault stack.
pub fn init_cpu(cpu: usize, double_fault_stack_top: u64) {
    let gdt = GDT[cpu].call_once(|| {
        unsafe {
            let tss = &mut *core::ptr::addr_of_mut!(TSS[cpu]);
            tss.interrupt_stacks[(DOUBLE_FAULT_IST - 1) as usize] = double_fault_stack_top;
        }
        let (tss_low, tss_high) = tss_descriptor(tss(cpu));
        Gdt([0, KERNEL_CODE, KERNEL_DATA, USER_DATA, USER_CODE, tss_low, tss_high])
    });

    let ptr = DescriptorTablePointer {
        limit: (size_of::<Gdt>() - 1) as u16,
        base: gdt as *const _ as u64,
    };

    unsafe {
        asm!("lgdt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
        // CS cannot be set with `mov` -> far return into the new segment
        asm!(
            "push {sel}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            sel = in(reg) KERNEL_CODE_SELECTOR as u64,
            tmp = lateout(reg) _,
            options(preserves_flags),
        );
        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            "mov ss, {0:x}",
            "mov fs, {0:x}",
            "mov gs, {0:x}",
            in(reg) KERNEL_DATA_SELECTOR,
            options(nostack, preserves_flags),
        );
        asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}

/// Top of this CPU's double fault stack (for tests).
pub fn double_fault_stack_top() -> u64 {
    tss(smp::cpu_index()).interrupt_stacks[(DOUBLE_FAULT_IST - 1) as usize]
}

/// Sets RSP0 of this CPU: the stack the CPU loads when an interrupt or
/// exception arrives while ring-3 code runs. Call with interrupts disabled.
pub fn set_kernel_stack(top: u64) {
    let cpu = smp::cpu_index();
    unsafe { core::ptr::addr_of_mut!(TSS[cpu].privilege_stacks).write_unaligned([top, 0, 0]) };
}

pub fn kernel_stack() -> u64 {
    let cpu = smp::cpu_index();
    unsafe { core::ptr::addr_of!(TSS[cpu].privilege_stacks).read_unaligned()[0] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_cs() -> u16 {
        let cs: u16;
        unsafe { asm!("mov {0:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags)) };
        cs
    }

    fn read_tr() -> u16 {
        let tr: u16;
        unsafe { asm!("str {0:x}", out(reg) tr, options(nomem, nostack, preserves_flags)) };
        tr
    }

    fn read_gdtr() -> (u16, u64) {
        let mut ptr = DescriptorTablePointer { limit: 0, base: 0 };
        unsafe { asm!("sgdt [{}]", in(reg) &mut ptr, options(nostack, preserves_flags)) };
        (ptr.limit, ptr.base)
    }

    #[test_case]
    fn segments_are_loaded() {
        assert_eq!(read_cs(), KERNEL_CODE_SELECTOR);
        assert_eq!(read_tr(), TSS_SELECTOR);
    }

    #[test_case]
    fn gdtr_points_to_our_gdt() {
        // interrupts off: the thread must not move to another CPU in between
        let (limit, base, cpu) = crate::arch::x86_64::without_interrupts(|| {
            let (l, b) = read_gdtr();
            (l, b, smp::cpu_index())
        });
        assert_eq!(limit as usize, size_of::<Gdt>() - 1);
        assert_eq!(base, GDT[cpu].get().unwrap() as *const _ as u64);
    }

    #[test_case]
    fn user_segments_have_dpl3() {
        let gdt = GDT[0].get().unwrap();
        for sel in [USER_DATA_SELECTOR, USER_CODE_SELECTOR] {
            let desc = gdt.0[(sel >> 3) as usize];
            assert_eq!((desc >> 45) & 3, 3, "selector {:#x} is not DPL3", sel);
        }
        // sysret: SS = base + 8, CS = base + 16 (base = kernel data selector)
        assert_eq!(USER_DATA_SELECTOR & !3, KERNEL_DATA_SELECTOR + 8);
        assert_eq!(USER_CODE_SELECTOR & !3, KERNEL_DATA_SELECTOR + 16);
    }

    #[test_case]
    fn double_fault_stack_is_set() {
        let top = double_fault_stack_top();
        assert!(top != 0);
        assert_eq!(top % 16, 0);
    }
}
