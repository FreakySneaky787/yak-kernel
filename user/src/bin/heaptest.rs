//! Self-test of the user heap (`brk` + the allocator in ulib).
//! Prints every failed check; exit code = number of failures.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;
use ulib::{heap, println, sys};

ulib::entry!(main);

fn main(_args: ulib::Args) -> i32 {
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        if !ok {
            println!("FAIL: {}", what);
            failures += 1;
        }
    };

    let start = heap::program_break();
    check(
        start % 4096 == 0 && start > 0x40_0000,
        "the heap starts page-aligned after the program",
    );

    // one big vector: 8 MiB, written and verified
    let mut big: Vec<u64> = Vec::new();
    for i in 0..(1u64 << 20) {
        big.push(i * 3);
    }
    check(
        big.iter().enumerate().all(|(i, &v)| v == i as u64 * 3),
        "8 MiB vector content",
    );
    let grown = heap::program_break();
    check(grown >= start + 8 * 1024 * 1024, "brk grew for the big vector");
    drop(big);

    // freed memory is reused: the same amount again must not grow the heap
    let again: Vec<u64> = alloc::vec![7; 1 << 20];
    check(again.iter().all(|&v| v == 7), "second big vector");
    check(heap::program_break() == grown, "freed memory is reused");
    drop(again);

    // many small allocations of different sizes
    let mut map = BTreeMap::new();
    for i in 0..5000u32 {
        map.insert(i, alloc::format!("value number {}", i));
    }
    check(
        map.len() == 5000 && map[&4321] == "value number 4321",
        "BTreeMap with 5000 strings",
    );
    map.retain(|k, _| k % 2 == 0);
    let mut s = String::new();
    for (k, v) in map.iter().take(3) {
        s.push_str(&alloc::format!("{}={};", k, v.len()));
    }
    check(s == "0=14;2=14;4=14;", "iterate after removals");
    drop(map);

    // alignment requests are honoured
    for align in [16usize, 64, 4096] {
        let layout = Layout::from_size_align(100, align).unwrap();
        let p = unsafe { alloc::alloc::alloc(layout) };
        check(!p.is_null() && p as usize % align == 0, "aligned allocation");
        unsafe { alloc::alloc::dealloc(p, layout) };
    }

    // limits: more than the kernel allows fails cleanly
    let huge = Layout::from_size_align(512 * 1024 * 1024, 16).unwrap();
    // black_box: otherwise LLVM may drop the unused allocation and assume it succeeded
    let p = core::hint::black_box(unsafe { alloc::alloc::alloc(huge) });
    check(p.is_null(), "512 MiB allocation fails");
    if !p.is_null() {
        unsafe { alloc::alloc::dealloc(p, huge) };
    }
    check(sys::brk(0x7FFF_FFFF_0000) == -12, "brk into the stack -> ENOMEM");
    check(sys::brk(0x1000) == -12, "brk below the heap -> ENOMEM");

    if failures == 0 {
        println!("heaptest: all checks passed");
    }
    failures
}
