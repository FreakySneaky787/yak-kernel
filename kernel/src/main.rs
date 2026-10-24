#![no_std]
#![no_main]

use core::panic::PanicInfo;
use kernel::drivers::keyboard;
use kernel::drivers::scancode::Key;
use kernel::{arch, print, println};

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
    println!("Uptime: {} ms, Tastatur-Layout: {:?}", kernel::time::uptime_ms(), keyboard::layout());
    println!("Tippe etwas (Echo-Modus, die Shell kommt in M11):");
    print!("> ");

    // M7: einfacher Echo-Modus, bis die Shell kommt
    loop {
        let event = keyboard::read_key();
        match event.key {
            Key::Char(c) => print!("{}", c),
            Key::Enter => print!("\n> "),
            Key::Backspace => print!("\x08"),
            Key::Tab => print!("\t"),
            other => kernel::serial_println!("[Taste: {:?}]", other),
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Nicht `println!` – der Panic könnte mitten in der Konsole passiert sein
    // und der Lock wäre noch gehalten.
    kernel::serial_println!("KERNEL PANIC: {}", info);
    arch::x86_64::halt_loop()
}
