#![no_std]
#![no_main]

mod arch;
mod boot;
mod qemu;
mod serial;

use core::panic::PanicInfo;

/// Einsprungpunkt – Limine springt hierher (siehe `ENTRY(kmain)` in linker.ld).
/// Wir laufen bereits im 64-bit Long Mode, Paging ist aktiv, Interrupts sind aus.
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    serial_println!("KernelTry booting...");

    if !boot::BASE_REVISION.is_supported() {
        serial_println!(
            "Limine unterstuetzt Base Revision nicht (aktiv: {:?})",
            boot::BASE_REVISION.actual_revision()
        );
        arch::x86_64::halt_loop();
    }

    if let Some(fb_resp) = boot::FRAMEBUFFER.response() {
        if let Some(fb) = fb_resp.framebuffers().first() {
            serial_println!("Framebuffer: {}x{} @ {} bpp", fb.width, fb.height, fb.bpp);
            draw_test_pattern(fb);
        }
    }

    serial_println!("Hello from KernelTry!");
    arch::x86_64::halt_loop()
}

/// M1: Ein paar farbige Balken als Lebenszeichen auf dem Bildschirm.
/// Echte Textausgabe kommt in M3.
fn draw_test_pattern(fb: &limine::framebuffer::Framebuffer) {
    if fb.bpp != 32 {
        return;
    }
    let colors: [u32; 4] = [0x00FF5555, 0x0055FF55, 0x005555FF, 0x00FFFFFF];
    let base = fb.address() as *mut u8;
    let band = (fb.height / colors.len() as u64).max(1);
    for y in 0..fb.height {
        let color = colors[((y / band) as usize).min(colors.len() - 1)];
        let row = unsafe { base.add((y * fb.pitch) as usize) as *mut u32 };
        for x in 0..fb.width {
            unsafe { row.add(x as usize).write_volatile(color) };
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("KERNEL PANIC: {}", info);
    arch::x86_64::halt_loop()
}
