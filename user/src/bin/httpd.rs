//! A tiny HTTP/1.0 server for the files below the current directory.
//! Usage: httpd [port] [-n count]   (default port 80; -n: stop after <count> requests)

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ulib::fs::{self, Dir};
use ulib::{eprintln, io, sys};

ulib::entry!(main);

fn write_all(fd: u64, mut data: &[u8]) {
    while !data.is_empty() {
        let n = sys::write(fd, data);
        if n <= 0 {
            return;
        }
        data = &data[n as usize..];
    }
}

fn respond(fd: u64, status: &str, content_type: &str, body: &[u8]) {
    let head = alloc::format!(
        "HTTP/1.0 {}\r\nServer: YAK-httpd\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        content_type,
        body.len()
    );
    write_all(fd, head.as_bytes());
    write_all(fd, body);
}

/// Reads the request head (up to the empty line, at most 4 KiB).
fn read_request(fd: u64) -> Vec<u8> {
    let mut req = Vec::new();
    let mut buf = [0u8; 512];
    while req.len() < 4096 && !req.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = sys::read(fd, &mut buf);
        if n <= 0 {
            break;
        }
        req.extend_from_slice(&buf[..n as usize]);
    }
    req
}

/// Status, content type and body for `path` (relative to the cwd).
fn serve(path: &str) -> (&'static str, &'static str, Vec<u8>) {
    let local = alloc::format!(".{}", path);
    if path.contains("..") {
        return ("403 Forbidden", "text/plain", b"forbidden\n".to_vec());
    }
    match fs::stat(&local) {
        Ok(st) if st.kind == sys::KIND_DIR => {
            let mut listing = String::new();
            if let Ok(dir) = Dir::open(&local) {
                for e in dir.flatten() {
                    listing.push_str(e.name());
                    if e.is_dir() {
                        listing.push('/');
                    }
                    listing.push('\n');
                }
            }
            ("200 OK", "text/plain", listing.into_bytes())
        }
        Ok(_) => match io::read_input(Some(&local)) {
            Ok(data) => {
                let kind = if path.ends_with(".html") {
                    "text/html"
                } else {
                    "text/plain"
                };
                ("200 OK", kind, data)
            }
            Err(_) => ("500 Internal Server Error", "text/plain", b"cannot read\n".to_vec()),
        },
        Err(_) => ("404 Not Found", "text/plain", b"not found\n".to_vec()),
    }
}

fn main(args: ulib::Args) -> i32 {
    let mut port = 80u16;
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
        eprintln!("httpd: listen on {}: {}", port, ulib::fs::Error(-listener));
        return 1;
    }
    eprintln!("httpd: serving the current directory on port {}", port);
    let mut served = 0u32;
    while limit.is_none_or(|l| served < l) {
        let mut from = sys::SockAddr::default();
        let fd = sys::tcp_accept(listener as u64, &mut from);
        if fd < 0 {
            eprintln!("httpd: accept: {}", ulib::fs::Error(-fd));
            return 1;
        }
        let fd = fd as u64;
        let request = read_request(fd);
        let line = String::from_utf8_lossy(&request);
        let mut words = line.lines().next().unwrap_or("").split(' ');
        let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or("/"));
        let (status, kind, body) = if method == "GET" {
            serve(path)
        } else {
            ("405 Method Not Allowed", "text/plain", b"only GET\n".to_vec())
        };
        respond(fd, status, kind, &body);
        sys::close(fd);
        served += 1;
        let [a, b, c, d] = from.ip;
        eprintln!(
            "httpd: {}.{}.{}.{} {} {} -> {} ({} bytes)",
            a,
            b,
            c,
            d,
            method,
            path,
            status,
            body.len()
        );
    }
    0
}
