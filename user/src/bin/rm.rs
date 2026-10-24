//! Removes files and empty directories. Usage: rm <path>...

#![no_std]
#![no_main]

extern crate alloc;

use ulib::{eprintln, fs, sys};

ulib::entry!(main);

/// Absolute form of `path` relative to `cwd` (enough for the comparison below).
fn absolute(cwd: &str, path: &str) -> alloc::string::String {
    let mut parts: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    let start = if path.starts_with('/') { "" } else { cwd };
    for part in start.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = alloc::string::String::new();
    for p in parts {
        out.push('/');
        out.push_str(p);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

fn main(args: ulib::Args) -> i32 {
    let mut buf = [0u8; 1024];
    let cwd = alloc::string::String::from(fs::cwd(&mut buf).unwrap_or("/"));
    let mut status = 0;
    let mut any = false;
    for path in args {
        any = true;
        let abs = absolute(&cwd, path);
        if cwd == abs || cwd.starts_with(&alloc::format!("{}/", abs)) || abs == "/" {
            eprintln!(
                "rm: {}: cannot remove the current directory or one of its parents",
                path
            );
            status = 1;
            continue;
        }
        let r = sys::unlink(path);
        if r < 0 {
            eprintln!("rm: {}: {}", path, fs::Error(-r));
            status = 1;
        }
    }
    if !any {
        eprintln!("usage: rm <path>...");
        return 2;
    }
    status
}
