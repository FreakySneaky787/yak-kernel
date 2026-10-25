//! Integration test: reclaiming the bootloader's memory. Afterwards that
//! memory is handed out and overwritten - the kernel (page tables, all CPUs,
//! threads, user processes) must keep working.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use kernel::mem::{PhysAddr, pmm, stack};
use kernel::process::{self, ExitStatus, Output};
use kernel::qemu::{ExitCode, exit};
use kernel::{arch, fs, serial_println, task};

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    // like the real kernel: leave Limine's stack first
    let stack = stack::alloc_stack(stack::DEFAULT_STACK_PAGES).unwrap();
    unsafe { arch::x86_64::switch_stack(stack.top.as_u64(), after_switch) }
}

extern "C" fn after_switch() -> ! {
    serial_println!("reclaim::bootloader_memory_is_reused ... ");
    let regions = pmm::bootloader_regions();
    let before = pmm::stats();
    let gained = kernel::mem::reclaim_bootloader_memory();
    let after = pmm::stats();
    serial_println!("  reclaimed {} KiB in {} regions", gained / 1024, regions.len());
    assert!(gained >= 1024 * 1024, "less than 1 MiB reclaimed");
    assert!(after.usable_frames > before.usable_frames);

    // Take every free frame, so the old bootloader frames are certainly among
    // them, and fill them with a pattern (this would overwrite stale page tables).
    let inside = |f: PhysAddr| {
        regions
            .iter()
            .any(|r| f.as_u64() >= r.base && f.as_u64() < r.base + r.length)
    };
    let mut frames = Vec::new();
    let mut reused = 0;
    while let Some(f) = pmm::alloc_frame() {
        unsafe { core::ptr::write_bytes(f.to_virt().as_mut_ptr::<u8>(), 0xA5, 4096) };
        reused += inside(f) as usize;
        frames.push(f);
        if frames.len() > 100_000 {
            break;
        }
    }
    serial_println!(
        "  overwrote {} frames, {} of them former bootloader memory",
        frames.len(),
        reused
    );
    assert!(reused > 100, "the reclaimed frames were not handed out");
    for f in frames {
        pmm::free_frame(f);
    }

    // threads on all CPUs, then a user process loaded from the disk
    let ids: Vec<_> = (0..8)
        .map(|i| {
            task::spawn("after-reclaim", move || {
                let v: Vec<u64> = (0..10_000).map(|x| x * i).collect();
                assert_eq!(v[9_999], 9_999 * i);
            })
        })
        .collect();
    for id in ids {
        task::join(id);
    }
    let image = fs::with_root(|fs| fs.read_file("/bin/fault")).unwrap();
    let status = process::run("fault", &image, "exit\x009", Output::Console).unwrap();
    assert_eq!(status, ExitStatus::Exited(9));
    serial_println!("[ok]");
    exit(ExitCode::Success)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::testing::test_panic_handler(info)
}
