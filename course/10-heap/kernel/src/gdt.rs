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
use core::mem::size_of;
use spin::Once;

/// A selector is the byte offset of a descriptor in the GDT (plus 2 bits of privilege level, 0 here).
pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const TSS_SELECTOR: u16 = 0x18;

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

/// Null descriptor, kernel code, kernel data, TSS (2 entries).
#[repr(C, align(8))]
pub struct Gdt([u64; 5]);

static TSS: Once<TaskStateSegment> = Once::new();
static GDT: Once<Gdt> = Once::new();

/// What `lgdt` loads and `sgdt` stores: size - 1 and address of the table.
#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// Builds GDT and TSS and makes the CPU use them.
pub fn init() {
    let tss = TSS.call_once(|| {
        let stack_bottom = (&raw const DOUBLE_FAULT_STACK) as u64;
        let mut interrupt_stacks = [0; 7];
        // stacks grow downwards: the CPU needs the address just past the end
        interrupt_stacks[(DOUBLE_FAULT_IST - 1) as usize] = stack_bottom + STACK_SIZE as u64;
        TaskStateSegment {
            _reserved0: 0,
            privilege_stacks: [0; 3],
            _reserved1: 0,
            interrupt_stacks,
            _reserved2: 0,
            _reserved3: 0,
            iomap_base: size_of::<TaskStateSegment>() as u16,
        }
    });
    let gdt = GDT.call_once(|| {
        let (tss_low, tss_high) = tss_descriptor(tss as *const _ as u64);
        Gdt([0, KERNEL_CODE, KERNEL_DATA, tss_low, tss_high])
    });
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
    TSS.get().expect("gdt::init ran").interrupt_stacks[(DOUBLE_FAULT_IST - 1) as usize]
}

/// Whether `address` lies inside the double fault stack - the double fault handler checks its own stack pointer.
pub fn on_double_fault_stack(address: u64) -> bool {
    let top = double_fault_stack_top();
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
        let mut pointer = DescriptorTablePointer { limit: 0, base: 0 };
        unsafe { asm!("sgdt [{}]", in(reg) &mut pointer, options(nostack, preserves_flags)) };
        let (limit, base) = (pointer.limit, pointer.base); // copies: fields of a packed struct
        assert_eq!(base, GDT.get().unwrap() as *const _ as u64);
        assert_eq!(limit as usize, 5 * 8 - 1);
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
    fn the_double_fault_stack_is_in_the_tss() {
        let top = double_fault_stack_top();
        let bottom = (&raw const DOUBLE_FAULT_STACK) as u64;
        assert_eq!(top, bottom + STACK_SIZE as u64);
        assert_eq!(top % 16, 0); // the x86-64 ABI wants 16-byte aligned stacks
    }
}
