//! The other CPUs. After reset only one CPU runs, the *bootstrap processor* (BSP) - the one Limine handed to
//! `kmain`. The others, the *application processors* (APs), wait until their local APIC receives an INIT and then a
//! STARTUP interrupt with a page number `v`: then they start in 16-bit real mode, like a PC of 1981, at physical
//! address `v << 12`.
//!
//! So the start code - the *trampoline* - must lie below 1 MiB, and it must walk through the history of the x86 on
//! its own: real mode -> 32-bit protected mode -> 64-bit long mode with our page tables, then a jump into Rust on a
//! stack of its own. There every AP loads its own GDT and TSS, the shared IDT, switches its local APIC on and waits
//! for jobs (`run_on`). The scheduler stays on the BSP for now.

use crate::interrupts::{self, WAKE_VECTOR, without_interrupts};
use crate::mem::{MIB, PAGE_SIZE, PhysAddr};
use crate::paging::{self, Flags};
use crate::{gdt, lapic, pmm, stack, time};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

/// How many CPUs we manage at most.
pub const MAX_CPUS: usize = 8;

/// The APIC ID of every CPU, by CPU number (0 = the BSP), and whether it runs.
static APIC_IDS: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
static ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static COUNT: AtomicUsize = AtomicUsize::new(1);

/// The frame below 1 MiB for the trampoline - taken right after the frame allocator is built (`reserve_frame`).
static TRAMPOLINE_FRAME: AtomicU64 = AtomicU64::new(0);

// The trampoline. It runs at whatever address it was copied to, so it addresses its own data relative to its start:
// ebx/rbx holds that physical address. The assembler cannot use the difference of two labels as a memory operand,
// so the few instructions that read the trampoline's data are written as bytes, with the offset after them.
global_asm!(
    ".global ap_trampoline",
    ".global ap_trampoline_end",
    ".global ap_gdt",
    ".global ap_gdt_pointer",
    ".global ap_far32",
    ".global ap_far64",
    ".global ap_protected",
    ".global ap_long",
    ".global ap_cr3",
    ".global ap_stack",
    ".global ap_entry",
    ".global ap_cpu",
    ".code16",
    "ap_trampoline:",
    "cli",
    "cld",
    "mov ax, cs",
    "mov ds, ax",
    // ebx = the trampoline's physical address (CS * 16), kept through all three modes
    "xor ebx, ebx",
    "mov bx, ax",
    "shl ebx, 4",
    // lgdt [ap_gdt_pointer]: 0F 01 /2 with a 16-bit displacement
    ".byte 0x0F, 0x01, 0x16",
    ".word ap_gdt_pointer - ap_trampoline",
    "mov eax, cr0",
    "or eax, 1", // PE: protected mode
    "mov cr0, eax",
    // jmp far dword [ap_far32] - loads CS with the 32-bit code segment
    ".byte 0x66, 0xFF, 0x2E",
    ".word ap_far32 - ap_trampoline",
    ".code32",
    "ap_protected:",
    "mov ax, 0x10",
    "mov ds, ax",
    "mov es, ax",
    "mov ss, ax",
    "mov eax, cr4",
    "or eax, 1 << 5", // PAE: 64-bit page table entries
    "mov cr4, eax",
    // mov eax, [ebx + ap_cr3]: our PML4
    ".byte 0x8B, 0x83",
    ".long ap_cr3 - ap_trampoline",
    "mov cr3, eax",
    "mov ecx, 0xC0000080", // the EFER register
    "rdmsr",
    "or eax, (1 << 8) | (1 << 11)", // LME: long mode; NXE: our tables use the no-execute bit
    "wrmsr",
    "mov eax, cr0",
    "or eax, (1 << 31) | (1 << 16)", // PG: paging on - and with LME, long mode; WP: like the BSP
    "mov cr0, eax",
    // jmp far [ebx + ap_far64] - loads CS with the 64-bit code segment
    ".byte 0xFF, 0xAB",
    ".long ap_far64 - ap_trampoline",
    ".code64",
    "ap_long:",
    "mov ax, 0x20",
    "mov ds, ax",
    "mov es, ax",
    "mov ss, ax",
    // mov rsp / rdi / rax, [rbx + ...]: the stack, the CPU number and where to go
    ".byte 0x48, 0x8B, 0xA3",
    ".long ap_stack - ap_trampoline",
    ".byte 0x48, 0x8B, 0xBB",
    ".long ap_cpu - ap_trampoline",
    ".byte 0x48, 0x8B, 0x83",
    ".long ap_entry - ap_trampoline",
    "xor ebp, ebp",
    "call rax",
    "ud2",
    ".align 16",
    // the trampoline's own GDT: null, 32-bit code, 32-bit data, 64-bit code, 64-bit data
    "ap_gdt:",
    ".quad 0",
    ".quad 0x00CF9A000000FFFF",
    ".quad 0x00CF92000000FFFF",
    ".quad 0x00AF9A000000FFFF",
    ".quad 0x00CF92000000FFFF",
    "ap_gdt_pointer:",
    ".word 5 * 8 - 1",
    ".long 0", // filled in: the physical address of ap_gdt
    "ap_far32:",
    ".long 0", // filled in: the physical address of ap_protected
    ".word 0x08",
    "ap_far64:",
    ".long 0", // filled in: the physical address of ap_long
    ".word 0x18",
    ".align 8",
    "ap_cr3: .quad 0",
    "ap_stack: .quad 0",
    "ap_entry: .quad 0",
    "ap_cpu: .quad 0",
    "ap_trampoline_end:",
);

unsafe extern "C" {
    static ap_trampoline: u8;
    static ap_trampoline_end: u8;
    static ap_gdt: u8;
    static ap_gdt_pointer: u8;
    static ap_far32: u8;
    static ap_far64: u8;
    static ap_protected: u8;
    static ap_long: u8;
    static ap_cr3: u8;
    static ap_stack: u8;
    static ap_entry: u8;
    static ap_cpu: u8;
}

/// Where `symbol` lies inside the trampoline.
fn offset(symbol: *const u8) -> u64 {
    symbol as u64 - (&raw const ap_trampoline) as u64
}

/// Writes the lowest `size` bytes of `value` into the copy of the trampoline in `frame`, at `symbol`.
fn fill_in(frame: PhysAddr, symbol: *const u8, value: u64, size: usize) {
    let target = PhysAddr(frame.0 + offset(symbol)).as_mut_ptr::<u8>();
    unsafe { core::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), target, size) };
}

/// Takes a frame below 1 MiB for the trampoline. Call it right after the frame allocator is built: it hands out the
/// lowest free frame first, and the frames below 1 MiB are few.
pub fn reserve_frame() {
    let frame = pmm::alloc_frame().expect("a free frame");
    assert!(frame.0 < MIB, "no free frame below 1 MiB for the trampoline");
    TRAMPOLINE_FRAME.store(frame.0, Ordering::Relaxed);
}

/// Where the trampoline is copied to.
pub fn trampoline_frame() -> PhysAddr {
    PhysAddr(TRAMPOLINE_FRAME.load(Ordering::Relaxed))
}

/// The number of the CPU this runs on (0 = the BSP).
pub fn cpu() -> usize {
    let id = lapic::id();
    (0..COUNT.load(Ordering::Relaxed))
        .find(|&cpu| APIC_IDS[cpu].load(Ordering::Relaxed) == id)
        .unwrap_or(0)
}

/// How many CPUs run, the BSP included.
pub fn online() -> usize {
    ONLINE.iter().filter(|cpu| cpu.load(Ordering::Acquire)).count()
}

type Job = Box<dyn FnOnce() + Send>;
/// One mailbox per CPU: the job it runs next.
static JOBS: [Mutex<Option<Job>>; MAX_CPUS] = [const { Mutex::new(None) }; MAX_CPUS];

/// Where an AP arrives in Rust, on its own stack, with interrupts off.
extern "C" fn ap_main(cpu: u64) -> ! {
    let cpu = cpu as usize;
    let double_fault_stack = stack::alloc(5).expect("frames for a double fault stack");
    gdt::init_other_cpu(double_fault_stack.top);
    interrupts::load_idt();
    lapic::init_local();
    ONLINE[cpu].store(true, Ordering::Release);
    loop {
        // the mailbox is taken with interrupts off - on every CPU, like every lock that a CPU might otherwise hold
        // while an interrupt handler on the same CPU wants it
        unsafe { asm!("cli", options(nomem, nostack)) };
        let job = JOBS[cpu].lock().take();
        match job {
            Some(job) => job(),
            // `sti` takes effect after `hlt` begins: a wake-up right after the check still ends the `hlt`
            None => unsafe { asm!("sti", "hlt", options(nomem, nostack)) },
        }
    }
}

/// Runs `job` on CPU `cpu` and waits for its result; `None` if there is no such CPU. On the APs, jobs run with
/// interrupts off and must not wait for anything - there is no scheduler there yet.
pub fn run_on<R: Send + 'static>(cpu: usize, job: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    if cpu >= MAX_CPUS || !ONLINE[cpu].load(Ordering::Acquire) {
        return None;
    }
    if cpu == 0 {
        return Some(without_interrupts(job)); // the scheduler only runs threads on the BSP
    }
    let result = Arc::new(Mutex::new(None));
    let slot = result.clone();
    let mut job: Option<Job> = Some(Box::new(move || *slot.lock() = Some(job())));
    while job.is_some() {
        without_interrupts(|| {
            let mut mailbox = JOBS[cpu].lock();
            if mailbox.is_none() {
                *mailbox = job.take();
            }
        });
    }
    lapic::send_interrupt(APIC_IDS[cpu].load(Ordering::Relaxed), WAKE_VECTOR);
    loop {
        if let Some(value) = without_interrupts(|| result.lock().take()) {
            return Some(value);
        }
        core::hint::spin_loop(); // microseconds; the timer can still switch threads meanwhile
    }
}

/// Waits up to `ms` milliseconds for CPU `cpu` to report.
fn wait_online(cpu: usize, ms: u64) -> bool {
    let end = time::ticks() + time::ticks_for(ms);
    while time::ticks() < end && !ONLINE[cpu].load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    ONLINE[cpu].load(Ordering::Acquire)
}

/// Starts every other CPU the MADT lists, one after another. Needs the timer (for the waits) and `reserve_frame`.
pub fn start(apic_ids: &[u8]) {
    let bsp = lapic::id();
    APIC_IDS[0].store(bsp, Ordering::Relaxed);
    ONLINE[0].store(true, Ordering::Release);
    let others: Vec<u8> = apic_ids
        .iter()
        .copied()
        .filter(|&id| id != bsp)
        .take(MAX_CPUS - 1)
        .collect();
    if others.is_empty() {
        return;
    }
    // copy the trampoline below 1 MiB, and fill in the addresses it needs
    let frame = trampoline_frame();
    let size = offset(&raw const ap_trampoline_end);
    assert!(size <= PAGE_SIZE, "the trampoline is larger than a page");
    unsafe { core::ptr::copy_nonoverlapping(&raw const ap_trampoline, frame.as_mut_ptr::<u8>(), size as usize) };
    let physical = |symbol: *const u8| frame.0 + offset(symbol);
    fill_in(
        frame,
        (&raw const ap_gdt_pointer).wrapping_add(2),
        physical(&raw const ap_gdt),
        4,
    );
    fill_in(frame, &raw const ap_far32, physical(&raw const ap_protected), 4);
    fill_in(frame, &raw const ap_far64, physical(&raw const ap_long), 4);
    let root = paging::read_cr3();
    assert!(root.0 < 1 << 32, "the PML4 must be reachable from 32-bit code");
    fill_in(frame, &raw const ap_cr3, root.0, 8);
    fill_in(
        frame,
        &raw const ap_entry,
        ap_main as extern "C" fn(u64) -> ! as usize as u64,
        8,
    );

    // When the AP turns paging on, its next instruction is fetched through our page tables - at the trampoline's
    // physical address. So for the start, that page is mapped there too ("identity mapped").
    paging::map(frame.0, frame, Flags::CODE).expect("the trampoline's address is free in the lower half");
    for (cpu, apic_id) in (1..).zip(others) {
        let stack = stack::alloc(8).expect("frames for a CPU's stack");
        fill_in(frame, &raw const ap_stack, stack.top, 8);
        fill_in(frame, &raw const ap_cpu, cpu as u64, 8);
        APIC_IDS[cpu].store(apic_id, Ordering::Relaxed);
        COUNT.store(cpu + 1, Ordering::Relaxed);
        // Intel's recipe (SDM Vol. 3A, 9.4.4): INIT, wait 10 ms, STARTUP - and a second STARTUP if the first is lost
        let page = (frame.0 >> 12) as u8;
        lapic::send_init(apic_id);
        time::sleep_ms(10);
        lapic::send_startup(apic_id, page);
        if !wait_online(cpu, 10) {
            lapic::send_startup(apic_id, page);
            if !wait_online(cpu, 200) {
                crate::println!("SMP: the CPU with APIC ID {} did not start", apic_id);
            }
        }
    }
    paging::unmap(frame.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (APIC ID, CPU number, task register, GDT address) of the CPU this runs on.
    fn who_am_i() -> (u8, usize, u16, u64) {
        let tr: u16;
        let mut gdtr = [0u8; 10];
        unsafe {
            asm!("str {0:x}", out(reg) tr, options(nomem, nostack, preserves_flags));
            asm!("sgdt [{}]", in(reg) gdtr.as_mut_ptr(), options(nostack, preserves_flags));
        }
        (
            lapic::id(),
            cpu(),
            tr,
            u64::from_le_bytes(gdtr[2..].try_into().unwrap()),
        )
    }

    #[test_case]
    fn every_cpu_of_the_madt_runs() {
        let madt = crate::acpi::read().unwrap().madt;
        assert_eq!(online(), madt.apic_ids.len().min(MAX_CPUS));
    }

    #[test_case]
    fn a_job_runs_on_the_cpu_it_was_sent_to() {
        let answers: Vec<_> = (0..online()).map(|cpu| run_on(cpu, who_am_i).unwrap()).collect();
        for (cpu, &(apic_id, number, tr, _)) in answers.iter().enumerate() {
            assert_eq!(number, cpu);
            assert_eq!(apic_id, APIC_IDS[cpu].load(Ordering::Relaxed));
            assert_eq!(tr, gdt::TSS_SELECTOR, "CPU {} has no TSS loaded", cpu);
        }
        // every CPU has a GDT (and with it a TSS) of its own
        let mut gdts: Vec<u64> = answers.iter().map(|a| a.3).collect();
        gdts.sort();
        gdts.dedup();
        assert_eq!(gdts.len(), online());
        assert!(run_on(MAX_CPUS, || ()).is_none());
    }

    #[test_case]
    fn the_heap_works_on_every_cpu() {
        for cpu in 0..online() {
            let sum = run_on(cpu, move || (1..=100u64).collect::<Vec<_>>().iter().sum::<u64>());
            assert_eq!(sum, Some(5050));
        }
    }

    #[test_case]
    fn the_trampoline_is_below_1_mib_and_unmapped_again() {
        let frame = trampoline_frame();
        assert!(frame.0 != 0 && frame.0 < MIB);
        assert_eq!(paging::translate(frame.0), None); // the identity mapping was only for the start
    }
}
