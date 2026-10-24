//! Sends one UDP datagram and prints the answer.
//! Usage: udpecho <ip> <port> <text...>   (QEMU: 10.0.2.2 = the host's 127.0.0.1)

#![no_std]
#![no_main]

extern crate alloc;

use ulib::{eprintln, println, sys};

ulib::entry!(main);

fn main(mut args: ulib::Args) -> i32 {
    let (Some(ip), Some(port)) = (
        args.next().and_then(sys::parse_ip),
        args.next().and_then(|p| p.parse::<u16>().ok()),
    ) else {
        eprintln!("usage: udpecho <ip> <port> <text...>");
        return 2;
    };
    let text = args.collect::<alloc::vec::Vec<_>>().join(" ");
    let fd = sys::udp_socket(0);
    if fd < 0 {
        eprintln!("udpecho: socket: {}", ulib::fs::Error(-fd));
        return 1;
    }
    let fd = fd as u64;
    let sent = sys::sendto(fd, text.as_bytes(), ip, port);
    if sent < 0 {
        eprintln!("udpecho: send: {}", ulib::fs::Error(-sent));
        return 1;
    }
    let mut buf = [0u8; 1500];
    let mut from = sys::SockAddr::default();
    let n = sys::recvfrom(fd, &mut buf, &mut from, 3000);
    sys::close(fd);
    if n < 0 {
        eprintln!("udpecho: no answer: {}", ulib::fs::Error(-n));
        return 1;
    }
    let [a, b, c, d] = from.ip;
    println!(
        "reply from {}.{}.{}.{}:{}: {}",
        a,
        b,
        c,
        d,
        from.port,
        core::str::from_utf8(&buf[..n as usize]).unwrap_or("<binary>")
    );
    0
}
