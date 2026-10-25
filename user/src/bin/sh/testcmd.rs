//! `test expression` / `[ expression ]`: 0 = true, 1 = false, 2 = error.
//!
//! - strings: `-n s`, `-z s`, `s1 = s2`, `s1 != s2`, `s` (non-empty)
//! - numbers: `-eq -ne -lt -le -gt -ge`
//! - files: `-e` (exists), `-f` (file), `-d` (directory), `-s` (not empty)
//! - `! expression`

use alloc::string::String;
use ulib::{fs, sys};

pub fn run(name: &str, args: &[String]) -> Result<i32, String> {
    let mut args: alloc::vec::Vec<&str> = args.iter().map(String::as_str).collect();
    if name == "[" {
        if args.last() != Some(&"]") {
            return Err(String::from("[: missing ']'"));
        }
        args.pop();
    }
    eval(&args).map(|truth| if truth { 0 } else { 1 })
}

fn eval(a: &[&str]) -> Result<bool, String> {
    match a {
        [] => Ok(false),
        ["!", rest @ ..] => Ok(!eval(rest)?),
        [s] => Ok(!s.is_empty()),
        [op, s] => unary(op, s),
        [l, op, r] => binary(l, op, r),
        _ => Err(String::from("test: too many arguments")),
    }
}

fn unary(op: &str, s: &str) -> Result<bool, String> {
    let stat = || fs::stat(s).ok();
    Ok(match op {
        "-n" => !s.is_empty(),
        "-z" => s.is_empty(),
        "-e" => stat().is_some(),
        "-f" => stat().is_some_and(|st| st.kind == sys::KIND_FILE),
        "-d" => stat().is_some_and(|st| st.kind == sys::KIND_DIR),
        "-s" => stat().is_some_and(|st| st.size > 0),
        _ => return Err(alloc::format!("test: unknown operator '{}'", op)),
    })
}

fn binary(l: &str, op: &str, r: &str) -> Result<bool, String> {
    let num = |s: &str| {
        s.trim()
            .parse::<i64>()
            .map_err(|_| alloc::format!("test: '{}' is not a number", s))
    };
    Ok(match op {
        "=" | "==" => l == r,
        "!=" => l != r,
        "-eq" => num(l)? == num(r)?,
        "-ne" => num(l)? != num(r)?,
        "-lt" => num(l)? < num(r)?,
        "-le" => num(l)? <= num(r)?,
        "-gt" => num(l)? > num(r)?,
        "-ge" => num(l)? >= num(r)?,
        _ => return Err(alloc::format!("test: unknown operator '{}'", op)),
    })
}
