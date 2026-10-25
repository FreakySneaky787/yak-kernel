//! Handlers for CPU exceptions (vectors 0-31) and construction of the kernel IDT.
//!
//! An exception raised by user code (CS has RPL 3) is the program's fault:
//! the process is killed and the kernel carries on. In kernel code it is a
//! bug and the kernel panics.

use super::gdt::DOUBLE_FAULT_IST;
use super::idt::{Idt, InterruptStackFrame, read_cr2};
use crate::serial_println;
use alloc::format;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Once;

pub mod vector {
    pub const DIVIDE_ERROR: u8 = 0;
    pub const DEBUG: u8 = 1;
    pub const NMI: u8 = 2;
    pub const BREAKPOINT: u8 = 3;
    pub const OVERFLOW: u8 = 4;
    pub const BOUND_RANGE: u8 = 5;
    pub const INVALID_OPCODE: u8 = 6;
    pub const DEVICE_NOT_AVAILABLE: u8 = 7;
    pub const DOUBLE_FAULT: u8 = 8;
    pub const INVALID_TSS: u8 = 10;
    pub const SEGMENT_NOT_PRESENT: u8 = 11;
    pub const STACK_SEGMENT_FAULT: u8 = 12;
    pub const GENERAL_PROTECTION: u8 = 13;
    pub const PAGE_FAULT: u8 = 14;
    pub const X87_FLOATING_POINT: u8 = 16;
    pub const ALIGNMENT_CHECK: u8 = 17;
    pub const MACHINE_CHECK: u8 = 18;
    pub const SIMD_FLOATING_POINT: u8 = 19;
    pub const VIRTUALIZATION: u8 = 20;
    pub const CONTROL_PROTECTION: u8 = 21;
}

static IDT: Once<Idt> = Once::new();

/// How often `int3` was handled (for tests and statistics).
pub static BREAKPOINT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Installs all exception handlers into `idt`. Separate from `init` so that
/// later modules (IRQs) and tests can build on it.
pub fn install_exception_handlers(idt: &mut Idt) {
    use vector::*;
    idt.set(DIVIDE_ERROR, divide_error);
    idt.set(DEBUG, debug);
    idt.set(NMI, nmi);
    idt.set(BREAKPOINT, breakpoint);
    idt.set(OVERFLOW, overflow);
    idt.set(BOUND_RANGE, bound_range);
    idt.set(INVALID_OPCODE, invalid_opcode);
    idt.set(DEVICE_NOT_AVAILABLE, device_not_available);
    idt.set_diverging_with_error_code(DOUBLE_FAULT, double_fault)
        .set_ist(DOUBLE_FAULT_IST);
    idt.set_with_error_code(INVALID_TSS, invalid_tss);
    idt.set_with_error_code(SEGMENT_NOT_PRESENT, segment_not_present);
    idt.set_with_error_code(STACK_SEGMENT_FAULT, stack_segment_fault);
    idt.set_with_error_code(GENERAL_PROTECTION, general_protection);
    idt.set_with_error_code(PAGE_FAULT, page_fault);
    idt.set(X87_FLOATING_POINT, x87_floating_point);
    idt.set_with_error_code(ALIGNMENT_CHECK, alignment_check);
    idt.set_diverging(MACHINE_CHECK, machine_check);
    idt.set(SIMD_FLOATING_POINT, simd_floating_point);
    idt.set(VIRTUALIZATION, virtualization);
    idt.set_with_error_code(CONTROL_PROTECTION, control_protection);
}

/// Builds the kernel IDT (once) and loads it.
pub fn init() {
    let idt = IDT.call_once(|| {
        let mut idt = Idt::new();
        install_exception_handlers(&mut idt);
        super::irq::install_irq_handlers(&mut idt);
        idt
    });
    idt.load();
}

pub fn kernel_idt() -> Option<&'static Idt> {
    IDT.get()
}

// ---- Handler ----------------------------------------------------------------

fn from_user(frame: &InterruptStackFrame) -> bool {
    frame.cs & 3 == 3
}

extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    BREAKPOINT_COUNT.fetch_add(1, Ordering::Relaxed);
    serial_println!("EXCEPTION: BREAKPOINT at {:#x}", frame.rip);
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, error_code: u64) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT (code {:#x})\n{:?}", error_code, frame);
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, error_code: u64) {
    if from_user(&frame) {
        if crate::process::handle_page_fault(read_cr2(), error_code) {
            return; // demand paging: retry the instruction
        }
        crate::process::kill_current(format!(
            "page fault at {:#x} ({}), rip={:#x}",
            read_cr2(),
            PageFaultError(error_code),
            frame.rip
        ));
    }
    panic!(
        "EXCEPTION: PAGE FAULT at address {:#018x}\n  cause: {}\n{:?}",
        read_cr2(),
        PageFaultError(error_code),
        frame
    );
}

extern "x86-interrupt" fn general_protection(frame: InterruptStackFrame, error_code: u64) {
    if from_user(&frame) {
        crate::process::kill_current(format!(
            "general protection fault (code {:#x}), rip={:#x}",
            error_code, frame.rip
        ));
    }
    panic!(
        "EXCEPTION: GENERAL PROTECTION FAULT (Selector/Code {:#x})\n{:?}",
        error_code, frame
    );
}

extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    panic!("EXCEPTION: MACHINE CHECK\n{:?}", frame);
}

/// Generates simple handlers that kill the process (user mode) or panic
/// (kernel mode) with their name (and error code, if any).
macro_rules! panicking_handler {
    ($name:ident, $text:literal) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame) {
            if from_user(&frame) {
                crate::process::kill_current(format!(concat!($text, ", rip={:#x}"), frame.rip).to_lowercase());
            }
            panic!(concat!("EXCEPTION: ", $text, "\n{:?}"), frame);
        }
    };
    ($name:ident, $text:literal, error_code) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame, error_code: u64) {
            if from_user(&frame) {
                crate::process::kill_current(
                    format!(concat!($text, " (code {:#x}), rip={:#x}"), error_code, frame.rip).to_lowercase(),
                );
            }
            panic!(
                concat!("EXCEPTION: ", $text, " (code {:#x})\n{:?}"),
                error_code, frame
            );
        }
    };
}

panicking_handler!(divide_error, "DIVIDE ERROR");
panicking_handler!(debug, "DEBUG");
panicking_handler!(nmi, "NON-MASKABLE INTERRUPT");
panicking_handler!(overflow, "OVERFLOW");
panicking_handler!(bound_range, "BOUND RANGE EXCEEDED");
panicking_handler!(invalid_opcode, "INVALID OPCODE");
panicking_handler!(device_not_available, "DEVICE NOT AVAILABLE");
panicking_handler!(x87_floating_point, "x87 FLOATING POINT");
panicking_handler!(simd_floating_point, "SIMD FLOATING POINT");
panicking_handler!(virtualization, "VIRTUALIZATION");
panicking_handler!(invalid_tss, "INVALID TSS", error_code);
panicking_handler!(segment_not_present, "SEGMENT NOT PRESENT", error_code);
panicking_handler!(stack_segment_fault, "STACK SEGMENT FAULT", error_code);
panicking_handler!(alignment_check, "ALIGNMENT CHECK", error_code);
panicking_handler!(control_protection, "CONTROL PROTECTION", error_code);

/// Human-readable page fault error code (Intel SDM Vol. 3, 4.7).
pub struct PageFaultError(pub u64);

impl core::fmt::Display for PageFaultError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let c = self.0;
        write!(
            f,
            "{}, {}, {}",
            if c & 1 != 0 {
                "protection violation"
            } else {
                "page not present"
            },
            if c & 2 != 0 { "write" } else { "read" },
            if c & 4 != 0 { "user mode" } else { "kernel mode" },
        )?;
        if c & 16 != 0 {
            write!(f, ", instruction fetch")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn breakpoint_returns() {
        let before = BREAKPOINT_COUNT.load(Ordering::Relaxed);
        unsafe { core::arch::asm!("int3") };
        assert_eq!(BREAKPOINT_COUNT.load(Ordering::Relaxed), before + 1);
    }

    #[test_case]
    fn exception_entries_present() {
        let idt = kernel_idt().unwrap();
        for v in [0u8, 3, 6, 8, 13, 14] {
            assert!(idt.entry(v).is_present(), "vector {} missing", v);
        }
        assert!(!idt.entry(9).is_present()); // reserved
    }

    #[test_case]
    fn page_fault_error_text() {
        use core::fmt::Write;
        struct Buf([u8; 128], usize);
        impl Write for Buf {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                self.0[self.1..self.1 + s.len()].copy_from_slice(s.as_bytes());
                self.1 += s.len();
                Ok(())
            }
        }
        let mut b = Buf([0; 128], 0);
        write!(b, "{}", PageFaultError(0b10)).unwrap();
        assert_eq!(
            core::str::from_utf8(&b.0[..b.1]).unwrap(),
            "page not present, write, kernel mode"
        );
    }
}
