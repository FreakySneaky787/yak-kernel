//! The two 8259 PICs (programmable interrupt controllers) - the classic way hardware interrupts reach the CPU.
//!
//! Each chip has 8 input lines, IRQ 0-7; the second ("slave") is wired to IRQ 2 of the first ("master"), so there
//! are 16 IRQs. When a device raises its line, the PIC hands the CPU a vector number: the chip's *offset* plus the
//! line. After a reset the master's offset is 8 - IRQ 0, the timer, would arrive as vector 8, the double fault.
//! So the first thing we do is move both chips to 32-47, just past the CPU's exceptions.

use crate::port::{inb, outb};

pub const MASTER_OFFSET: u8 = 32;
pub const SLAVE_OFFSET: u8 = MASTER_OFFSET + 8;

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xA0;
const SLAVE_DATA: u16 = 0xA1;

const END_OF_INTERRUPT: u8 = 0x20;

/// The vector an IRQ arrives on.
pub const fn vector(irq: u8) -> u8 {
    MASTER_OFFSET + irq
}

/// The chips are old and slow: a write to the unused port 0x80 gives them about a microsecond between commands.
fn io_wait() {
    unsafe { outb(0x80, 0) };
}

/// Moves both PICs to vectors 32-47 and masks every IRQ; `unmask` lets single ones through.
pub fn init() {
    // the four "initialization command words" (ICW), always in this order
    let commands = [
        (MASTER_COMMAND, 0x11),       // ICW1 master: start initialization, ICW4 follows
        (SLAVE_COMMAND, 0x11),        // ICW1 slave
        (MASTER_DATA, MASTER_OFFSET), // ICW2: the vector of IRQ 0
        (SLAVE_DATA, SLAVE_OFFSET),   //       the vector of IRQ 8
        (MASTER_DATA, 1 << 2),        // ICW3 master: a slave hangs on line 2
        (SLAVE_DATA, 2),              // ICW3 slave: you hang on line 2
        (MASTER_DATA, 0x01),          // ICW4: 8086 mode
        (SLAVE_DATA, 0x01),
    ];
    for (port, value) in commands {
        unsafe { outb(port, value) };
        io_wait();
    }
    // mask register: a set bit blocks that IRQ
    unsafe {
        outb(MASTER_DATA, 0xFF);
        outb(SLAVE_DATA, 0xFF);
    }
}

/// Lets `irq` through. An IRQ of the slave also needs the master's line 2 open.
pub fn unmask(irq: u8) {
    let (port, line) = if irq < 8 {
        (MASTER_DATA, irq)
    } else {
        (SLAVE_DATA, irq - 8)
    };
    unsafe { outb(port, inb(port) & !(1 << line)) };
    if irq >= 8 {
        unmask(2);
    }
}

/// The masks of (master, slave): a set bit means blocked - for the tests.
#[cfg(test)]
pub fn masks() -> (u8, u8) {
    unsafe { (inb(MASTER_DATA), inb(SLAVE_DATA)) }
}

/// Tells the PIC that the handler of `irq` is done. Until then it delivers no further IRQ of the same or a lower
/// priority. An IRQ of the slave went through both chips, so both get the message.
pub fn end_of_interrupt(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(SLAVE_COMMAND, END_OF_INTERRUPT);
        }
        outb(MASTER_COMMAND, END_OF_INTERRUPT);
    }
}

/// The in-service register of (master, slave): which IRQ handlers the PIC thinks are running right now.
pub fn in_service() -> (u8, u8) {
    const READ_ISR: u8 = 0x0B;
    unsafe {
        outb(MASTER_COMMAND, READ_ISR);
        outb(SLAVE_COMMAND, READ_ISR);
        (inb(MASTER_COMMAND), inb(SLAVE_COMMAND))
    }
}

/// Whether the CPU has a local APIC: CPUID leaf 1, EDX bit 9.
///
/// Every x86 core since the late 1990s has a *local APIC*, a newer interrupt controller between the PIC and the
/// CPU; the PIC's signal enters it on its LINT0 pin. Limine leaves LINT0 masked, so with an APIC no PIC interrupt
/// would ever arrive. `run-qemu.sh` therefore starts QEMU with a CPU without one (`-cpu qemu64,-apic`): the PIC is
/// wired straight to the CPU, as on a PC of 1990. The APIC comes back in a later chapter.
#[cfg(test)]
pub fn cpu_has_local_apic() -> bool {
    let edx: u32;
    // rbx is reserved by the compiler, so cpuid's EBX output is saved around the instruction
    unsafe {
        core::arch::asm!(
            "mov {tmp:r}, rbx", "cpuid", "mov rbx, {tmp:r}",
            tmp = out(reg) _, inout("eax") 1u32 => _, out("ecx") _, out("edx") edx,
            options(nomem, nostack, preserves_flags),
        )
    };
    edx & (1 << 9) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn the_cpu_has_no_local_apic() {
        assert!(!cpu_has_local_apic(), "run-qemu.sh starts QEMU with -cpu qemu64,-apic");
    }

    #[test_case]
    fn only_the_timer_is_let_through() {
        assert_eq!(masks(), (0b1111_1110, 0b1111_1111));
    }

    #[test_case]
    fn irqs_start_after_the_exceptions() {
        assert_eq!(vector(0), 32);
        assert_eq!(vector(15), 47);
    }
}
