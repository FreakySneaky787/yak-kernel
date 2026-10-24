//! A small netcat: connects to <ip>:<port>, sends the text from the command
//! line (plus a newline) or everything from stdin, closes its sending side
//! and prints what comes back until the other side closes.
//! Usage: nc <ip> <port> [text...]

#![no_std]
#![no_main]

extern crate alloc;

use ulib::{eprintln, io, sys};

ulib::entry!(main);

fn send_all(fd: u64, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let n = sys::write(fd, data);
        if n <= 0 {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

fn main(mut args: ulib::Args) -> i32 {
    let (Some(ip), Some(port)) = (
        args.next().and_then(sys::parse_ip),
        args.next().and_then(|p| p.parse::<u16>().ok()),
    ) else {
        eprintln!("usage: nc <ip> <port> [text...]");
        return 2;
    };
    let words: alloc::vec::Vec<&str> = args.collect();
    let fd = sys::tcp_connect(ip, port, 5000);
    if fd < 0 {
        eprintln!("nc: connect: {}", ulib::fs::Error(-fd));
        return 1;
    }
    let fd = fd as u64;
    let ok = if words.is_empty() {
        send_all(fd, &io::read_stdin())
    } else {
        let mut text = words.join(" ");
        text.push('\n');
        send_all(fd, text.as_bytes())
    };
    if !ok {
        eprintln!("nc: send failed");
    }
    sys::shutdown(fd);
    let mut buf = [0u8; 4096];
    loop {
        match sys::read(fd, &mut buf) {
            0 => break,
            n if n > 0 => {
                sys::write(sys::STDOUT, &buf[..n as usize]);
            }
            e => {
                eprintln!("nc: receive: {}", ulib::fs::Error(-e));
                sys::close(fd);
                return 1;
            }
        }
    }
    sys::close(fd);
    0
}
