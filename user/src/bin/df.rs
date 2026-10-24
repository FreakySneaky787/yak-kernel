//! Size and free space of the file system.

#![no_std]
#![no_main]

use ulib::{println, sys};

ulib::entry!(main);

fn main(_args: ulib::Args) -> i32 {
    const KIB: u64 = 1024;
    let i = sys::sysinfo();
    let (total, free) = (i.fs_total, i.fs_free);
    println!("FAT32 volume '{}' on the system disk", sys::field_str(&i.fs_label));
    println!(
        "  size {} KiB, used {} KiB, free {} KiB ({} % used), cluster size {} bytes",
        total / KIB,
        (total - free) / KIB,
        free / KIB,
        (total - free) * 100 / total.max(1),
        i.fs_cluster_size
    );
    0
}
