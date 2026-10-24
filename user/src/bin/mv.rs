//! Renames or moves a file or directory. Usage: mv <from> <to>
//! If <to> is a directory, <from> is moved into it.

#![no_std]
#![no_main]

extern crate alloc;

use ulib::{eprintln, fs, sys};

ulib::entry!(main);

fn main(mut args: ulib::Args) -> i32 {
    let (Some(from), Some(to), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: mv <from> <to>");
        return 2;
    };
    let target = match fs::stat(to) {
        Ok(st) if st.kind == sys::KIND_DIR => {
            let name = from.trim_end_matches('/').rsplit('/').next().unwrap_or(from);
            alloc::format!("{}/{}", to.trim_end_matches('/'), name)
        }
        _ => alloc::string::String::from(to),
    };
    match fs::rename(from, &target) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mv: {}: {}", from, e);
            1
        }
    }
}
