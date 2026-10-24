//! Sends ICMP echo requests and prints the replies. Usage: ping <ip> [count]

#![no_std]
#![no_main]

use ulib::{eprintln, println, sys};

ulib::entry!(main);

fn main(mut args: ulib::Args) -> i32 {
    let Some(ip) = args.next().and_then(sys::parse_ip) else {
        eprintln!("usage: ping <ip> [count]");
        return 2;
    };
    let count: u16 = args.next().and_then(|c| c.parse().ok()).unwrap_or(4);
    let [a, b, c, d] = ip;
    let mut received = 0;
    for seq in 1..=count {
        match sys::ping(ip, seq, 2000) {
            us if us >= 0 => {
                received += 1;
                println!(
                    "reply from {}.{}.{}.{}: seq={} time={}.{:03} ms",
                    a,
                    b,
                    c,
                    d,
                    seq,
                    us / 1000,
                    us % 1000
                );
            }
            e => println!(
                "no reply from {}.{}.{}.{}: seq={} ({})",
                a,
                b,
                c,
                d,
                seq,
                ulib::fs::Error(-e)
            ),
        }
        if seq < count {
            sys::sleep_ms(500);
        }
    }
    println!("{} packets sent, {} received", count, received);
    if received == 0 { 1 } else { 0 }
}
