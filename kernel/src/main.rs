#![no_std]
#![no_main]

use core::panic::PanicInfo;
use kernel::drivers::keyboard;
use kernel::drivers::scancode::Key;
use kernel::{arch, print, println};

/// Entry point - Limine jumps here (see `ENTRY(kmain)` in linker.ld).
/// We are already in 64-bit long mode, paging is enabled, interrupts are off.
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    println!("KernelTry booting...");
    if let Some(console) = kernel::console::CONSOLE.lock().as_ref() {
        let (cols, rows) = console.size();
        kernel::serial_println!("Console: {}x{} characters", cols, rows);
    }
    println!("Hello from KernelTry! Umlaut check: äöüß");
    println!("Uptime: {} ms, Tastatur-Layout: {:?}", kernel::time::uptime_ms(), keyboard::layout());
    println!("Type something (echo mode, the shell arrives in M11):");
    print!("> ");

    // M7: simple echo mode until the shell exists
    loop {
        let event = keyboard::read_key();
        match event.key {
            Key::Char(c) => print!("{}", c),
            Key::Enter => print!("\n> "),
            Key::Backspace => print!("\x08"),
            Key::Tab => print!("\t"),
            other => kernel::serial_println!("[key: {:?}]", other),
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Not `println!` - the panic may have happened inside the console code
    // while its lock is still held.
    kernel::serial_println!("KERNEL PANIC: {}", info);
    arch::x86_64::halt_loop()
}
