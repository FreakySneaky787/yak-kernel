//! Splits a command line into words: spaces separate them, and quotes keep spaces inside one word -
//! `echo 'a  b' c` is the three words `echo`, `a  b` and `c`.

use alloc::string::String;
use alloc::vec::Vec;

/// A quote was opened and never closed - the line cannot be split.
#[derive(Debug, PartialEq, Eq)]
pub struct UnclosedQuote(pub char);

/// The words of `line`. Inside `"…"` or `'…'` a space is part of the word; the quotes themselves are not.
pub fn split(line: &str) -> Result<Vec<String>, UnclosedQuote> {
    let mut words = Vec::new();
    let mut word = String::new();
    // a word can be empty but still there: `''` is one empty word, two spaces are none
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match (quote, c) {
            // inside quotes: everything up to the matching quote belongs to the word
            (Some(open), c) if c == open => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, ' ') => {
                if in_word {
                    words.push(core::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if let Some(open) = quote {
        return Err(UnclosedQuote(open));
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        split(line).unwrap()
    }

    #[test_case]
    fn spaces_separate_words() {
        assert_eq!(words("echo  hello   world "), ["echo", "hello", "world"]);
        assert!(words("").is_empty());
        assert!(words("    ").is_empty());
    }

    #[test_case]
    fn quotes_keep_spaces_in_a_word() {
        assert_eq!(words("echo 'a  b' c"), ["echo", "a  b", "c"]);
        assert_eq!(words("say \"it's\" x'y z'"), ["say", "it's", "xy z"]);
        assert_eq!(words("'' \"\""), ["", ""]); // two empty words
    }

    #[test_case]
    fn an_unclosed_quote_is_an_error() {
        assert_eq!(split("echo 'oops"), Err(UnclosedQuote('\'')));
        assert_eq!(split("echo \"it's"), Err(UnclosedQuote('"')));
    }
}
