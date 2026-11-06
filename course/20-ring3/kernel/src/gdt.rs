//! Our own Global Descriptor Table (GDT) and Task State Segment (TSS).
//!
//! Segments are a leftover of 16- and 32-bit x86: in 64-bit mode base and limit are ignored, but the CPU still
//! wants a valid code segment (it says "this is 64-bit code" and "this is the kernel, ring 0") and a data segment.
//! The part that really matters today is the TSS: it holds the Interrupt Stack Table (IST) - stacks the CPU
//! switches to for chosen exceptions. That is how a kernel survives a stack overflow (see interrupts.rs).
//!
//! Limine gave us a GDT too, but it lives in bootloader memory we will want back later - and we want to know
//! exactly what is in it.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use spin::Once;

/// A selector is the byte offset of a descriptor in the GDT (plus 2 bits of privilege level, 0 here).
pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const TSS_SELECTOR: u16 = 0x18;
/// Ring 3 (chapter 20): data at 0x28, code at 0x30 - the low 2 bits of a selector are the privilege level it asks
/// for, so code running in ring 3 uses 0x2B and 0x33. (Data before code is the order `sysret` expects, should a
/// later chapter use it.)
pub const USER_DATA_SELECTOR: u16 = 0x28 | 3;
pub const USER_CODE_SELECTOR: u16 = 0x30 | 3;

/// Index of the double fault stack in the IST, counted from 1 as in the gates of the IDT (idt.rs).
pub const DOUBLE_FAULT_IST: u8 = 1;
const STACK_SIZE: usize = 5 * 4096;

/// The 64-bit TSS (Intel SDM Vol. 3A, section 8.7): 104 bytes, some of its fields not 8-byte aligned.
#[repr(C, packed(4))]
pub struct TaskStateSegment {
    _reserved0: u32,
    /// RSP0..RSP2: stacks for switching from user mode to the kernel (a later chapter).
    pub privilege_stacks: [u64; 3],
    _reserved1: u64,
    /// IST1..IST7 - index 0 here is IST1.
    pub interrupt_stacks: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    /// Offset of the I/O permission bitmap; pointing past the end means "there is none".
    pub iomap_base: u16,
}

const _: () = assert!(size_of::<TaskStateSegment>() == 104);

/// The stack the CPU switches to on a double fault. `static mut` only so that it lands in writable memory:
/// no Rust code ever reads or writes it, only the CPU does.
#[repr(C, align(16))]
struct Stack([u8; STACK_SIZE]);
static mut DOUBLE_FAULT_STACK: Stack = Stack([0; STACK_SIZE]);

// Code and data descriptors: P = present, DPL 0 = kernel, S = code/data, E = executable, RW, L = 64-bit code,
// G = limit in 4 KiB units. Base and limit (all 0 / all 1) do not matter in 64-bit mode.
const KERNEL_CODE: u64 = 0x00AF_9A00_0000_FFFF; // P | DPL0 | S | E | RW, L = 1, G = 1
const KERNEL_DATA: u64 = 0x00CF_9200_0000_FFFF; // P | DPL0 | S | RW, D/B = 1, G = 1
// The same for ring 3: only the DPL (bits 45-46) differs - 0xF2 / 0xFA instead of 0x92 / 0x9A.
const USER_DATA: u64 = 0x00CF_F200_0000_FFFF; // P | DPL3 | S | RW
const USER_CODE: u64 = 0x00AF_FA00_0000_FFFF; // P | DPL3 | S | E | RW, L = 1

/// A TSS descriptor is 16 bytes (two GDT entries): base and limit scattered over several fields.
fn tss_descriptor(base: u64) -> (u64, u64) {
    let limit = (size_of::<TaskStateSegment>() - 1) as u64;
    let mut low = limit & 0xFFFF;
    low |= (base & 0xFF_FFFF) << 16; // base bits 0..24
    low |= 0x89 << 40; // P = 1, DPL 0, type 0x9 = available 64-bit TSS
    low |= ((limit >> 16) & 0xF) << 48;
    low |= ((base >> 24) & 0xFF) << 56; // base bits 24..32
    (low, base >> 32) // the second entry holds base bits 32..64
}

/// Null descriptor, kernel code, kernel data, TSS (2 entries), user data, user code.
#[repr(C, align(8))]
pub struct Gdt([u64; 7]);

/// A TSS the kernel changes while the CPU uses it: since chapter 20, `set_kernel_stack` writes RSP0 at every thread
/// switch. Writing through a shared reference needs an `UnsafeCell`; only the CPU that owns a TSS writes it.
pub struct Tss(UnsafeCell<TaskStateSegment>);
unsafe impl Sync for Tss {}

impl Tss {
    fn get(&self) -> *mut TaskStateSegment {
        self.0.get()
    }
}

static TSS: Once<Tss> = Once::new();
static GDT: Once<Gdt> = Once::new();
/// Each CPU's TSS, by CPU number.
static TSS_OF: [AtomicPtr<TaskStateSegment>; crate::smp::MAX_CPUS] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; crate::smp::MAX_CPUS];

/// What `lgdt` loads and `sgdt` stores: size - 1 and address of the table.
#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// A TSS whose double fault stack ends at `double_fault_stack_top`.
fn tss(double_fault_stack_top: u64) -> TaskStateSegment {
    let mut interrupt_stacks = [0; 7];
    interrupt_stacks[(DOUBLE_FAULT_IST - 1) as usize] = double_fault_stack_top;
    TaskStateSegment {
        _reserved0: 0,
        privilege_stacks: [0; 3],
        _reserved1: 0,
        interrupt_stacks,
        _reserved2: 0,
        _reserved3: 0,
        iomap_base: size_of::<TaskStateSegment>() as u16,
    }
}

/// The GDT for one CPU: the same code and data descriptors everywhere, its own TSS.
fn gdt(tss: &'static Tss) -> Gdt {
    let (tss_low, tss_high) = tss_descriptor(tss.get() as u64);
    Gdt([0, KERNEL_CODE, KERNEL_DATA, tss_low, tss_high, USER_DATA, USER_CODE])
}

/// Builds GDT and TSS of the boot CPU and makes it use them.
pub fn init() {
    // stacks grow downwards: the CPU needs the address just past the end
    let tss = TSS.call_once(|| {
        Tss(UnsafeCell::new(tss(
            (&raw const DOUBLE_FAULT_STACK) as u64 + STACK_SIZE as u64
        )))
    });
    TSS_OF[0].store(tss.get(), Ordering::Relaxed);
    load(GDT.call_once(|| gdt(tss)));
}

/// GDT and TSS for one of the other CPUs (chapter 14). Each needs a TSS of its own: loading a TSS marks it *busy* in
/// its descriptor, and the CPU refuses to load a busy one - and each needs its own double fault stack. They live on
/// the heap, for ever.
pub fn init_other_cpu(cpu: usize, double_fault_stack_top: u64) {
    let tss: &'static Tss = alloc::boxed::Box::leak(alloc::boxed::Box::new(Tss(UnsafeCell::new(tss(
        double_fault_stack_top,
    )))));
    TSS_OF[cpu].store(tss.get(), Ordering::Relaxed);
    OTHER_DOUBLE_FAULT_STACKS[cpu].store(double_fault_stack_top, Ordering::Relaxed);
    load(alloc::boxed::Box::leak(alloc::boxed::Box::new(gdt(tss))));
}

/// The top of each other CPU's double fault stack (index = CPU number; 0, the BSP, uses `DOUBLE_FAULT_STACK`).
static OTHER_DOUBLE_FAULT_STACKS: [AtomicU64; crate::smp::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::smp::MAX_CPUS];

fn load(gdt: &'static Gdt) {
    let pointer = DescriptorTablePointer {
        limit: (size_of::<Gdt>() - 1) as u16,
        base: gdt as *const _ as u64,
    };
    unsafe {
        asm!("lgdt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags));
        // CS cannot be loaded with `mov`: a far return pops a new RIP and CS - to the very next instruction
        asm!(
            "push {selector}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            selector = in(reg) KERNEL_CODE_SELECTOR as u64,
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
        // the task register: which GDT entry is the TSS
        asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}

/// Top of the double fault stack, as stored in the TSS.
pub fn double_fault_stack_top() -> u64 {
    let tss = TSS.get().expect("gdt::init ran").get();
    // a copy of the field: it is not aligned in the packed struct, so no reference to it
    let stacks = unsafe { (*tss).interrupt_stacks };
    stacks[(DOUBLE_FAULT_IST - 1) as usize]
}

/// RSP0 of this CPU's TSS: the stack the CPU switches to when an interrupt or a system call comes from ring 3
/// (chapter 20). The scheduler sets it to the stack of every thread it switches to - each thread needs its own,
/// and a thread may run on any CPU. Called with interrupts off.
pub fn set_kernel_stack(top: u64) {
    let tss = TSS_OF[crate::smp::cpu()].load(Ordering::Relaxed);
    if tss.is_null() {
        return; // a CPU that has no TSS yet runs no thread from ring 3 either
    }
    unsafe {
        let mut stacks = (*tss).privilege_stacks;
        stacks[0] = top;
        (*tss).privilege_stacks = stacks;
    }
}

/// RSP0 of this CPU's TSS (the tests).
#[cfg(test)]
pub fn kernel_stack() -> u64 {
    let stacks = unsafe { (*TSS_OF[crate::smp::cpu()].load(Ordering::Relaxed)).privilege_stacks };
    stacks[0]
}

/// Whether `address` lies inside the double fault stack of the CPU this runs on - the double fault handler checks
/// its own stack pointer. Since chapter 15 a thread can be on any CPU when it faults.
pub fn on_double_fault_stack(address: u64) -> bool {
    let top = match crate::smp::cpu() {
        0 => double_fault_stack_top(),
        cpu => OTHER_DOUBLE_FAULT_STACKS[cpu].load(Ordering::Relaxed),
    };
    (top - STACK_SIZE as u64..top).contains(&address)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_cs() -> u16 {
        let cs: u16;
        unsafe { asm!("mov {0:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags)) };
        cs
    }

    fn read_ss() -> u16 {
        let ss: u16;
        unsafe { asm!("mov {0:x}, ss", out(reg) ss, options(nomem, nostack, preserves_flags)) };
        ss
    }

    fn read_tr() -> u16 {
        let tr: u16;
        unsafe { asm!("str {0:x}", out(reg) tr, options(nomem, nostack, preserves_flags)) };
        tr
    }

    #[test_case]
    fn our_segments_are_loaded() {
        assert_eq!(read_cs(), KERNEL_CODE_SELECTOR);
        assert_eq!(read_ss(), KERNEL_DATA_SELECTOR);
        assert_eq!(read_tr(), TSS_SELECTOR);
    }

    #[test_case]
    fn gdtr_points_to_our_table() {
        // Each CPU has a GDT of its own, and since chapter 15 the test thread may run on any of them - so each CPU
        // is asked itself, with `run_on` (bug #31: asked from wherever the test ran, this failed on another CPU).
        let gdtr = || {
            let mut pointer = DescriptorTablePointer { limit: 0, base: 0 };
            unsafe { asm!("sgdt [{}]", in(reg) &mut pointer, options(nostack, preserves_flags)) };
            (pointer.limit, pointer.base) // copies: fields of a packed struct
        };
        let tables: alloc::vec::Vec<(u16, u64)> = (0..crate::smp::online())
            .map(|cpu| crate::smp::run_on(cpu, gdtr).expect("the CPU runs"))
            .collect();
        assert_eq!(tables[0].1, GDT.get().unwrap() as *const _ as u64, "CPU 0 uses the static GDT");
        for (cpu, &(limit, base)) in tables.iter().enumerate() {
            assert_eq!(limit as usize, 7 * 8 - 1, "CPU {}", cpu);
            assert_eq!(tables.iter().filter(|t| t.1 == base).count(), 1, "CPU {} shares its GDT", cpu);
        }
    }

    #[test_case]
    fn tss_descriptor_scatters_the_base() {
        let (low, high) = tss_descriptor(0xFFFF_FFFF_8012_3456);
        assert_eq!(low & 0xFFFF, 103); // limit = size - 1
        assert_eq!((low >> 16) & 0xFF_FFFF, 0x12_3456);
        assert_eq!((low >> 56) & 0xFF, 0x80);
        assert_eq!(high, 0xFFFF_FFFF);
        assert_eq!((low >> 40) & 0xFF, 0x89); // present, available 64-bit TSS
    }

    #[test_case]
    fn the_user_segments_have_privilege_level_3() {
        let gdt = &GDT.get().unwrap().0;
        let dpl = |descriptor: u64| (descriptor >> 45) & 3;
        assert_eq!(dpl(gdt[(USER_DATA_SELECTOR >> 3) as usize]), 3);
        assert_eq!(dpl(gdt[(USER_CODE_SELECTOR >> 3) as usize]), 3);
        assert_eq!(gdt[(USER_CODE_SELECTOR >> 3) as usize] >> 53 & 1, 1, "L: 64-bit code");
        assert_eq!(dpl(gdt[(KERNEL_CODE_SELECTOR >> 3) as usize]), 0);
    }

    #[test_case]
    fn the_double_fault_stack_is_in_the_tss() {
        let top = double_fault_stack_top();
        let bottom = (&raw const DOUBLE_FAULT_STACK) as u64;
        assert_eq!(top, bottom + STACK_SIZE as u64);
        assert_eq!(top % 16, 0); // the x86-64 ABI wants 16-byte aligned stacks
    }
}
