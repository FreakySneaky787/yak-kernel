//! Prints the environment variables, or the value of one. Usage: env [NAME]

#![no_std]
#![no_main]

use ulib::{env, println};

ulib::entry!(main);

fn main(mut args: ulib::Args) -> i32 {
    if let Some(name) = args.next() {
        return match env::var(name) {
            Some(v) => {
                println!("{}", v);
                0
            }
            None => 1,
        };
    }
    for (k, v) in env::vars() {
        println!("{}={}", k, v);
    }
    0
}
