//! `$(( expression ))`: integer arithmetic with C precedence.
//!
//! Operators: `|| && == != < <= > >= + - * / %`, unary `- + !`, parentheses.
//! Names are variables (empty or not a number = 0). Results are 64-bit,
//! overflow wraps; division by zero is an error.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Name(String),
    Op(&'static str),
}

const OPS: &[&str] = &[
    "||", "&&", "==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%", "!", "(", ")",
];

fn tokenize(text: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        let c = rest.chars().next().unwrap();
        if c.is_ascii_digit() {
            let len = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
            let n = rest[..len]
                .parse::<i64>()
                .map_err(|_| alloc::format!("bad number '{}'", &rest[..len]))?;
            toks.push(Tok::Num(n));
            rest = &rest[len..];
        } else if c.is_ascii_alphabetic() || c == '_' {
            let len = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            toks.push(Tok::Name(String::from(&rest[..len])));
            rest = &rest[len..];
        } else if let Some(op) = OPS.iter().find(|op| rest.starts_with(**op)) {
            toks.push(Tok::Op(op));
            rest = &rest[op.len()..];
        } else {
            return Err(alloc::format!("unexpected '{}'", c));
        }
        rest = rest.trim_start();
    }
    Ok(toks)
}

/// Evaluates `text`; `var` gives the value of a name.
pub fn eval(text: &str, var: &dyn Fn(&str) -> String) -> Result<i64, String> {
    let mut p = Parser {
        toks: tokenize(text)?,
        pos: 0,
        var,
    };
    if p.toks.is_empty() {
        return Ok(0);
    }
    let v = p.binary(0)?;
    match p.toks.get(p.pos) {
        None => Ok(v),
        Some(t) => Err(alloc::format!("unexpected {:?}", t)),
    }
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    var: &'a dyn Fn(&str) -> String,
}

/// Binary operators by precedence level (lowest first).
const LEVELS: &[&[&str]] = &[
    &["||"],
    &["&&"],
    &["==", "!="],
    &["<", "<=", ">", ">="],
    &["+", "-"],
    &["*", "/", "%"],
];

impl Parser<'_> {
    fn binary(&mut self, level: usize) -> Result<i64, String> {
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        while let Some(Tok::Op(op)) = self.toks.get(self.pos) {
            let op = *op;
            if !LEVELS[level].contains(&op) {
                break;
            }
            self.pos += 1;
            let right = self.binary(level + 1)?;
            left = match op {
                "||" => (left != 0 || right != 0) as i64,
                "&&" => (left != 0 && right != 0) as i64,
                "==" => (left == right) as i64,
                "!=" => (left != right) as i64,
                "<" => (left < right) as i64,
                "<=" => (left <= right) as i64,
                ">" => (left > right) as i64,
                ">=" => (left >= right) as i64,
                "+" => left.wrapping_add(right),
                "-" => left.wrapping_sub(right),
                "*" => left.wrapping_mul(right),
                _ if right == 0 => return Err(String::from("division by zero")),
                "/" => left.wrapping_div(right),
                _ => left.wrapping_rem(right),
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<i64, String> {
        let tok = self.toks.get(self.pos).cloned();
        self.pos += 1;
        match tok {
            Some(Tok::Num(n)) => Ok(n),
            Some(Tok::Name(name)) => Ok((self.var)(&name).trim().parse().unwrap_or(0)),
            Some(Tok::Op("-")) => Ok(self.unary()?.wrapping_neg()),
            Some(Tok::Op("+")) => self.unary(),
            Some(Tok::Op("!")) => Ok((self.unary()? == 0) as i64),
            Some(Tok::Op("(")) => {
                let v = self.binary(0)?;
                match self.toks.get(self.pos) {
                    Some(Tok::Op(")")) => {
                        self.pos += 1;
                        Ok(v)
                    }
                    _ => Err(String::from("missing ')'")),
                }
            }
            Some(t) => Err(alloc::format!("unexpected {:?}", t)),
            None => Err(String::from("expression ends too early")),
        }
    }
}
