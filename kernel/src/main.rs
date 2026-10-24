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
    // Leave Limine's stack (no guard page) for our own guarded kernel stack
    let stack = kernel::mem::stack::alloc_stack(kernel::mem::stack::DEFAULT_STACK_PAGES)
        .expect("cannot allocate the kernel stack");
    unsafe { arch::x86_64::switch_stack(stack.top.as_u64(), kernel_main) }
}

/// Runs on the guarded kernel stack.
extern "C" fn kernel_main() -> ! {
    println!("KernelTry booting...");
    if let Some(console) = kernel::console::CONSOLE.lock().as_ref() {
        let (cols, rows) = console.size();
        kernel::serial_println!("Console: {}x{} characters", cols, rows);
    }
    println!("Hello from KernelTry! Umlaut check: äöüß");
    let mem = kernel::mem::pmm::stats();
    println!(
        "Memory: {} MiB usable, {} MiB free",
        mem.usable_bytes() / (1024 * 1024),
        mem.free_bytes() / (1024 * 1024)
    );
    println!("Uptime: {} ms, keyboard layout: {:?}", kernel::time::uptime_ms(), keyboard::layout());
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
