//! Reboot and power off.

use super::port::{inb, outb, outw};

/// Resets the machine. Tries the keyboard controller first, then forces a
/// triple fault. (With QEMU's `-no-reboot`, a reset simply ends QEMU.)
pub fn reboot() -> ! {
    super::disable_interrupts();
    unsafe {
        // 8042 keyboard controller: wait until its input buffer is empty, then pulse the reset line
        for _ in 0..100_000 {
            if inb(0x64) & 0x02 == 0 {
                break;
            }
        }
        outb(0x64, 0xFE);
    }
    // Fallback: an empty IDT turns the next interrupt into a triple fault
    #[repr(C, packed)]
    struct Pointer {
        limit: u16,
        base: u64,
    }
    let empty = Pointer { limit: 0, base: 0 };
    unsafe { core::arch::asm!("lidt [{}]", "int3", in(reg) &empty, options(noreturn)) };
}

/// Powers the machine off via ACPI (QEMU's fixed PM1a control port), falling
/// back to QEMU's debug exit device.
pub fn poweroff() -> ! {
    super::disable_interrupts();
    unsafe {
        // SLP_TYPa = 0 (S5) | SLP_EN on QEMU's PM1a control block (q35 and newer i440fx: 0x604)
        outw(0x604, 0x2000);
        // Older QEMU / Bochs
        outw(0xB004, 0x2000);
    }
    crate::qemu::exit(crate::qemu::ExitCode::Success)
}
