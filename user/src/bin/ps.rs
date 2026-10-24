//! Lists all threads: kernel threads and user processes.

#![no_std]
#![no_main]

use ulib::{println, sys};

ulib::entry!(main);

fn state_name(state: u32) -> &'static str {
    match state {
        sys::PROC_RUNNING => "running",
        sys::PROC_READY => "ready",
        sys::PROC_SLEEPING => "sleeping",
        sys::PROC_WAIT_INPUT => "wait input",
        sys::PROC_JOIN => "wait",
        sys::PROC_WAIT_PIPE => "wait pipe",
        sys::PROC_DEAD => "dead",
        _ => "?",
    }
}

fn main(_args: ulib::Args) -> i32 {
    let mut list = [sys::ProcInfo {
        tid: 0,
        cpu_ticks: 0,
        parent: 0,
        state: 0,
        user: 0,
        name: [0; 32],
    }; 64];
    let n = sys::proclist(&mut list);
    let total: u64 = list[..n].iter().map(|p| p.cpu_ticks).sum::<u64>().max(1);
    println!("  TID  PARENT  KIND    STATE         CPU    NAME");
    for p in &list[..n] {
        println!(
            "  {:>3}  {:>6}  {:<6}  {:<12} {:>3}%    {}",
            p.tid,
            p.parent,
            if p.user == 1 { "user" } else { "kernel" },
            state_name(p.state),
            p.cpu_ticks * 100 / total,
            sys::field_str(&p.name)
        );
    }
    0
}
