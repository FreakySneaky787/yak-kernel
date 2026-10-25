//! File name patterns: `*` (any characters) and `?` (one character).
//!
//! The expander marks *unquoted* wildcards with private-use characters
//! (`STAR`, `ANY`), so a quoted or escaped `*` stays a plain star. Names
//! starting with '.' only match a pattern that starts with '.'.

use alloc::string::String;
use alloc::vec::Vec;
use ulib::fs::{self, Dir};
use ulib::sys;

pub const STAR: char = '\u{E000}';
pub const ANY: char = '\u{E001}';

pub fn has_wildcards(word: &str) -> bool {
    word.contains([STAR, ANY])
}

/// The word with its wildcard marks turned back into `*` and `?`.
pub fn literal(word: &str) -> String {
    word.replace(STAR, "*").replace(ANY, "?")
}

/// Whether `name` matches `pattern` (with marks).
pub fn matches(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((&STAR, rest)) => (0..=name.len()).any(|skip| matches(rest, &name[skip..])),
        Some((&ANY, rest)) => !name.is_empty() && matches(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && matches(rest, &name[1..]),
    }
}

/// All existing paths matching `pattern`, sorted; relative ones start in `cwd`.
pub fn expand(pattern: &str, cwd: &str) -> Vec<String> {
    let parts: Vec<&str> = pattern.split('/').filter(|p| !p.is_empty()).collect();
    let mut paths = alloc::vec![String::from(if pattern.starts_with('/') { "/" } else { "" })];
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let mut next = Vec::new();
        for base in &paths {
            let join = |name: &str| match base.as_str() {
                "" => String::from(name),
                b if b.ends_with('/') => alloc::format!("{}{}", b, name),
                b => alloc::format!("{}/{}", b, name),
            };
            if !has_wildcards(part) {
                let path = join(part);
                if fs::stat(&path).is_ok_and(|st| last || st.kind == sys::KIND_DIR) {
                    next.push(path);
                }
                continue;
            }
            let pattern: Vec<char> = part.chars().collect();
            let dir = if base.is_empty() { cwd } else { base.as_str() };
            let Ok(entries) = Dir::open(dir) else { continue };
            for entry in entries.flatten() {
                let name = entry.name();
                if name.starts_with('.') && !part.starts_with('.') {
                    continue;
                }
                let chars: Vec<char> = name.chars().collect();
                if matches(&pattern, &chars) && (last || entry.is_dir()) {
                    next.push(join(name));
                }
            }
        }
        paths = next;
    }
    paths.sort();
    paths
}
