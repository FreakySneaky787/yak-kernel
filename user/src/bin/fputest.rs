//! Self-test of the per-thread x87/SSE state. `fputest` starts four workers
//! (`fputest worker <n>`) that run at the same time and are preempted all the
//! time. Each one loads its own pattern into all 16 XMM registers and its own
//! rounding mode into MXCSR, keeps spinning, and checks after every round
//! that nothing changed - another process's values would show up here if the
//! kernel did not save and restore the registers. With AVX (XSAVE) they do
//! the same with all 16 YMM registers (256 bit). They also compute pi with
//! hardware floating point. Exit code = number of failures.

#![no_std]
#![no_main]

extern crate alloc;

use core::arch::asm;
use ulib::process::{self, ExitStatus};
use ulib::{println, sys};

ulib::entry!(main);

const WORKERS: u64 = 4;
const ROUNDS: usize = 40;

/// Loads 16 × 16 bytes into XMM0..XMM15.
fn load_xmm(regs: &[u8; 256]) {
    unsafe {
        asm!(
            "movdqu xmm0, [{p}]", "movdqu xmm1, [{p} + 16]", "movdqu xmm2, [{p} + 32]", "movdqu xmm3, [{p} + 48]",
            "movdqu xmm4, [{p} + 64]", "movdqu xmm5, [{p} + 80]", "movdqu xmm6, [{p} + 96]", "movdqu xmm7, [{p} + 112]",
            "movdqu xmm8, [{p} + 128]", "movdqu xmm9, [{p} + 144]", "movdqu xmm10, [{p} + 160]", "movdqu xmm11, [{p} + 176]",
            "movdqu xmm12, [{p} + 192]", "movdqu xmm13, [{p} + 208]", "movdqu xmm14, [{p} + 224]", "movdqu xmm15, [{p} + 240]",
            p = in(reg) regs.as_ptr(),
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _, out("xmm4") _, out("xmm5") _,
            out("xmm6") _, out("xmm7") _, out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
            options(nostack, readonly),
        )
    }
}

/// Spins for a while without touching memory or XMM registers, then stores
/// XMM0..XMM15. Timer interrupts preempt us in the middle of the loop.
fn spin_and_store_xmm(out: &mut [u8; 256], spins: u64) {
    unsafe {
        asm!(
            "2:", "dec {n}", "jnz 2b",
            "movdqu [{p}], xmm0", "movdqu [{p} + 16], xmm1", "movdqu [{p} + 32], xmm2", "movdqu [{p} + 48], xmm3",
            "movdqu [{p} + 64], xmm4", "movdqu [{p} + 80], xmm5", "movdqu [{p} + 96], xmm6", "movdqu [{p} + 112], xmm7",
            "movdqu [{p} + 128], xmm8", "movdqu [{p} + 144], xmm9", "movdqu [{p} + 160], xmm10", "movdqu [{p} + 176], xmm11",
            "movdqu [{p} + 192], xmm12", "movdqu [{p} + 208], xmm13", "movdqu [{p} + 224], xmm14", "movdqu [{p} + 240], xmm15",
            p = in(reg) out.as_mut_ptr(),
            n = inout(reg) spins.max(1) => _,
            options(nostack),
        )
    }
}

/// Loads 16 × 32 bytes into YMM0..YMM15, spins (preempted meanwhile) and
/// stores them again - in one asm block, so the compiler cannot touch them.
#[target_feature(enable = "avx")]
unsafe fn ymm_round_trip(regs: &[u8; 512], out: &mut [u8; 512], spins: u64) {
    unsafe {
        asm!(
            "vmovdqu ymm0, [{p} + 0]", "vmovdqu ymm1, [{p} + 32]", "vmovdqu ymm2, [{p} + 64]", "vmovdqu ymm3, [{p} + 96]", "vmovdqu ymm4, [{p} + 128]", "vmovdqu ymm5, [{p} + 160]", "vmovdqu ymm6, [{p} + 192]", "vmovdqu ymm7, [{p} + 224]", "vmovdqu ymm8, [{p} + 256]", "vmovdqu ymm9, [{p} + 288]", "vmovdqu ymm10, [{p} + 320]", "vmovdqu ymm11, [{p} + 352]", "vmovdqu ymm12, [{p} + 384]", "vmovdqu ymm13, [{p} + 416]", "vmovdqu ymm14, [{p} + 448]", "vmovdqu ymm15, [{p} + 480]",
            "2:", "dec {n}", "jnz 2b",
            "mov {p}, {o}",
            "vmovdqu [{p} + 0], ymm0", "vmovdqu [{p} + 32], ymm1", "vmovdqu [{p} + 64], ymm2", "vmovdqu [{p} + 96], ymm3", "vmovdqu [{p} + 128], ymm4", "vmovdqu [{p} + 160], ymm5", "vmovdqu [{p} + 192], ymm6", "vmovdqu [{p} + 224], ymm7", "vmovdqu [{p} + 256], ymm8", "vmovdqu [{p} + 288], ymm9", "vmovdqu [{p} + 320], ymm10", "vmovdqu [{p} + 352], ymm11", "vmovdqu [{p} + 384], ymm12", "vmovdqu [{p} + 416], ymm13", "vmovdqu [{p} + 448], ymm14", "vmovdqu [{p} + 480], ymm15",
            p = inout(reg) regs.as_ptr() => _,
            o = in(reg) out.as_mut_ptr(),
            n = inout(reg) spins.max(1) => _,
            out("ymm0") _, out("ymm1") _, out("ymm2") _, out("ymm3") _, out("ymm4") _, out("ymm5") _, out("ymm6") _, out("ymm7") _, out("ymm8") _, out("ymm9") _, out("ymm10") _, out("ymm11") _, out("ymm12") _, out("ymm13") _, out("ymm14") _, out("ymm15") _,
            options(nostack),
        );
    }
}

fn mxcsr() -> u32 {
    let mut v = 0u32;
    unsafe { asm!("stmxcsr [{}]", in(reg) &mut v, options(nostack)) };
    v
}

fn set_mxcsr(v: u32) {
    unsafe { asm!("ldmxcsr [{}]", in(reg) &v, options(nostack, readonly)) };
}

/// Leibniz series: 4 * (1 - 1/3 + 1/5 - ...).
fn pi(terms: u32) -> f64 {
    let mut sum = 0.0f64;
    for k in 0..terms {
        let term = 1.0 / (2 * k + 1) as f64;
        sum += if k % 2 == 0 { term } else { -term };
    }
    4.0 * sum
}

fn worker(n: u64) -> i32 {
    let mut pattern = [0u8; 256];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (n as u8).wrapping_mul(37).wrapping_add(i as u8);
    }
    // a different rounding mode per worker (bits 13-14 of MXCSR)
    let my_mxcsr = 0x1F80 | ((n as u32 & 3) << 13);
    set_mxcsr(my_mxcsr);
    let mut seen = [0u8; 256];
    for round in 0..ROUNDS {
        load_xmm(&pattern);
        spin_and_store_xmm(&mut seen, 2_000_000);
        if seen != pattern {
            println!("worker {}: XMM registers changed in round {}", n, round);
            return 1;
        }
        if mxcsr() != my_mxcsr {
            println!("worker {}: MXCSR changed to {:#x} in round {}", n, mxcsr(), round);
            return 1;
        }
        if round % 8 == 0 {
            sys::yield_now(); // also switch voluntarily
        }
    }
    if ulib::avx_enabled() {
        let mut ymm = [0u8; 512];
        for (i, b) in ymm.iter_mut().enumerate() {
            *b = (n as u8).wrapping_mul(91).wrapping_add((i * 7) as u8);
        }
        let mut seen = [0u8; 512];
        for round in 0..ROUNDS / 2 {
            unsafe { ymm_round_trip(&ymm, &mut seen, 2_000_000) };
            if seen != ymm {
                println!("worker {}: YMM registers changed in round {}", n, round);
                return 1;
            }
        }
    }
    set_mxcsr(0x1F80);
    let p = pi(200_000);
    if (p - 3.141_587_653_589_8).abs() > 1e-9 {
        println!("worker {}: pi = {} is wrong", n, p);
        return 1;
    }
    0
}

fn main(mut args: ulib::Args) -> i32 {
    if args.next() == Some("worker") {
        return worker(args.next().and_then(|n| n.parse().ok()).unwrap_or(0));
    }
    let ns = ["1", "2", "3", "4"];
    let pids: alloc::vec::Vec<u64> = ns[..WORKERS as usize]
        .iter()
        .filter_map(|n| process::spawn("/bin/fputest", ["worker", n]).ok())
        .collect();
    let mut failures = (WORKERS as usize - pids.len()) as i32;
    for pid in pids {
        if process::wait(pid) != Ok(ExitStatus::Exited(0)) {
            failures += 1;
        }
    }
    let x = 1.5f64 * 2.25;
    if x != 3.375 {
        failures += 1;
    }
    if failures == 0 {
        let regs = if ulib::avx_enabled() { "XMM + YMM" } else { "XMM" };
        println!(
            "fputest: all checks passed (4 workers, {} + MXCSR kept across preemption, pi = {:.6})",
            regs,
            pi(200_000)
        );
    }
    failures
}
