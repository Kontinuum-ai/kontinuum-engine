//! Hand-written lexer for the LiveCode DSL v0 (no parser dependencies).
//! Produces line-tagged tokens; lexical errors are fatal.

use super::error::{dsl_err, DslCode, DslError};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f32),
    /// Binary mask literal: slots from the most significant digit.
    Bin(Vec<bool>),
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Eq,
    Dot,
    Colon,
    Comma,
    Semi,
    At,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
}

/// Tokenizes `src`. Comments (`#`, `//` to end of line) are skipped. All
/// errors are fatal (the stream cannot be trusted afterwards).
pub fn lex(src: &str) -> Result<Vec<Token>, Vec<DslError>> {
    let mut cur = Cursor { chars: src.chars().collect(), pos: 0, line: 1 };
    let mut tokens = Vec::new();
    while let Some(c) = cur.peek() {
        match c {
            '\n' => {
                cur.line += 1;
                cur.bump();
            }
            ' ' | '\t' | '\r' => cur.bump(),
            '#' => cur.skip_line(),
            '/' => {
                cur.bump();
                if cur.peek() == Some('/') {
                    cur.skip_line();
                } else {
                    return Err(vec![dsl_err(
                        DslCode::E_DSL_BAD_CHAR,
                        cur.line,
                        "/source",
                        "stray `/` (only `//` comments exist in v0)",
                        "use `//` or `#` for comments",
                    )]);
                }
            }
            '{' => cur.punct(Tok::LBrace, &mut tokens),
            '}' => cur.punct(Tok::RBrace, &mut tokens),
            '[' => cur.punct(Tok::LBracket, &mut tokens),
            ']' => cur.punct(Tok::RBracket, &mut tokens),
            '(' => cur.punct(Tok::LParen, &mut tokens),
            ')' => cur.punct(Tok::RParen, &mut tokens),
            '=' => cur.punct(Tok::Eq, &mut tokens),
            '.' => cur.punct(Tok::Dot, &mut tokens),
            ':' => cur.punct(Tok::Colon, &mut tokens),
            ',' => cur.punct(Tok::Comma, &mut tokens),
            ';' => cur.punct(Tok::Semi, &mut tokens),
            '@' => cur.punct(Tok::At, &mut tokens),
            c if c.is_ascii_digit() || c == '-' => {
                let tok = lex_number(&mut cur)?;
                tokens.push(Token { tok, line: cur.line });
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let name = cur.ident();
                tokens.push(Token { tok: Tok::Ident(name), line: cur.line });
            }
            other => {
                return Err(vec![dsl_err(
                    DslCode::E_DSL_BAD_CHAR,
                    cur.line,
                    "/source",
                    format!("unexpected character `{other}`"),
                    "remove the character; see the v0 grammar in kontinuum_ir::dsl",
                )]);
            }
        }
    }
    tokens.push(Token { tok: Tok::Eof, line: cur.line });
    Ok(tokens)
}

struct Cursor {
    chars: Vec<char>,
    pos: usize,
    line: usize,
}

impl Cursor {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn bump(&mut self) {
        self.pos += 1;
    }

    fn skip_line(&mut self) {
        while let Some(c) = self.peek() {
            self.bump();
            if c == '\n' {
                break;
            }
        }
        self.line = 1 + self.chars[..self.pos].iter().filter(|&&c| c == '\n').count();
    }

    fn punct(&mut self, tok: Tok, tokens: &mut Vec<Token>) {
        self.bump();
        tokens.push(Token { tok, line: self.line });
    }

    fn ident(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            self.bump();
        }
        self.chars[start..self.pos].iter().collect()
    }
}

/// Numbers: `-?digits[.digits][e[+-]digits]` or `0b[01_]+`. Underscores are
/// digit separators in both forms.
fn lex_number(cur: &mut Cursor) -> Result<Tok, Vec<DslError>> {
    let line = cur.line;
    let start = cur.pos;
    if cur.peek() == Some('-') {
        cur.bump();
    }
    if cur.peek() == Some('0') && cur.at(1) == Some('b') {
        cur.bump();
        cur.bump();
        let mut bits = Vec::new();
        while let Some(c) = cur.peek() {
            match c {
                '0' => bits.push(false),
                '1' => bits.push(true),
                '_' => {}
                _ => break,
            }
            cur.bump();
        }
        if bits.is_empty() {
            return Err(vec![dsl_err(
                DslCode::E_DSL_MASK_EMPTY,
                line,
                "/source",
                "binary mask has no digits",
                "write at least one 0/1 digit, e.g. `0b1000`",
            )]);
        }
        return Ok(Tok::Bin(bits));
    }
    let mut is_float = false;
    while let Some(c) = cur.peek() {
        if c.is_ascii_digit() {
            cur.bump();
        } else if c == '.' && !is_float && cur.at(1).is_some_and(|n| n.is_ascii_digit()) {
            is_float = true;
            cur.bump();
        } else if (c == 'e' || c == 'E') && exponent_ahead(cur) {
            is_float = true;
            cur.bump();
            if matches!(cur.peek(), Some('+') | Some('-')) {
                cur.bump();
            }
        } else {
            break;
        }
    }
    let text: String = cur.chars[start..cur.pos].iter().filter(|&&c| c != '_').collect();
    if is_float {
        match text.parse::<f32>() {
            Ok(v) if v.is_finite() => Ok(Tok::Float(v)),
            _ => Err(vec![dsl_err(
                DslCode::E_DSL_BAD_NUMBER,
                line,
                "/source",
                format!("`{text}` is not a valid float"),
                "write a plain decimal number in f32 range",
            )]),
        }
    } else {
        text.parse::<i64>().map(Tok::Int).map_err(|_| {
            vec![dsl_err(
                DslCode::E_DSL_BAD_NUMBER,
                line,
                "/source",
                format!("`{text}` is out of integer range"),
                "keep integers within i64",
            )]
        })
    }
}

fn exponent_ahead(cur: &Cursor) -> bool {
    let mut i = cur.pos + 1;
    if matches!(cur.chars.get(i), Some('+') | Some('-')) {
        i += 1;
    }
    cur.chars.get(i).is_some_and(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        lex(src).expect("lex").into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn punctuation_idents_and_comments() {
        assert_eq!(
            toks("kick.mask = 0b1010; # trailing\n// whole line\nhat: E(1, 16, 2);"),
            vec![
                Tok::Ident("kick".into()),
                Tok::Dot,
                Tok::Ident("mask".into()),
                Tok::Eq,
                Tok::Bin(vec![true, false, true, false]),
                Tok::Semi,
                Tok::Ident("hat".into()),
                Tok::Colon,
                Tok::Ident("E".into()),
                Tok::LParen,
                Tok::Int(1),
                Tok::Comma,
                Tok::Int(16),
                Tok::Comma,
                Tok::Int(2),
                Tok::RParen,
                Tok::Semi,
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn floats_ints_and_underscores() {
        let mask16 = Tok::Bin((0..16).map(|i| i == 0).collect());
        assert_eq!(
            toks("1.0 -2 1e3 0b1000_0000_0000_0000 120.5"),
            vec![Tok::Float(1.0), Tok::Int(-2), Tok::Float(1000.0), mask16, Tok::Float(120.5), Tok::Eof]
        );
    }

    #[test]
    fn lexical_errors_are_fatal() {
        assert_eq!(lex("$").expect_err("bad char")[0].code, DslCode::E_DSL_BAD_CHAR);
        assert_eq!(lex("0b").expect_err("empty mask")[0].code, DslCode::E_DSL_MASK_EMPTY);
        assert!(lex("/x").is_err());
        assert!(lex("1e99999").is_err(), "float overflow rejected");
    }
}
