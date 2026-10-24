#![no_std]
#![no_main]

use core::panic::PanicInfo;
use kernel::{arch, println};

/// Einsprungpunkt – Limine springt hierher (siehe `ENTRY(kmain)` in linker.ld).
/// Wir laufen bereits im 64-bit Long Mode, Paging ist aktiv, Interrupts sind aus.
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    println!("KernelTry booting...");
    if let Some(console) = kernel::console::CONSOLE.lock().as_ref() {
        let (cols, rows) = console.size();
        kernel::serial_println!("Konsole: {}x{} Zeichen", cols, rows);
    }
    println!("Hello from KernelTry! Grüße aus dem Kernel. äöüß");
    arch::x86_64::halt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Nicht `println!` – der Panic könnte mitten in der Konsole passiert sein
    // und der Lock wäre noch gehalten.
    kernel::serial_println!("KERNEL PANIC: {}", info);
    arch::x86_64::halt_loop()
}
