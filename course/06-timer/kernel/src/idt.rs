//! The Interrupt Descriptor Table (IDT): 256 entries, one per interrupt vector. Each entry - a *gate* - tells the
//! CPU where to jump for that vector, with which code segment, and whether to switch to an IST stack first.
//!
//! Vectors 0-31 are the CPU's exceptions (division by zero, page fault, ...); 32-255 are free for hardware
//! interrupts and software (the next chapters).

use crate::gdt::KERNEL_CODE_SELECTOR;
use core::arch::asm;
use core::fmt;
use core::mem::size_of;

/// What the CPU pushes onto the stack before it calls a handler, lowest address first.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct InterruptStackFrame {
    /// Where the interrupted code was - for most faults the instruction that failed, so returning retries it.
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl fmt::Debug for InterruptStackFrame {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "rip={:#018x} cs={:#x} rflags={:#x} rsp={:#018x} ss={:#x}",
            self.rip, self.cs, self.rflags, self.rsp, self.ss
        )
    }
}

/// The three kinds of handler the CPU can call. Some exceptions push an error code after the frame, and some can
/// never be returned from - the handler must say which, or the stack is read wrongly.
pub type Handler = extern "x86-interrupt" fn(InterruptStackFrame);
pub type HandlerWithErrorCode = extern "x86-interrupt" fn(InterruptStackFrame, u64);
pub type DivergingHandlerWithErrorCode = extern "x86-interrupt" fn(InterruptStackFrame, u64) -> !;

/// A 16-byte gate (Intel SDM Vol. 3A, section 6.14.1): like the TSS descriptor, the address is cut into pieces.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Entry {
    offset_low: u16,
    selector: u16,
    /// Bits 0..3: which IST stack to switch to, 1..=7 (0 = stay on the current stack).
    ist: u8,
    /// P | DPL | 0 | type; 0xE = interrupt gate: the CPU turns interrupts off while the handler runs.
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

const _: () = assert!(size_of::<Entry>() == 16);

const PRESENT_INTERRUPT_GATE: u8 = 0x8E; // P = 1, DPL 0, type 0xE

impl Entry {
    /// A gate that is not present: if its vector fires, the CPU raises a general protection fault instead.
    const fn missing() -> Entry {
        Entry {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            _reserved: 0,
        }
    }

    fn set_address(&mut self, address: u64) -> &mut Entry {
        self.offset_low = address as u16;
        self.offset_mid = (address >> 16) as u16;
        self.offset_high = (address >> 32) as u32;
        self.selector = KERNEL_CODE_SELECTOR;
        self.type_attr = PRESENT_INTERRUPT_GATE;
        self
    }

    /// The CPU switches to IST stack `index` (1..=7, see the TSS) before it pushes anything.
    pub fn set_ist(&mut self, index: u8) -> &mut Entry {
        assert!((1..=7).contains(&index));
        self.ist = index;
        self
    }

    // reading a gate back - only the tests need it
    #[cfg(test)]
    pub fn is_present(&self) -> bool {
        self.type_attr & 0x80 != 0
    }

    #[cfg(test)]
    pub fn ist(&self) -> u8 {
        self.ist
    }

    #[cfg(test)]
    pub fn address(&self) -> u64 {
        self.offset_low as u64 | (self.offset_mid as u64) << 16 | (self.offset_high as u64) << 32
    }
}

#[repr(C, align(16))]
pub struct Idt {
    entries: [Entry; 256],
}

impl Idt {
    pub const fn new() -> Idt {
        Idt {
            entries: [Entry::missing(); 256],
        }
    }

    pub fn set(&mut self, vector: u8, handler: Handler) -> &mut Entry {
        self.entries[vector as usize].set_address(handler as usize as u64)
    }

    pub fn set_with_error_code(&mut self, vector: u8, handler: HandlerWithErrorCode) -> &mut Entry {
        self.entries[vector as usize].set_address(handler as usize as u64)
    }

    pub fn set_diverging_with_error_code(&mut self, vector: u8, handler: DivergingHandlerWithErrorCode) -> &mut Entry {
        self.entries[vector as usize].set_address(handler as usize as u64)
    }

    #[cfg(test)]
    pub fn entry(&self, vector: u8) -> &Entry {
        &self.entries[vector as usize]
    }

    /// Makes the CPU use this table. `'static` because it keeps using it for as long as the kernel runs.
    pub fn load(&'static self) {
        let pointer = DescriptorTablePointer {
            limit: (size_of::<Idt>() - 1) as u16,
            base: self as *const _ as u64,
        };
        unsafe { asm!("lidt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags)) };
    }
}

/// What `lidt` loads and `sidt` stores - the same layout as for the GDT.
#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// On a page fault, control register CR2 holds the address the code tried to access.
pub fn read_cr2() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn a_gate_scatters_the_handler_address() {
        let mut entry = Entry::missing();
        entry.set_address(0xFFFF_FFFF_8012_3456);
        assert_eq!(entry.offset_low, 0x3456);
        assert_eq!(entry.offset_mid, 0x8012);
        assert_eq!(entry.offset_high, 0xFFFF_FFFF);
        assert_eq!(entry.address(), 0xFFFF_FFFF_8012_3456);
        assert_eq!(entry.selector, KERNEL_CODE_SELECTOR);
        assert!(entry.is_present() && entry.ist() == 0);
    }

    #[test_case]
    fn idtr_points_to_our_table() {
        let mut pointer = DescriptorTablePointer { limit: 0, base: 0 };
        unsafe { asm!("sidt [{}]", in(reg) &mut pointer, options(nostack, preserves_flags)) };
        let (limit, base) = (pointer.limit, pointer.base); // copies: fields of a packed struct
        assert_eq!(base, crate::interrupts::idt() as *const _ as u64);
        assert_eq!(limit as usize, 256 * 16 - 1);
    }
}
