//! The shell grammar: splits a script into tokens and parses them into
//! lists of and-or chains of pipelines of commands.
//!
//! Words stay *raw* (quotes and `$` included): they are expanded only when a
//! command runs, so a loop sees new variable values in every round.
//!
//! ```text
//! list     := and_or ((';' | '\n' | '&') and_or)*
//! and_or   := pipeline (('&&' | '||') pipeline)*
//! pipeline := ['!'] command ('|' command)*
//! command  := simple | compound [redirections] | NAME '()' '{' list '}'
//! compound := 'if' list 'then' list ('elif' list 'then' list)* ['else' list] 'fi'
//!           | ('while' | 'until') list 'do' list 'done'
//!           | 'for' NAME ['in' words] (';' | '\n') 'do' list 'done'
//!           | '{' list '}'
//!           | 'case' word 'in' (['('] pattern ('|' pattern)* ')' list [';;'])* 'esac'
//! ```
//!
//! Keywords count only at the start of a command (`echo fi` prints "fi").
//! `;`, newlines and `#` comments work anywhere outside quotes; `&`, `&&`,
//! `||` and `|` must be separate words. `$((...))` and `$(...)` (command
//! substitution, M55) are always part of one word.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Word(String),
    Semi,
    /// `;;` (ends a `case` branch)
    DoubleSemi,
    Newline,
    Amp,
}

#[derive(Debug, PartialEq)]
pub enum SyntaxError {
    /// The text ends inside a quote or a compound command: more lines needed.
    Incomplete,
    Unexpected(String),
    Message(&'static str),
}

impl core::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            SyntaxError::Incomplete => write!(f, "unexpected end of input"),
            SyntaxError::Unexpected(w) => write!(f, "syntax error near '{}'", w),
            SyntaxError::Message(m) => write!(f, "syntax error: {}", m),
        }
    }
}

/// Splits `text` into words and separators. Quotes and backslashes stay in
/// the words; `\` + newline joins two lines.
pub fn tokenize(text: &str) -> Result<Vec<Token>, SyntaxError> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut i = 0;
    let end_word = |word: &mut String, in_word: &mut bool, tokens: &mut Vec<Token>| {
        if *in_word {
            tokens.push(Token::Word(core::mem::take(word)));
            *in_word = false;
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                end_word(&mut word, &mut in_word, &mut tokens);
                tokens.push(Token::Newline);
            }
            ';' if chars.get(i + 1) == Some(&';') => {
                end_word(&mut word, &mut in_word, &mut tokens);
                tokens.push(Token::DoubleSemi);
                i += 1;
            }
            ';' => {
                end_word(&mut word, &mut in_word, &mut tokens);
                tokens.push(Token::Semi);
            }
            c if c.is_whitespace() => end_word(&mut word, &mut in_word, &mut tokens),
            '#' if !in_word => {
                while i + 1 < chars.len() && chars[i + 1] != '\n' {
                    i += 1;
                }
            }
            '&' if !in_word && chars.get(i + 1).is_none_or(|n| n.is_whitespace() || *n == ';') => {
                tokens.push(Token::Amp);
            }
            '\\' => match chars.get(i + 1) {
                Some('\n') => i += 1, // line continuation
                Some(&next) => {
                    word.push(c);
                    word.push(next);
                    in_word = true;
                    i += 1;
                }
                None => return Err(SyntaxError::Incomplete),
            },
            '\'' | '"' => {
                in_word = true;
                word.push(c);
                loop {
                    i += 1;
                    match chars.get(i) {
                        None => return Err(SyntaxError::Incomplete),
                        Some(&q) if q == c => {
                            word.push(q);
                            break;
                        }
                        Some('\\') if c == '"' => {
                            word.push('\\');
                            i += 1;
                            match chars.get(i) {
                                Some(&n) => word.push(n),
                                None => return Err(SyntaxError::Incomplete),
                            }
                        }
                        Some('$') if c == '"' && chars.get(i + 1) == Some(&'(') => {
                            // "...$(...)...": quotes inside the command do not end ours
                            let end = subst_end(&chars, i + 2).ok_or(SyntaxError::Incomplete)?;
                            word.extend(&chars[i..=end]);
                            i = end;
                        }
                        Some(&other) => word.push(other),
                    }
                }
            }
            '$' if chars.get(i + 1) == Some(&'(') && chars.get(i + 2) == Some(&'(') => {
                // `$((...))` is one word, spaces and operators included
                let mut depth = 0;
                in_word = true;
                word.push_str("$((");
                i += 3;
                loop {
                    match chars.get(i) {
                        None => return Err(SyntaxError::Incomplete),
                        Some(')') if depth == 0 && chars.get(i + 1) == Some(&')') => {
                            word.push_str("))");
                            i += 1;
                            break;
                        }
                        Some(&p) => {
                            match p {
                                '(' => depth += 1,
                                ')' => depth -= 1,
                                _ => {}
                            }
                            word.push(p);
                        }
                    }
                    i += 1;
                }
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                // `$(...)`: the command text stays in the word, expanded when the command runs
                let end = subst_end(&chars, i + 2).ok_or(SyntaxError::Incomplete)?;
                word.extend(&chars[i..=end]);
                in_word = true;
                i = end;
            }
            _ => {
                word.push(c);
                in_word = true;
            }
        }
        i += 1;
    }
    end_word(&mut word, &mut in_word, &mut tokens);
    Ok(tokens)
}

/// The alternatives of a case pattern: split at `|` outside quotes.
fn split_patterns(pattern: &str) -> Vec<String> {
    let mut parts = alloc::vec![String::new()];
    let mut quote = None;
    let mut escaped = false;
    for c in pattern.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '\'' | '"' if quote.is_none() => quote = Some(c),
            q if quote == Some(q) => quote = None,
            '|' if quote.is_none() => {
                parts.push(String::new());
                continue;
            }
            _ => {}
        }
        parts.last_mut().unwrap().push(c);
    }
    parts
}

/// The index of the `)` that closes a `$(` whose text starts at `start`;
/// quotes, backslashes and nested parentheses are skipped. `None`: not closed.
pub fn subst_end(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            q @ ('\'' | '"') => {
                i += 1;
                while *chars.get(i)? != q {
                    if q == '"' && chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '(' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

pub type List = Vec<AndOr>;

/// Pipelines joined by `&&` / `||`, possibly run in the background.
#[derive(Debug, Clone)]
pub struct AndOr {
    pub first: Pipeline,
    /// (true = `&&`, false = `||`, pipeline)
    pub rest: Vec<(bool, Pipeline)>,
    pub background: bool,
    /// The source text (for `jobs` and for running it in a background shell).
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    /// `!`: the status is inverted.
    pub negate: bool,
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone)]
pub enum Command {
    /// Raw words, including redirection operators.
    Simple(Vec<String>),
    /// A compound command and the raw words of its redirections.
    Compound(Compound, Vec<String>),
    Function(String, Rc<List>),
}

#[derive(Debug, Clone)]
pub enum Compound {
    If {
        /// (condition, body) for `if` and every `elif`
        branches: Vec<(List, List)>,
        otherwise: Option<List>,
    },
    Loop {
        /// `until`: runs while the condition fails
        until: bool,
        condition: List,
        body: List,
    },
    For {
        var: String,
        /// `None`: the positional arguments
        items: Option<Vec<String>>,
        body: List,
    },
    Group(List),
    Case {
        /// The raw word that is matched.
        word: String,
        /// (raw patterns, body) in order; the first branch with a matching pattern runs.
        branches: Vec<(Vec<String>, List)>,
    },
}

const TERMINATORS: &[&str] = &["then", "elif", "else", "fi", "do", "done", "}", "esac"];

pub fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parses a whole script.
pub fn parse(text: &str) -> Result<List, SyntaxError> {
    let mut p = Parser {
        tokens: tokenize(text)?,
        pos: 0,
    };
    let list = p.list(&[])?;
    match p.peek() {
        None => Ok(list),
        Some(t) => Err(SyntaxError::Unexpected(describe(t))),
    }
}

fn describe(t: &Token) -> String {
    match t {
        Token::Word(w) => w.clone(),
        Token::Semi => String::from(";"),
        Token::DoubleSemi => String::from(";;"),
        Token::Newline => String::from("newline"),
        Token::Amp => String::from("&"),
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn peek_word(&self) -> Option<&str> {
        match self.peek() {
            Some(Token::Word(w)) => Some(w),
            _ => None,
        }
    }

    fn skip_newlines(&mut self) {
        while self.peek() == Some(&Token::Newline) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, word: &str) -> Result<(), SyntaxError> {
        match self.peek() {
            None => Err(SyntaxError::Incomplete),
            Some(Token::Word(w)) if w == word => {
                self.pos += 1;
                Ok(())
            }
            Some(t) => Err(SyntaxError::Unexpected(describe(t))),
        }
    }

    /// And-or chains until the end or one of `terms` at the start of a command.
    fn list(&mut self, terms: &[&str]) -> Result<List, SyntaxError> {
        let mut list = Vec::new();
        loop {
            while matches!(self.peek(), Some(Token::Semi | Token::Newline)) {
                self.pos += 1;
            }
            match self.peek() {
                None if terms.is_empty() => return Ok(list),
                None => return Err(SyntaxError::Incomplete),
                Some(Token::Amp) => return Err(SyntaxError::Unexpected(String::from("&"))),
                Some(Token::DoubleSemi) if terms.contains(&";;") => return Ok(list),
                Some(Token::Word(w)) if TERMINATORS.contains(&w.as_str()) => {
                    return if terms.contains(&w.as_str()) {
                        Ok(list)
                    } else {
                        Err(SyntaxError::Unexpected(w.clone()))
                    };
                }
                _ => {}
            }
            let mut and_or = self.and_or()?;
            match self.peek() {
                Some(Token::Amp) => {
                    and_or.background = true;
                    self.pos += 1;
                }
                Some(Token::Semi | Token::Newline) => self.pos += 1,
                Some(Token::DoubleSemi) if terms.contains(&";;") => {} // the next round ends the list
                None => {}
                Some(t) => return Err(SyntaxError::Unexpected(describe(t))),
            }
            list.push(and_or);
        }
    }

    fn and_or(&mut self) -> Result<AndOr, SyntaxError> {
        let start = self.pos;
        let first = self.pipeline()?;
        let mut rest = Vec::new();
        while let Some(op @ ("&&" | "||")) = self.peek_word() {
            let and = op == "&&";
            self.pos += 1;
            self.skip_newlines();
            rest.push((and, self.pipeline()?));
        }
        let text = self.tokens[start..self.pos]
            .iter()
            .map(|t| match t {
                Token::Word(w) => w.clone(),
                Token::Newline => String::from("\n"),
                other => describe(other),
            })
            .collect::<Vec<_>>()
            .join(" ");
        Ok(AndOr {
            first,
            rest,
            background: false,
            text,
        })
    }

    fn pipeline(&mut self) -> Result<Pipeline, SyntaxError> {
        let negate = self.peek_word() == Some("!");
        if negate {
            self.pos += 1;
        }
        let mut commands = alloc::vec![self.command()?];
        while self.peek_word() == Some("|") {
            self.pos += 1;
            self.skip_newlines();
            commands.push(self.command()?);
        }
        Ok(Pipeline { negate, commands })
    }

    fn command(&mut self) -> Result<Command, SyntaxError> {
        let compound = match self.peek_word() {
            None => {
                return match self.peek() {
                    None => Err(SyntaxError::Incomplete),
                    Some(t) => Err(SyntaxError::Unexpected(describe(t))),
                };
            }
            Some("if") => self.if_command()?,
            Some(w @ ("while" | "until")) => {
                let until = w == "until";
                self.pos += 1;
                let condition = self.list(&["do"])?;
                self.expect("do")?;
                let body = self.list(&["done"])?;
                self.expect("done")?;
                Compound::Loop { until, condition, body }
            }
            Some("for") => self.for_command()?,
            Some("case") => self.case_command()?,
            Some("{") => {
                self.pos += 1;
                let body = self.list(&["}"])?;
                self.expect("}")?;
                Compound::Group(body)
            }
            Some(w) if w.ends_with("()") && is_name(&w[..w.len() - 2]) => {
                let name = String::from(&w[..w.len() - 2]);
                self.pos += 1;
                self.skip_newlines();
                self.expect("{")?;
                let body = self.list(&["}"])?;
                self.expect("}")?;
                return Ok(Command::Function(name, Rc::new(body)));
            }
            Some(w) if TERMINATORS.contains(&w) => return Err(SyntaxError::Unexpected(String::from(w))),
            Some(_) => {
                let words = self.words();
                return Ok(Command::Simple(words));
            }
        };
        Ok(Command::Compound(compound, self.words()))
    }

    /// Words up to the next separator or operator.
    fn words(&mut self) -> Vec<String> {
        let mut words = Vec::new();
        while let Some(w) = self.peek_word() {
            if matches!(w, "|" | "&&" | "||") {
                break;
            }
            words.push(String::from(w));
            self.pos += 1;
        }
        words
    }

    fn if_command(&mut self) -> Result<Compound, SyntaxError> {
        self.pos += 1; // if
        let mut branches = Vec::new();
        let mut otherwise = None;
        loop {
            let condition = self.list(&["then"])?;
            self.expect("then")?;
            let body = self.list(&["elif", "else", "fi"])?;
            branches.push((condition, body));
            match self.peek_word() {
                Some("elif") => self.pos += 1,
                Some("else") => {
                    self.pos += 1;
                    otherwise = Some(self.list(&["fi"])?);
                    self.expect("fi")?;
                    break;
                }
                _ => {
                    self.expect("fi")?;
                    break;
                }
            }
        }
        Ok(Compound::If { branches, otherwise })
    }

    fn case_command(&mut self) -> Result<Compound, SyntaxError> {
        self.pos += 1; // case
        let word = match self.peek() {
            Some(Token::Word(w)) => w.clone(),
            None => return Err(SyntaxError::Incomplete),
            Some(t) => return Err(SyntaxError::Unexpected(describe(t))),
        };
        self.pos += 1;
        self.skip_newlines();
        self.expect("in")?;
        let mut branches = Vec::new();
        loop {
            while matches!(self.peek(), Some(Token::Semi | Token::Newline)) {
                self.pos += 1;
            }
            let mut pattern = match self.peek() {
                None => return Err(SyntaxError::Incomplete),
                Some(Token::Word(w)) if w == "esac" => {
                    self.pos += 1;
                    break;
                }
                Some(Token::Word(w)) => w.clone(),
                Some(t) => return Err(SyntaxError::Unexpected(describe(t))),
            };
            self.pos += 1;
            // `a|b)` is one word; `a | b )` several
            while !pattern.ends_with(')') {
                match self.peek_word() {
                    Some(w) => {
                        pattern.push_str(w);
                        self.pos += 1;
                    }
                    None if self.peek().is_none() => return Err(SyntaxError::Incomplete),
                    None => return Err(SyntaxError::Message("a case pattern ends with ')'")),
                }
            }
            pattern.pop();
            let pattern = pattern.strip_prefix('(').unwrap_or(&pattern);
            let patterns = split_patterns(pattern);
            let body = self.list(&[";;", "esac"])?;
            if self.peek() == Some(&Token::DoubleSemi) {
                self.pos += 1;
            }
            branches.push((patterns, body));
        }
        Ok(Compound::Case { word, branches })
    }

    fn for_command(&mut self) -> Result<Compound, SyntaxError> {
        self.pos += 1; // for
        let var = match self.peek_word() {
            Some(w) if is_name(w) => String::from(w),
            Some(w) => return Err(SyntaxError::Unexpected(String::from(w))),
            None if self.peek().is_none() => return Err(SyntaxError::Incomplete),
            None => return Err(SyntaxError::Message("for needs a variable name")),
        };
        self.pos += 1;
        self.skip_newlines();
        let mut items = None;
        if self.peek_word() == Some("in") {
            self.pos += 1;
            let mut list = Vec::new();
            while let Some(w) = self.peek_word() {
                list.push(String::from(w));
                self.pos += 1;
            }
            items = Some(list);
        }
        while matches!(self.peek(), Some(Token::Semi | Token::Newline)) {
            self.pos += 1;
        }
        self.expect("do")?;
        let body = self.list(&["done"])?;
        self.expect("done")?;
        Ok(Compound::For { var, items, body })
    }
}
