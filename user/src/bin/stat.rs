//! Shows type, size and modification time of files. Usage: stat <path>...

#![no_std]
#![no_main]

use ulib::fs::{self, Timestamp};
use ulib::{eprintln, println, sys};

ulib::entry!(main);

fn main(args: ulib::Args) -> i32 {
    let mut status = 0;
    for path in args {
        match fs::stat(path) {
            Ok(st) => {
                println!("  path:      {}", path);
                println!(
                    "  type:      {}",
                    if st.kind == sys::KIND_DIR { "directory" } else { "file" }
                );
                println!("  size:      {} bytes", st.size);
                println!("  modified:  {}", Timestamp(st.modified));
            }
            Err(e) => {
                eprintln!("stat: {}: {}", path, e);
                status = 1;
            }
        }
    }
    status
}
