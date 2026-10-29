//! Ending QEMU from inside the kernel, with an exit status: the `isa-debug-exit` device.
//! QEMU only has it when started with `-device isa-debug-exit,iobase=0xf4,iosize=0x04` (see run-qemu.sh).
//! A 32-bit write of `value` to port 0xF4 ends QEMU with exit status `(value << 1) | 1`.

#[derive(Clone, Copy)]
#[repr(u32)]
pub enum ExitCode {
    Success = 0x10, // QEMU exits with 33
    Failed = 0x11,  // QEMU exits with 35
}

pub fn exit(code: ExitCode) -> ! {
    unsafe { core::arch::asm!("out dx, eax", in("dx") 0xF4u16, in("eax") code as u32) };
    // Without the device (a real PC), the write does nothing: just stop
    crate::halt()
}
