//! Looks up the IPv4 address of a name. Usage: host <name> [server [port]] | host --selftest

#![no_std]
#![no_main]

use ulib::{dns, eprintln, println, sys};

ulib::entry!(main);

fn main(mut args: ulib::Args) -> i32 {
    let Some(name) = args.next() else {
        eprintln!("usage: host <name> [server [port]] | host --selftest");
        return 2;
    };
    if name == "--selftest" {
        let failed = dns::selftest();
        for f in &failed {
            println!("FAIL: {}", f);
        }
        if failed.is_empty() {
            println!("host selftest: all checks passed");
        }
        return failed.len() as i32;
    }
    let server = args.next().and_then(sys::parse_ip).unwrap_or_else(dns::default_server);
    let port = args.next().and_then(|p| p.parse().ok()).unwrap_or(53);
    match dns::resolve(name, server, port) {
        Ok([a, b, c, d]) => {
            println!("{} has address {}.{}.{}.{}", name, a, b, c, d);
            0
        }
        Err(e) => {
            eprintln!("host: {}: {}", name, e);
            1
        }
    }
}
