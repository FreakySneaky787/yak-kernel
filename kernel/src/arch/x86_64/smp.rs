//! Multiprocessor start: wakes the other CPU cores ("application
//! processors", APs) and brings them into the kernel.
//!
//! After reset only the boot CPU (BSP) runs; the others wait for a startup
//! signal from their local APIC. The classic sequence (Intel SDM Vol. 3, 9.4):
//! INIT IPI, wait 10 ms, STARTUP IPI (SIPI) with a page number `v` - the AP
//! then starts in 16-bit real mode at physical address `v << 12`. So the
//! start code ("trampoline") must live below 1 MiB; `pmm` reserves one frame
//! there at boot. The trampoline switches real mode -> protected mode ->
//! long mode (PAE + EFER.LME + paging, using the kernel's own PML4, in which
//! the trampoline page is temporarily identity-mapped) and jumps to `ap_main`
//! on a fresh kernel stack. APs are started one after another.
//!
//! In `ap_main` every AP loads its own GDT/TSS, the shared IDT, enables its
//! FPU/SSE, syscall MSRs and local APIC, turns its start-up context into its
//! idle thread (`task::init_cpu`), starts its LAPIC timer and reports in.
//! From then on the scheduler runs threads on it. The idle thread also runs
//! jobs handed to this CPU with `run_on` (woken by a `WAKE_VECTOR` IPI, which
//! also makes an idle CPU look for ready threads); jobs must not block.

use super::{gdt, interrupts, lapic};
use crate::mem::paging::{self, Flags};
use crate::mem::{PhysAddr, VirtAddr, pmm, stack};
use crate::time;
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

/// Highest number of CPUs the kernel manages.
pub const MAX_CPUS: usize = 16;
/// IPI vector that wakes an idle AP (the handler only sends EOI).
pub const WAKE_VECTOR: u8 = 0xF0;
/// IPI vector of a TLB shootdown (`shoot_down_tlb`).
pub const TLB_VECTOR: u8 = 0xF1;

const NO_CPU: u8 = 0xFF;
/// Local APIC ID -> CPU index (0 = boot CPU).
static APIC_TO_CPU: [AtomicU8; 256] = [const { AtomicU8::new(NO_CPU) }; 256];
static CPU_APIC_ID: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(NO_CPU) }; MAX_CPUS];
static ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static CPU_COUNT: AtomicUsize = AtomicUsize::new(1);

type Job = Box<dyn FnOnce() + Send>;
static JOBS: [Mutex<Option<Job>>; MAX_CPUS] = [const { Mutex::new(None) }; MAX_CPUS];

/// Index of the CPU this code runs on (0 before the local APIC is set up).
pub fn cpu_index() -> usize {
    if !lapic::is_initialized() {
        return 0;
    }
    match APIC_TO_CPU[lapic::id() as usize].load(Ordering::Relaxed) {
        NO_CPU => 0,
        i => i as usize,
    }
}

/// Number of CPUs that are running (including the boot CPU).
pub fn online_count() -> usize {
    ONLINE.iter().filter(|o| o.load(Ordering::Acquire)).count()
}

/// Local APIC IDs of the online CPUs, by CPU index.
pub fn online_apic_ids() -> Vec<u8> {
    (0..CPU_COUNT.load(Ordering::Relaxed))
        .filter(|&i| ONLINE[i].load(Ordering::Acquire))
        .map(|i| CPU_APIC_ID[i].load(Ordering::Relaxed))
        .collect()
}

// ---- the trampoline --------------------------------------------------------

core::arch::global_asm!(
    ".global ap_trampoline",
    ".global ap_trampoline_end",
    ".global ap_gdt_ptr",
    ".global ap_far32",
    ".global ap_far64",
    ".global ap_cr3",
    ".global ap_stack",
    ".global ap_entry",
    ".global ap_arg",
    ".global ap_gdt",
    ".global ap_pm32",
    ".global ap_lm64",
    ".code16",
    "ap_trampoline:",
    "cli",
    "cld",
    "mov ax, cs",
    "mov ds, ax",
    // ebx = physical base of the trampoline (CS * 16), kept through all modes
    "xor ebx, ebx",
    "mov bx, ax",
    "shl ebx, 4",
    // The assembler does not take label differences in memory operands, so the
    // instructions that address trampoline data are encoded by hand.
    // lgdt [disp16]: 0F 01 /2, ModRM 0x16
    ".byte 0x0F, 0x01, 0x16",
    ".word ap_gdt_ptr - ap_trampoline",
    "mov eax, cr0",
    "or eax, 1", // PE: protected mode
    "mov cr0, eax",
    // jmp far dword [ap_far32] (66 FF /5 with a 16-bit displacement)
    ".byte 0x66, 0xFF, 0x2E",
    ".word ap_far32 - ap_trampoline",
    ".code32",
    "ap_pm32:",
    "mov ax, 0x10",
    "mov ds, ax",
    "mov es, ax",
    "mov ss, ax",
    "mov eax, cr4",
    "or eax, 1 << 5", // PAE
    "mov cr4, eax",
    // mov eax, [ebx + disp32]: 8B /r, ModRM 0x83
    ".byte 0x8B, 0x83",
    ".long ap_cr3 - ap_trampoline",
    "mov cr3, eax",
    "mov ecx, 0xC0000080", // EFER
    "rdmsr",
    "or eax, (1 << 8) | (1 << 11)", // LME + NXE (our page tables use NX)
    "wrmsr",
    "mov eax, cr0",
    "or eax, (1 << 31) | (1 << 16)", // PG + WP (like the boot CPU)
    "mov cr0, eax",
    // jmp far [ebx + ap_far64] (FF /5, [ebx + disp32])
    ".byte 0xFF, 0xAB",
    ".long ap_far64 - ap_trampoline",
    ".code64",
    "ap_lm64:",
    "mov ax, 0x20",
    "mov ds, ax",
    "mov es, ax",
    "mov ss, ax",
    // mov rsp/rdi/rax, [rbx + disp32]: REX.W 8B /r, ModRM 0xA3 / 0xBB / 0x83
    ".byte 0x48, 0x8B, 0xA3",
    ".long ap_stack - ap_trampoline",
    ".byte 0x48, 0x8B, 0xBB",
    ".long ap_arg - ap_trampoline",
    ".byte 0x48, 0x8B, 0x83",
    ".long ap_entry - ap_trampoline",
    "xor ebp, ebp",
    "call rax",
    "ud2",
    ".align 16",
    // null, code32, data32, code64, data64
    "ap_gdt:",
    ".quad 0",
    ".quad 0x00CF9A000000FFFF",
    ".quad 0x00CF92000000FFFF",
    ".quad 0x00AF9A000000FFFF",
    ".quad 0x00CF92000000FFFF",
    "ap_gdt_ptr:",
    ".word 5 * 8 - 1",
    ".long 0", // patched: physical address of ap_gdt
    "ap_far32:",
    ".long 0", // patched: physical address of ap_pm32
    ".word 0x08",
    "ap_far64:",
    ".long 0", // patched: physical address of ap_lm64
    ".word 0x18",
    ".align 8",
    "ap_cr3: .quad 0",
    "ap_stack: .quad 0",
    "ap_entry: .quad 0",
    "ap_arg: .quad 0",
    "ap_trampoline_end:",
);

unsafe extern "C" {
    static ap_trampoline: u8;
    static ap_trampoline_end: u8;
    static ap_gdt: u8;
    static ap_gdt_ptr: u8;
    static ap_far32: u8;
    static ap_far64: u8;
    static ap_pm32: u8;
    static ap_lm64: u8;
    static ap_cr3: u8;
    static ap_stack: u8;
    static ap_entry: u8;
    static ap_arg: u8;
}

/// Offset of a trampoline symbol from its start.
fn offset(sym: *const u8) -> usize {
    sym as usize - (&raw const ap_trampoline) as usize
}

/// Writes `value` (little endian, `size` bytes) at `field` of the copy at `base`.
fn patch(base: VirtAddr, field: *const u8, value: u64, size: usize) {
    let dst = (base.as_u64() as usize + offset(field)) as *mut u8;
    unsafe { core::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), dst, size) };
}

/// Where the AP lands in long mode, on its own stack.
extern "C" fn ap_main(cpu: u64) -> ! {
    let cpu = cpu as usize;
    // first, so that `cpu_index()` is right from here on
    APIC_TO_CPU[lapic::id() as usize].store(cpu as u8, Ordering::Relaxed);
    let df_stack = stack::alloc_stack(5).expect("no memory for a double fault stack");
    gdt::init_cpu(cpu, df_stack.top.as_u64());
    interrupts::kernel_idt().expect("IDT not built").load();
    super::fpu::init();
    super::cpu::enable_protection();
    super::syscall::init(cpu);
    lapic::init_local();
    crate::task::init_cpu(cpu, &alloc::format!("idle{}", cpu));
    lapic::start_timer_calibrated(super::irq::TIMER_VECTOR);
    ONLINE[cpu].store(true, Ordering::Release);
    idle_loop(cpu)
}

/// The idle thread of CPU `cpu` (every CPU has one, the boot CPU's too):
/// runs jobs sent with `run_on`, sleeps in between.
pub fn idle_loop(cpu: usize) -> ! {
    loop {
        super::disable_interrupts();
        let job = JOBS[cpu].lock().take();
        match job {
            // with interrupts off: an idle thread preempted inside a job (holding
            // the result lock) could starve behind the very thread waiting for it (bug #9)
            Some(job) => job(),
            // `sti; hlt` is atomic: a wake-up IPI right after the check still ends the `hlt`
            None => unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) },
        }
    }
}

/// Runs `f` on CPU `cpu` and waits for its result. `None` if that CPU is not
/// online. `f` runs with interrupts disabled and must not block.
pub fn run_on<R: Send + 'static>(cpu: usize, f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    if cpu >= MAX_CPUS || !ONLINE[cpu].load(Ordering::Acquire) {
        return None;
    }
    // Our own CPU: run it right here, with interrupts off so that we cannot
    // move to another CPU between the check and the call (jobs never block).
    let was_enabled = super::interrupts_enabled();
    super::disable_interrupts();
    if cpu == cpu_index() {
        let r = f();
        if was_enabled {
            super::enable_interrupts();
        }
        return Some(r);
    }
    if was_enabled {
        super::enable_interrupts();
    }
    let result: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let slot = result.clone();
    let job: Job = Box::new(move || {
        let r = f();
        *slot.lock() = Some(r);
    });
    let mut job = Some(job);
    loop {
        // interrupts off while holding the mailbox: the AP takes it with interrupts off too
        let posted = super::without_interrupts(|| {
            let mut mailbox = JOBS[cpu].lock();
            if mailbox.is_none() {
                *mailbox = job.take();
                true
            } else {
                false
            }
        });
        if posted {
            break;
        }
        time::sleep_ms(1); // another job is still pending there
    }
    wake_cpu(cpu);
    loop {
        // interrupts off while holding the lock: the job takes it with interrupts off
        if let Some(r) = super::without_interrupts(|| result.lock().take()) {
            return Some(r);
        }
        // Block, don't just yield: if we landed on the target CPU ourselves, a
        // ready thread there would keep its idle thread (which runs the job) away.
        time::sleep_ms(1);
    }
}

/// One TLB shootdown at a time: the page table to flush and how many CPUs
/// still have to confirm.
static SHOOTDOWN: Mutex<()> = Mutex::new(());
static SHOOTDOWN_ROOT: AtomicU64 = AtomicU64::new(0);
static SHOOTDOWN_PENDING: AtomicUsize = AtomicUsize::new(0);

fn flush_if_running(root: u64) {
    if paging::read_cr3().0 == root {
        unsafe { paging::write_cr3(PhysAddr(root)) };
    }
}

/// Makes every CPU that runs on the page table `root` drop its cached
/// translations (CR3 reload) and waits until all online CPUs have checked.
/// The other CPUs do it right in the `TLB_VECTOR` interrupt handler, so a
/// busy CPU answers within microseconds (bug #15: `run_on` waited for its
/// idle thread). Needs interrupts enabled, so that a CPU waiting here still
/// answers another CPU's shootdown.
pub fn shoot_down_tlb(root: PhysAddr) {
    debug_assert!(super::interrupts_enabled(), "shoot_down_tlb with interrupts off");
    let _one_at_a_time = SHOOTDOWN.lock();
    SHOOTDOWN_ROOT.store(root.0, Ordering::SeqCst);
    // interrupts off: we must not move to another CPU while choosing the targets
    super::without_interrupts(|| {
        let me = cpu_index();
        flush_if_running(root.0);
        let targets: Vec<usize> = (0..CPU_COUNT.load(Ordering::Relaxed))
            .filter(|&c| c != me && ONLINE[c].load(Ordering::Acquire))
            .collect();
        SHOOTDOWN_PENDING.store(targets.len(), Ordering::SeqCst);
        for cpu in targets {
            lapic::send_fixed(CPU_APIC_ID[cpu].load(Ordering::Relaxed), TLB_VECTOR);
        }
    });
    while SHOOTDOWN_PENDING.load(Ordering::SeqCst) != 0 {
        core::hint::spin_loop();
    }
}

/// Called by the `TLB_VECTOR` interrupt handler.
pub fn on_tlb_ipi() {
    flush_if_running(SHOOTDOWN_ROOT.load(Ordering::SeqCst));
    SHOOTDOWN_PENDING.fetch_sub(1, Ordering::SeqCst);
}

/// Sends the wake-up IPI to CPU `cpu`.
pub fn wake_cpu(cpu: usize) {
    if ONLINE[cpu].load(Ordering::Acquire) {
        lapic::send_fixed(CPU_APIC_ID[cpu].load(Ordering::Relaxed), WAKE_VECTOR);
    }
}

/// Waits up to `ms` milliseconds for CPU `cpu` to report in.
fn wait_online(cpu: usize, ms: u64) -> bool {
    let end = time::ticks() + ms.div_ceil(time::MS_PER_TICK) + 1;
    while time::ticks() < end {
        if ONLINE[cpu].load(Ordering::Acquire) {
            return true;
        }
        core::hint::spin_loop();
    }
    ONLINE[cpu].load(Ordering::Acquire)
}

/// Starts every enabled CPU from the MADT. Needs the timer (interrupts on).
pub fn init() {
    let bsp = lapic::id();
    APIC_TO_CPU[bsp as usize].store(0, Ordering::Relaxed);
    CPU_APIC_ID[0].store(bsp, Ordering::Relaxed);
    ONLINE[0].store(true, Ordering::Release);

    let Some(madt) = crate::acpi::info().map(|a| &a.madt) else {
        return;
    };
    let aps: Vec<u8> = madt
        .local_apics
        .iter()
        .filter(|l| l.flags & 1 != 0 && l.apic_id != bsp)
        .map(|l| l.apic_id)
        .collect();
    if aps.is_empty() {
        return;
    }
    let Some(low) = pmm::low_frame() else {
        crate::serial_println!("smp: no frame below 1 MiB, staying on one CPU");
        return;
    };
    let root = paging::kernel_root();
    assert!(
        root.as_u64() < 1 << 32,
        "kernel PML4 above 4 GiB: not reachable from 32-bit code"
    );

    // Copy the trampoline and fill in the physical addresses
    let size = offset(&raw const ap_trampoline_end);
    assert!(size <= 4096, "trampoline larger than a page");
    let copy = low.to_virt();
    unsafe { core::ptr::copy_nonoverlapping(&raw const ap_trampoline, copy.as_mut_ptr::<u8>(), size) };
    let phys = |sym: *const u8| low.as_u64() + offset(sym) as u64;
    patch(
        copy,
        (&raw const ap_gdt_ptr).wrapping_add(2),
        phys(&raw const ap_gdt),
        4,
    );
    patch(copy, &raw const ap_far32, phys(&raw const ap_pm32), 4);
    patch(copy, &raw const ap_far64, phys(&raw const ap_lm64), 4);
    patch(copy, &raw const ap_cr3, root.as_u64(), 8);
    patch(
        copy,
        &raw const ap_entry,
        ap_main as extern "C" fn(u64) -> ! as usize as u64,
        8,
    );

    // The trampoline runs at its physical address once paging is on
    let identity = VirtAddr(low.as_u64());
    let mapped_here = paging::translate_in(root, identity).is_none();
    if mapped_here {
        paging::map_page_in(root, identity, low, Flags::PRESENT | Flags::WRITABLE).expect("cannot map the trampoline");
    }

    for apic_id in aps {
        let cpu = CPU_COUNT.load(Ordering::Relaxed);
        if cpu >= MAX_CPUS {
            break;
        }
        let stack = stack::alloc_stack(stack::DEFAULT_STACK_PAGES).expect("no memory for an AP stack");
        patch(copy, &raw const ap_stack, stack.top.as_u64(), 8);
        patch(copy, &raw const ap_arg, cpu as u64, 8);
        CPU_APIC_ID[cpu].store(apic_id, Ordering::Relaxed);
        CPU_COUNT.store(cpu + 1, Ordering::Relaxed);

        lapic::send_init(apic_id);
        time::sleep_ms(10);
        lapic::send_startup(apic_id, (low.as_u64() >> 12) as u8);
        if !wait_online(cpu, 10) {
            lapic::send_startup(apic_id, (low.as_u64() >> 12) as u8); // the spec sends it twice
            if !wait_online(cpu, 200) {
                crate::serial_println!("smp: CPU with APIC ID {} did not start", apic_id);
                CPU_COUNT.store(cpu, Ordering::Relaxed);
                stack::free_stack(stack);
                continue;
            }
        }
    }
    if mapped_here {
        let _ = paging::unmap_page_in(root, identity);
    }
}

/// Physical address of the trampoline frame (for tests and `cpu`).
pub fn trampoline_frame() -> Option<PhysAddr> {
    pmm::low_frame()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn all_cpus_from_the_madt_are_online() {
        let madt = &crate::acpi::info().unwrap().madt;
        let enabled = madt.local_apics.iter().filter(|l| l.flags & 1 != 0).count();
        assert_eq!(
            online_count(),
            enabled.min(MAX_CPUS),
            "APIC IDs online: {:?}",
            online_apic_ids()
        );
    }

    #[test_case]
    fn jobs_run_on_their_cpu() {
        let mut seen = Vec::new();
        for cpu in 0..online_count() {
            let (apic, index, tr) = run_on(cpu, || {
                let tr: u16;
                unsafe { core::arch::asm!("str {0:x}", out(reg) tr) };
                (lapic::id(), cpu_index(), tr)
            })
            .expect("CPU not online");
            assert_eq!(index, cpu, "job ran on the wrong CPU");
            assert_eq!(tr, gdt::TSS_SELECTOR, "CPU {} has no TSS loaded", cpu);
            seen.push(apic);
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), online_count(), "two CPUs share an APIC ID");
        assert!(run_on(MAX_CPUS - 1, || ()).is_none() || online_count() == MAX_CPUS);
    }

    /// Regression test for bug #6: jobs for CPU 0 sent from threads running
    /// on other CPUs (the boot CPU's idle thread did not look for jobs).
    #[test_case]
    fn jobs_for_every_cpu_from_every_cpu() {
        let ids: Vec<_> = (0..4)
            .map(|_| {
                crate::task::spawn("run-on-test", || {
                    for _ in 0..5 {
                        for cpu in 0..online_count() {
                            assert_eq!(run_on(cpu, cpu_index), Some(cpu));
                        }
                    }
                })
            })
            .collect();
        for id in ids {
            crate::task::join(id);
        }
    }

    #[test_case]
    fn aps_have_their_own_fpu_and_heap_access() {
        for cpu in 1..online_count() {
            // the heap is shared (real lock), the FPU is enabled per CPU
            let v = run_on(cpu, move || {
                let v: Vec<u64> = (0..1000).map(|i| i * cpu as u64).collect();
                (v.iter().sum::<u64>(), super::super::fpu::read_cr4() & (1 << 9) != 0)
            });
            assert_eq!(v, Some((499_500 * cpu as u64, true)));
        }
    }

    /// Regression test for bug #15: a TLB shootdown must not wait until busy
    /// CPUs go idle (it took about a second per `brk` with threads).
    #[test_case]
    fn tlb_shootdown_is_fast_while_all_cpus_are_busy() {
        use crate::task;
        let stop = Arc::new(AtomicBool::new(false));
        let ids: Vec<_> = (0..online_count())
            .map(|_| {
                let stop = stop.clone();
                task::spawn("tlb-spin", move || {
                    while !stop.load(Ordering::Relaxed) {
                        core::hint::spin_loop();
                    }
                })
            })
            .collect();
        crate::time::sleep_ms(20); // let the spinners occupy every CPU
        let start = crate::time::uptime_us();
        for _ in 0..20 {
            paging::shoot_down(paging::kernel_root());
        }
        let us = crate::time::uptime_us() - start;
        stop.store(true, Ordering::Relaxed);
        for id in ids {
            task::join(id);
        }
        // The old code never finished here (30 s timeout); the IPI version needs a few ms locally and
        // ~75 ms on a loaded CI runner under TCG (bug #17) - 1 s keeps a wide margin on both sides.
        assert!(us < 1_000_000, "20 shootdowns took {} us", us);
    }

    #[test_case]
    fn threads_run_on_several_cpus_at_once() {
        use crate::task;
        use core::sync::atomic::AtomicU32;
        if online_count() < 2 {
            return; // KERNEL_SMP=1
        }
        let stop = Arc::new(AtomicBool::new(false));
        let cpus_seen = Arc::new(AtomicU32::new(0)); // bit mask
        let running = Arc::new(AtomicUsize::new(0));
        let ids: Vec<_> = (0..online_count())
            .map(|_| {
                let (stop, seen, running) = (stop.clone(), cpus_seen.clone(), running.clone());
                task::spawn("smp-spin", move || {
                    running.fetch_add(1, Ordering::SeqCst);
                    while !stop.load(Ordering::Relaxed) {
                        seen.fetch_or(1 << cpu_index(), Ordering::Relaxed);
                        core::hint::spin_loop();
                    }
                })
            })
            .collect();
        time::sleep_ms(200);
        stop.store(true, Ordering::Relaxed);
        for id in ids {
            task::join(id);
        }
        let seen = cpus_seen.load(Ordering::Relaxed);
        assert!(seen.count_ones() >= 2, "spinning threads only ran on CPUs {:#b}", seen);
        assert_eq!(running.load(Ordering::SeqCst), online_count());
    }

    #[test_case]
    fn trampoline_page_is_below_1mib() {
        let f = trampoline_frame().expect("no trampoline frame");
        assert!(f.as_u64() < 0x10_0000 && f.as_u64() >= 0x1000);
        // the identity mapping is gone again
        assert!(paging::translate_in(paging::kernel_root(), VirtAddr(f.as_u64())).is_none_or(|p| p == f));
    }
}
