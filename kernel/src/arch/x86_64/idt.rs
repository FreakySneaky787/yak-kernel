//! Interrupt Descriptor Table: 256 Einträge, einer pro Interrupt-Vektor.
//! Jeder Eintrag sagt der CPU, wohin sie bei diesem Vektor springen soll,
//! mit welchem Code-Segment und optional auf welchem IST-Stack.

use super::gdt::KERNEL_CODE_SELECTOR;
use core::arch::asm;
use core::fmt;
use core::mem::size_of;

/// Was die CPU beim Interrupt auf den Stack legt (in dieser Reihenfolge ab RSP).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct InterruptStackFrame {
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

pub type Handler = extern "x86-interrupt" fn(InterruptStackFrame);
pub type HandlerWithErrorCode = extern "x86-interrupt" fn(InterruptStackFrame, u64);
pub type DivergingHandlerWithErrorCode = extern "x86-interrupt" fn(InterruptStackFrame, u64) -> !;
pub type DivergingHandler = extern "x86-interrupt" fn(InterruptStackFrame) -> !;

/// Ein 16-Byte-Gate-Deskriptor (Intel SDM Vol. 3, 6.14.1).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Entry {
    offset_low: u16,
    selector: u16,
    /// Bits 0–2: IST-Index (0 = keiner)
    ist: u8,
    /// P | DPL | 0 | Typ (0xE = Interrupt Gate: IF wird beim Eintritt gelöscht)
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

const _: () = assert!(size_of::<Entry>() == 16);

const PRESENT_INTERRUPT_GATE: u8 = 0x8E;

impl Entry {
    const fn missing() -> Entry {
        Entry { offset_low: 0, selector: 0, ist: 0, type_attr: 0, offset_mid: 0, offset_high: 0, _reserved: 0 }
    }

    fn set_address(&mut self, addr: u64) -> &mut Entry {
        self.offset_low = addr as u16;
        self.offset_mid = (addr >> 16) as u16;
        self.offset_high = (addr >> 32) as u32;
        self.selector = KERNEL_CODE_SELECTOR;
        self.type_attr = PRESENT_INTERRUPT_GATE;
        self
    }

    /// Bei diesem Vektor auf IST-Stack `index` (1..=7) umschalten.
    pub fn set_ist(&mut self, index: u8) -> &mut Entry {
        assert!((1..=7).contains(&index));
        self.ist = index;
        self
    }

    pub fn is_present(&self) -> bool {
        self.type_attr & 0x80 != 0
    }

    pub fn address(&self) -> u64 {
        self.offset_low as u64 | (self.offset_mid as u64) << 16 | (self.offset_high as u64) << 32
    }
}

#[repr(C, align(16))]
pub struct Idt {
    entries: [Entry; 256],
}

impl Default for Idt {
    fn default() -> Self {
        Self::new()
    }
}

impl Idt {
    pub const fn new() -> Idt {
        Idt { entries: [Entry::missing(); 256] }
    }

    pub fn set(&mut self, vector: u8, handler: Handler) -> &mut Entry {
        self.entries[vector as usize].set_address(handler as usize as u64)
    }

    pub fn set_with_error_code(&mut self, vector: u8, handler: HandlerWithErrorCode) -> &mut Entry {
        self.entries[vector as usize].set_address(handler as usize as u64)
    }

    pub fn set_diverging(&mut self, vector: u8, handler: DivergingHandler) -> &mut Entry {
        self.entries[vector as usize].set_address(handler as usize as u64)
    }

    pub fn set_diverging_with_error_code(&mut self, vector: u8, handler: DivergingHandlerWithErrorCode) -> &mut Entry {
        self.entries[vector as usize].set_address(handler as usize as u64)
    }

    pub fn entry(&self, vector: u8) -> &Entry {
        &self.entries[vector as usize]
    }

    /// IDT in das IDTR-Register laden. `'static`, weil die CPU sie für immer benutzt.
    pub fn load(&'static self) {
        #[repr(C, packed)]
        struct Pointer {
            limit: u16,
            base: u64,
        }
        let ptr = Pointer { limit: (size_of::<Idt>() - 1) as u16, base: self as *const _ as u64 };
        unsafe { asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags)) };
    }
}

/// CR2 enthält bei einem Page Fault die Adresse, auf die zugegriffen wurde.
pub fn read_cr2() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}
