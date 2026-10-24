//! TCP echo server: sends every byte back. Usage: echod [port] [-n count]
//! (default port 7; with -n it stops after <count> connections)

#![no_std]
#![no_main]

use ulib::{eprintln, sys};

ulib::entry!(main);

fn main(args: ulib::Args) -> i32 {
    let mut port = 7u16;
    let mut limit: Option<u32> = None;
    let mut it = args;
    while let Some(a) = it.next() {
        match a {
            "-n" => limit = it.next().and_then(|n| n.parse().ok()),
            p => port = p.parse().unwrap_or(port),
        }
    }
    let listener = sys::tcp_listen(port);
    if listener < 0 {
        eprintln!("echod: listen on {}: {}", port, ulib::fs::Error(-listener));
        return 1;
    }
    eprintln!("echod: listening on port {}", port);
    let mut served = 0u32;
    while limit.is_none_or(|l| served < l) {
        let mut from = sys::SockAddr::default();
        let fd = sys::tcp_accept(listener as u64, &mut from);
        if fd < 0 {
            eprintln!("echod: accept: {}", ulib::fs::Error(-fd));
            return 1;
        }
        let fd = fd as u64;
        let [a, b, c, d] = from.ip;
        let mut total = 0usize;
        let mut buf = [0u8; 4096];
        loop {
            let n = sys::read(fd, &mut buf);
            if n <= 0 {
                break;
            }
            if sys::write(fd, &buf[..n as usize]) < 0 {
                break;
            }
            total += n as usize;
        }
        sys::close(fd);
        served += 1;
        eprintln!("echod: {}.{}.{}.{}:{} - echoed {} bytes", a, b, c, d, from.port, total);
    }
    eprintln!("echod: served {} connection(s)", served);
    0
}
