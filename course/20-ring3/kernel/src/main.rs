//! Chapter 20: the kernel of chapter 19 runs code in ring 3 - which may do nothing but compute, ask the kernel
//! through a system call, and be ended when it does something it may not.

#![no_std] // no standard library: it needs an operating system, and we are writing one
#![no_main]
// no `main`: nobody calls it - Limine jumps to `kmain` (see linker.ld: ENTRY(kmain))
// Tests without the standard test harness: the compiler collects `#[test_case]` functions and
// calls `testing::runner` with them from a generated function named `test_main`.
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]
// `extern "x86-interrupt" fn`: functions the CPU calls, not Rust code (they save every register and end with `iretq`)
#![feature(abi_x86_interrupt)]

// the part of Rust's standard library that needs only a heap: Box, Vec, String, format!, BTreeMap ...
extern crate alloc;

mod acpi;
mod ahci;
mod block;
mod console;
mod fat32;
mod font;
mod fs;
mod gdt;
mod heap;
mod idt;
mod interrupts;
mod ioapic;
mod keyboard;
mod lapic;
mod mem;
mod paging;
mod pci;
mod pic;
mod pit;
mod pmm;
mod port;
#[cfg(test)]
mod qemu;
mod ringbuf;
mod scancode;
mod serial;
mod shell;
mod smp;
mod stack;
mod task;
#[cfg(test)]
mod testing;
mod time;
mod user;
mod words;

#[cfg(not(test))]
use alloc::{boxed::Box, format, string::String, vec::Vec};
use core::panic::PanicInfo;
#[cfg(not(test))]
use interrupts::{KEYBOARD_IRQ, KEYBOARD_VECTOR, TIMER_IRQ, TIMER_VECTOR};
use limine::request::{
    ExecutableAddressRequest, ExecutableCmdlineRequest, FramebufferRequest, HhdmRequest, MemmapRequest, RsdpRequest,
};
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};

// Requests: static structures Limine looks for in the kernel file before it starts it.
// They must stay in the binary even though no code reads them (`#[used]`) and lie between
// the start and end markers in their own section (see linker.ld).
#[used]
#[unsafe(link_section = ".limine_requests_start")]
static REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

/// Which version of the boot protocol we speak. Limine sets a field in it to 0 if it supports it.
#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

/// A framebuffer: memory whose bytes are the pixels on the screen. Limine sets up a graphics mode for us.
#[used]
#[unsafe(link_section = ".limine_requests")]
static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::new();

/// The command line from `limine.conf` (`cmdline: …`): `fault=page`, `fault=stack`, … crash the kernel on purpose.
#[used]
#[unsafe(link_section = ".limine_requests")]
static CMDLINE: ExecutableCmdlineRequest = ExecutableCmdlineRequest::new();

/// Where Limine mapped all of physical memory (the higher-half direct map, see mem.rs).
#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM: HhdmRequest = HhdmRequest::new();

/// Which ranges of physical memory are RAM we may use, and which belong to firmware, Limine, the kernel, devices.
#[used]
#[unsafe(link_section = ".limine_requests")]
static MEMMAP: MemmapRequest = MemmapRequest::new();

/// Where Limine loaded the kernel: its physical and its virtual start.
#[used]
#[unsafe(link_section = ".limine_requests")]
static EXECUTABLE_ADDRESS: ExecutableAddressRequest = ExecutableAddressRequest::new();

/// Where the firmware's ACPI tables begin: the RSDP (see acpi.rs).
#[used]
#[unsafe(link_section = ".limine_requests")]
static RSDP: RsdpRequest = RsdpRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests_end")]
static REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

/// Limine jumps here: 64-bit long mode, paging on, interrupts off, on a stack Limine set up.
#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    if !BASE_REVISION.is_supported() {
        serial_println!("Limine does not support our base revision");
        halt();
    }
    console::init();
    gdt::init();
    interrupts::init();
    println!("Hello from YAK!");
    match console::CONSOLE.lock().as_ref() {
        Some(c) => serial_println!("Console: {} x {} characters", c.size().0, c.size().1),
        None => serial_println!("No 32-bit framebuffer - text goes to the serial port only"),
    }
    println!("Grüße - umlauts work too: äöü ÄÖÜ ß");
    println!(
        "GDT loaded: code segment {:#x}, TSS {:#x}, double fault stack top {:#x}",
        gdt::KERNEL_CODE_SELECTOR,
        gdt::TSS_SELECTOR,
        gdt::double_fault_stack_top()
    );
    println!(
        "IDT loaded: exceptions 0, 3, 6, 8 (on IST{}), 13, 14",
        gdt::DOUBLE_FAULT_IST
    );
    unsafe { core::arch::asm!("int3") };
    println!("Back from the breakpoint handler - the kernel carries on.");
    mem::init();
    smp::reserve_frame();
    print_memory_map();
    let info = pmm::info();
    println!(
        "Frames: {} MiB usable, {} frames free; the bitmap takes {} KiB at {:#x}",
        info.usable_frames as u64 * mem::PAGE_SIZE / (1024 * 1024),
        pmm::free_frames(),
        info.bitmap.length / 1024,
        info.bitmap.base
    );
    let frame = pmm::alloc_frame_zeroed().expect("a free frame");
    let pointer = frame.as_mut_ptr::<u64>();
    unsafe { pointer.write(0x00C0_FFEE) };
    let back = unsafe { pointer.read() };
    println!(
        "Frame {:#x}, seen at {:#x} through the HHDM: wrote 0xc0ffee, read {:#x}",
        frame.0, pointer as u64, back
    );
    pmm::free_frame(frame);
    show_paging();
    let stack = stack::alloc(4).expect("frames for a stack");
    println!(
        "Stack: 16 KiB below {:#x}, its guard page at {:#x} is mapped: {}",
        stack.top,
        stack.guard,
        if paging::translate(stack.guard).is_some() {
            "yes"
        } else {
            "no"
        }
    );
    show_heap();
    let apic_ids = start_interrupts();
    show_threads();
    show_smp(&apic_ids);
    show_scheduler();
    show_disk();
    show_ring3();
    match requested_fault() {
        Some("page") => interrupts::provoke_page_fault(),
        Some("stack") => interrupts::provoke_double_fault(),
        Some("overflow") => interrupts::provoke_stack_overflow(stack.top),
        Some("heap") => ask_too_much(),
        Some(other) => println!("Unknown fault={} (try page, stack, overflow or heap)", other),
        None => {}
    }
    println!("Shell: type help and Enter");
    shell::run()
}

/// Paging at work: a translation done by hand, and a frame mapped at an address of our choice.
#[cfg(not(test))]
fn show_paging() {
    let kernel = EXECUTABLE_ADDRESS.response().expect("Limine gave the kernel's address");
    let code = kmain as *const () as u64;
    println!(
        "Paging: PML4 at {:#x}; kmain at {:#x} is physical {:#x} (the kernel starts at physical {:#x})",
        paging::read_cr3().0,
        code,
        paging::translate(code).expect("kmain is mapped").0,
        kernel.physical_base
    );
    const PAGE: u64 = 0xFFFF_FE00_0000_0000;
    let frame = pmm::alloc_frame_zeroed().expect("a free frame");
    paging::map(PAGE, frame, paging::Flags::DATA).expect("PAGE is free");
    unsafe { (PAGE as *mut u64).write(0x0000_BEEF) };
    let back = unsafe { frame.as_mut_ptr::<u64>().read() };
    println!(
        "Mapped frame {:#x} at {:#x}: wrote 0xbeef there, read {:#x} through the HHDM",
        frame.0, PAGE, back
    );
    pmm::free_frame(paging::unmap(PAGE).expect("PAGE is mapped"));
}

/// Hardware interrupts through the APIC, and what the ACPI tables told us on the way.
#[cfg(not(test))]
fn start_interrupts() -> Vec<u8> {
    let started = interrupts::start();
    let acpi = &started.acpi;
    let names: Vec<&str> = acpi
        .tables
        .iter()
        .map(|signature| core::str::from_utf8(signature).unwrap_or("????"))
        .collect();
    println!(
        "ACPI: RSDP revision {}, {} tables: {}",
        acpi.revision,
        names.len(),
        names.join(" ")
    );
    let madt = &acpi.madt;
    println!(
        "MADT: {} CPU{}, local APIC at {:#x}, I/O APIC at {:#x} with {} inputs",
        madt.apic_ids.len(),
        if madt.apic_ids.len() == 1 { "" } else { "s" },
        madt.local_apic.0,
        madt.io_apic.0,
        ioapic::inputs()
    );
    println!(
        "Timer: the PIT's IRQ {} arrived at I/O APIC input {}; the local APIC timer counts {} per tick of 10 ms",
        TIMER_IRQ,
        madt.gsi(TIMER_IRQ),
        started.counts_per_tick
    );
    println!(
        "Timer: now the local APIC timer of CPU {} raises vector {}, {} Hz; input {} is masked: {}",
        started.cpu,
        TIMER_VECTOR,
        time::TICKS_PER_SECOND,
        madt.gsi(TIMER_IRQ),
        if ioapic::routed_to(madt.gsi(TIMER_IRQ)).is_none() {
            "yes"
        } else {
            "no"
        }
    );
    let before = time::uptime_ms();
    time::sleep_ms(1000);
    let slept = time::uptime_ms() - before;
    let enough = if slept >= 1000 { "yes" } else { "NO" };
    println!(
        "sleep_ms(1000) took {} ms (at least 1000: {}), the CPU waited in hlt",
        slept, enough
    );
    println!(
        "Keyboard: IRQ {} arrives at I/O APIC input {}, vector {} (US layout)",
        KEYBOARD_IRQ,
        madt.gsi(KEYBOARD_IRQ),
        KEYBOARD_VECTOR
    );
    madt.apic_ids.clone()
}

/// The other CPUs: started, and each asked who it is.
#[cfg(not(test))]
fn show_smp(apic_ids: &[u8]) {
    smp::start(apic_ids);
    println!(
        "SMP: {} CPUs run; the others started at the trampoline at physical {:#x}",
        smp::online(),
        smp::trampoline_frame().0
    );
    let answers: Vec<(usize, u8)> = (0..smp::online())
        .map(|cpu| smp::run_on(cpu, || (smp::cpu(), lapic::id())).expect("the CPU runs"))
        .collect();
    println!("SMP: a job on each CPU asked it (number, APIC ID): {:?}", answers);
}

/// One scheduler for all CPUs: as many threads as CPUs, all counting at the same time - each on a CPU of its own.
#[cfg(not(test))]
fn show_scheduler() {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, Ordering};
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(spin::Mutex::new(Vec::new()));
    let ids: Vec<task::Id> = (0..smp::online())
        .map(|_| {
            let (stop, seen) = (stop.clone(), seen.clone());
            task::spawn("counter", move || {
                while !stop.load(Ordering::Relaxed) {
                    let cpu = interrupts::without_interrupts(smp::cpu);
                    interrupts::without_interrupts(|| {
                        let mut seen = seen.lock();
                        if !seen.contains(&cpu) {
                            seen.push(cpu);
                        }
                    });
                }
            })
        })
        .collect();
    time::sleep_ms(100);
    stop.store(true, Ordering::Relaxed);
    ids.iter().for_each(|&id| task::join(id));
    let mut seen = interrupts::without_interrupts(|| seen.lock().clone());
    seen.sort();
    println!(
        "Scheduler: {} threads that never yield, 100 ms - they ran on the CPUs {:?}",
        ids.len(),
        seen
    );
}

/// Ring 3: a program that says hello through a system call, and one that tries a privileged instruction.
#[cfg(not(test))]
fn show_ring3() {
    let exit = user::run(user::hello());
    println!("Ring 3: the program ended with {:?}", exit);
    let exit = user::run(user::privileged());
    println!(
        "Ring 3: the program that tried `cli` ended with {:?} - and the kernel goes on",
        exit
    );
}

/// The devices on the PCI bus, the SATA disk, and the FAT32 volume on it.
#[cfg(not(test))]
fn show_disk() {
    use block::BlockDevice;
    for device in pci::enumerate() {
        println!(
            "PCI {:?}  {:04x}:{:04x}  {}",
            device.location,
            device.vendor,
            device.device,
            device.kind()
        );
    }
    if let Err(problem) = ahci::init() {
        println!("AHCI: {}", problem);
        return;
    }
    ahci::with_controller(|c| {
        let ports: Vec<String> = c.ports.iter().map(|p| format!("{} {:?}", p.number, p.kind)).collect();
        println!(
            "AHCI {}.{} at {:?}, ports in use: {}",
            c.version.0,
            c.version.1,
            c.pci.location,
            ports.join(", ")
        )
    });
    let guard = ahci::DISK.lock();
    let Some(disk) = guard.as_ref() else {
        println!("Disk: none - start QEMU with a SATA disk (run-qemu.sh does)");
        return;
    };
    println!(
        "Disk: {} on port {}, {} sectors ({} MiB)",
        disk.model(),
        disk.port(),
        disk.sector_count(),
        disk.sector_count() / 2048
    );
    drop(guard);
    if let Err(problem) = fs::mount() {
        println!("FAT32: cannot mount the disk: {:?}", problem);
        return;
    }
    let mut guard = fs::FS.lock();
    let fs = guard.as_mut().expect("just mounted");
    println!(
        "FAT32: volume {}, {} clusters of {} bytes, {} free",
        fs.label(),
        fs.cluster_count(),
        fs.cluster_size(),
        fs.free_clusters()
    );
    if let Ok(entries) = fs.read_dir("/") {
        let names: Vec<String> = entries
            .iter()
            .map(|e| format!("{}{}", e.name, if e.is_dir { "/" } else { "" }))
            .collect();
        println!("FAT32: / holds {}", names.join(", "));
    }
    if let Ok(text) = fs.read_file("/hello.txt") {
        print!("FAT32: /hello.txt says: {}", String::from_utf8_lossy(&text));
    }
    // what the cache saves: /big.bin read without it, then twice through it - counted in disk commands
    let commands = || ahci::stats().2;
    let mut uncached = fat32::FatFs::mount(fs::SystemDisk).expect("mounted once already");
    let before = commands();
    let _ = uncached.read_file("/big.bin");
    let without = commands() - before;
    let before = commands();
    let _ = fs.read_file("/big.bin");
    let first = commands() - before;
    let before = commands();
    let _ = fs.read_file("/big.bin");
    let second = commands() - before;
    println!(
        "Cache: reading /big.bin took {} disk commands without the cache, {} through it, {} the second time",
        without, first, second
    );
    let (interrupts, handled, _) = ahci::stats();
    println!(
        "AHCI: commands end by interrupt: {}, {} interrupts so far",
        if interrupts { "yes" } else { "no" },
        handled
    );
}

/// Threads at work: two that never give up the CPU, while main sleeps.
#[cfg(not(test))]
fn show_threads() {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    static STOP: AtomicBool = AtomicBool::new(false);
    static COUNTS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    task::init();
    println!(
        "Threads: kmain is now thread {}, \"main\"; the idle thread halts the CPU when nobody else is ready",
        task::current()
    );
    let ids: Vec<task::Id> = (0..2)
        .map(|i| {
            task::spawn("counter", move || {
                // no yield, no sleep: only the timer can take the CPU away from this loop
                while !STOP.load(Ordering::Relaxed) {
                    COUNTS[i].fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    time::sleep_ms(200);
    STOP.store(true, Ordering::Relaxed);
    ids.iter().for_each(|&id| task::join(id));
    let counts = COUNTS.each_ref().map(|c| c.load(Ordering::Relaxed));
    println!(
        "Threads: while main slept 200 ms, threads {} and {} counted to {} and {} - both ran: {}",
        ids[0],
        ids[1],
        counts[0],
        counts[1],
        if counts.iter().all(|&c| c > 0) { "yes" } else { "NO" }
    );
}

/// The heap at work: a `Box`, a `Vec` and a `String`, and what they cost.
#[cfg(not(test))]
fn show_heap() {
    let boxed = Box::new(42u64);
    let numbers: Vec<u64> = (1..=1000).collect();
    let text = format!("YAK has {} chapters", 10);
    println!(
        "Heap: Box at {:p} holds {}, a Vec of {} numbers sums to {}, format! made {:?}",
        boxed,
        *boxed,
        numbers.len(),
        numbers.iter().sum::<u64>(),
        text
    );
    let in_use = heap::used();
    drop((boxed, numbers, text));
    println!(
        "Heap: {} bytes in use, {} KiB mapped at {:#x} - after drop: {} bytes in use",
        in_use,
        heap::mapped() / 1024,
        heap::START,
        heap::used()
    );
}

/// `fault=heap`: more than the heap may ever map. `Vec` cannot report that - Rust panics instead.
#[cfg(not(test))]
fn ask_too_much() {
    println!(
        "Asking the heap for {} MiB and one byte ...",
        heap::MAX_SIZE / (1024 * 1024)
    );
    let too_big: Vec<u8> = Vec::with_capacity(heap::MAX_SIZE + 1);
    println!("Got {} bytes - the limit did not hold", too_big.capacity());
}

/// Every entry of Limine's memory map: where, how big, what.
#[cfg(not(test))]
fn print_memory_map() {
    use limine::memmap;
    let entries = MEMMAP.response().expect("Limine gave no memory map").entries();
    println!("Memory map ({} entries):", entries.len());
    for entry in entries {
        let kind = match entry.type_ {
            memmap::MEMMAP_USABLE => "usable",
            memmap::MEMMAP_RESERVED => "reserved",
            memmap::MEMMAP_ACPI_RECLAIMABLE => "ACPI tables",
            memmap::MEMMAP_ACPI_NVS => "ACPI non-volatile",
            memmap::MEMMAP_BAD_MEMORY => "bad memory",
            memmap::MEMMAP_BOOTLOADER_RECLAIMABLE => "Limine (page tables, our stack, the answers)",
            memmap::MEMMAP_EXECUTABLE_AND_MODULES => "our kernel",
            memmap::MEMMAP_FRAMEBUFFER => "framebuffer",
            memmap::MEMMAP_MAPPED_RESERVED => "reserved, but in the HHDM",
            _ => "other",
        };
        println!(
            "  {:#012x} - {:#012x} {:>8} KiB  {}",
            entry.base,
            entry.base + entry.length,
            entry.length / 1024,
            kind
        );
    }
}

/// `fault=…` from the kernel command line, if there is one.
#[cfg(not(test))]
fn requested_fault() -> Option<&'static str> {
    let cmdline = CMDLINE.response()?.cmdline();
    cmdline.split(' ').find_map(|word| word.strip_prefix("fault="))
}

/// The entry point of the test build (`cargo test`): run every `#[test_case]`.
#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    console::init();
    gdt::init();
    interrupts::init();
    mem::init();
    smp::reserve_frame();
    let apic_ids = interrupts::start().acpi.madt.apic_ids;
    task::init();
    smp::start(&apic_ids);
    ahci::init().expect("the AHCI controller and its disk");
    fs::mount().expect("a FAT32 volume on the disk");
    test_main();
    halt() // not reached: the runner ends QEMU
}

/// Stops the CPU for good: interrupts off, then `hlt` (wakes only for interrupts we no longer take).
pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("cli", "hlt") };
    }
}

/// Called by Rust when something panics. There is nobody to return to: report and stop.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("KERNEL PANIC: {}", info);
    halt()
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    testing::panic_handler(info)
}

#[test_case]
fn limine_supports_our_base_revision() {
    assert!(BASE_REVISION.is_supported());
}

#[test_case]
fn the_console_covers_the_real_screen() {
    let fb = FRAMEBUFFER.response().unwrap().framebuffers().first().unwrap();
    let console = console::CONSOLE.lock();
    let (columns, rows) = console.as_ref().expect("console on the framebuffer").size();
    assert_eq!((columns, rows), (fb.width as usize / 8, fb.height as usize / 16));
}
