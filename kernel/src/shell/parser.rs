//! Splits a command line into words, like a (very) small POSIX shell:
//! whitespace separates words, `"..."` and `'...'` group words, and a
//! backslash escapes the next character (outside single quotes).

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    UnterminatedQuote(char),
    TrailingBackslash,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            ParseError::UnterminatedQuote(q) => write!(f, "unterminated {} quote", q),
            ParseError::TrailingBackslash => write!(f, "trailing backslash"),
        }
    }
}

pub fn split(line: &str) -> Result<Vec<String>, ParseError> {
    let mut words = Vec::new();
    let mut current = String::new();
    // `in_word` distinguishes "no word yet" from an empty quoted word ("")
    let mut in_word = false;
    let mut chars = line.chars();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(core::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\\' => {
                current.push(chars.next().ok_or(ParseError::TrailingBackslash)?);
                in_word = true;
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err(ParseError::UnterminatedQuote('\'')),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => current.push(chars.next().ok_or(ParseError::UnterminatedQuote('"'))?),
                        Some(c) => current.push(c),
                        None => return Err(ParseError::UnterminatedQuote('"')),
                    }
                }
            }
            c => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(line: &str) -> Vec<String> {
        split(line).unwrap()
    }

    #[test_case]
    fn splits_on_whitespace() {
        assert_eq!(s("  echo   hello\tworld  "), ["echo", "hello", "world"]);
        assert!(s("   ").is_empty());
    }

    #[test_case]
    fn quotes_group_words() {
        assert_eq!(s(r#"echo "hello world" 'a  b'"#), ["echo", "hello world", "a  b"]);
        assert_eq!(s(r#"x"y z"w"#), ["xy zw"]);
        assert_eq!(s(r#"echo "" ''"#), ["echo", "", ""]);
    }

    #[test_case]
    fn backslash_escapes() {
        assert_eq!(s(r#"a\ b "c\"d" 'e\f'"#), ["a b", "c\"d", "e\\f"]);
    }

    #[test_case]
    fn errors() {
        assert_eq!(split("echo \"open"), Err(ParseError::UnterminatedQuote('"')));
        assert_eq!(split("echo 'open"), Err(ParseError::UnterminatedQuote('\'')));
        assert_eq!(split("echo \\"), Err(ParseError::TrailingBackslash));
    }

    #[test_case]
    fn unicode_words() {
        assert_eq!(s("echo Grüße äöü"), ["echo", "Grüße", "äöü"]);
    }
}
