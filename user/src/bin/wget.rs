//! Fetches a URL over HTTP/1.0. Usage: wget [-O file] http://host[:port]/path
//! The body goes to stdout (or the file); status and size to stderr.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ulib::fs::File;
use ulib::{dns, eprintln, io, sys};

ulib::entry!(main);

/// (host, port, path) of an http:// URL.
fn parse_url(url: &str) -> Option<(&str, u16, &str)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, 80),
    };
    (!host.is_empty()).then_some((host, port, path))
}

fn write_all(fd: u64, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let n = sys::write(fd, data);
        if n <= 0 {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

fn main(args: ulib::Args) -> i32 {
    let mut out_file: Option<&str> = None;
    let mut url = None;
    let mut it = args;
    while let Some(a) = it.next() {
        match a {
            "-O" => out_file = it.next(),
            u => url = Some(u),
        }
    }
    let Some((host, port, path)) = url.and_then(parse_url) else {
        eprintln!("usage: wget [-O file] http://host[:port]/path");
        return 2;
    };
    let ip = match dns::resolve(host, dns::default_server(), 53) {
        Ok(ip) => ip,
        Err(e) => {
            eprintln!("wget: {}: {}", host, e);
            return 1;
        }
    };
    let fd = sys::tcp_connect(ip, port, 5000);
    if fd < 0 {
        eprintln!("wget: connect to {}:{}: {}", host, port, ulib::fs::Error(-fd));
        return 1;
    }
    let fd = fd as u64;
    let request = alloc::format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: KernelTry-wget\r\n\r\n",
        path,
        host
    );
    write_all(fd, request.as_bytes());
    let response = io::read_all(fd);
    sys::close(fd);
    let Some(split) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
        eprintln!("wget: no HTTP response");
        return 1;
    };
    let head = String::from_utf8_lossy(&response[..split]);
    let body: Vec<u8> = response[split + 4..].to_vec();
    let status_line = head.lines().next().unwrap_or("");
    let code: u16 = status_line.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    eprintln!("wget: {} - {} bytes", status_line, body.len());
    if code != 200 {
        return 1;
    }
    match out_file {
        Some(name) => match File::create(name).and_then(|mut f| f.write(&body)) {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("wget: {}: {}", name, e);
                1
            }
        },
        None => {
            write_all(sys::STDOUT, &body);
            0
        }
    }
}
