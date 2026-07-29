//! Lexer for the indicator grammar.

use crate::error::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub pos: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    LBracket,
    RBracket,
    LParen,
    RParen,
    LBrace,
    RBrace,
    At,
    Semi,
    Colon,
    Comma,
    Plus,
    Minus,
    Star,
    Slash,
    Dollar,
    Ident(String),
    Int(i64),
    Float(f64),
    Eof,
}

pub fn tokenize(input: &str) -> Result<Vec<Token>, Error> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();

    while i < bytes.len() {
        let pos = i;
        let c = bytes[i] as char;

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        let kind = match c {
            '[' => {
                i += 1;
                TokenKind::LBracket
            }
            ']' => {
                i += 1;
                TokenKind::RBracket
            }
            '(' => {
                i += 1;
                TokenKind::LParen
            }
            ')' => {
                i += 1;
                TokenKind::RParen
            }
            '{' => {
                i += 1;
                TokenKind::LBrace
            }
            '}' => {
                i += 1;
                TokenKind::RBrace
            }
            '@' => {
                i += 1;
                TokenKind::At
            }
            ';' => {
                i += 1;
                TokenKind::Semi
            }
            ':' => {
                i += 1;
                TokenKind::Colon
            }
            ',' => {
                i += 1;
                TokenKind::Comma
            }
            '+' => {
                i += 1;
                TokenKind::Plus
            }
            '-' => {
                i += 1;
                TokenKind::Minus
            }
            '*' => {
                i += 1;
                TokenKind::Star
            }
            '/' => {
                i += 1;
                TokenKind::Slash
            }
            '$' => {
                i += 1;
                TokenKind::Dollar
            }
            '0'..='9' => {
                let (tok, next) = lex_number(input, i)?;
                i = next;
                tok
            }
            'A'..='Z' | 'a'..='z' | '_' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    let ch = bytes[i] as char;
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                TokenKind::Ident(input[start..i].to_string())
            }
            _ => {
                return Err(Error::parse(
                    format!("unexpected character `{c}`"),
                    input,
                    Some(pos),
                ));
            }
        };

        out.push(Token { kind, pos });
    }

    out.push(Token {
        kind: TokenKind::Eof,
        pos: input.len(),
    });
    Ok(out)
}

fn lex_number(input: &str, start: usize) -> Result<(TokenKind, usize), Error> {
    let bytes = input.as_bytes();
    let mut i = start;
    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
        i += 1;
    }
    let mut is_float = false;
    if i < bytes.len() && bytes[i] == b'.' {
        is_float = true;
        i += 1;
        if i >= bytes.len() || !(bytes[i] as char).is_ascii_digit() {
            return Err(Error::parse(
                "expected digit after decimal point",
                input,
                Some(i),
            ));
        }
        while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
            i += 1;
        }
    }
    let slice = &input[start..i];
    if is_float {
        let v: f64 = slice.parse().map_err(|_| {
            Error::parse(format!("invalid float `{slice}`"), input, Some(start))
        })?;
        Ok((TokenKind::Float(v), i))
    } else {
        let v: i64 = slice
            .parse()
            .map_err(|_| Error::parse(format!("invalid integer `{slice}`"), input, Some(start)))?;
        Ok((TokenKind::Int(v), i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_avg_call() {
        let toks = tokenize("AVG([close; $from:$to], $period)").unwrap();
        assert!(toks.iter().any(|t| matches!(&t.kind, TokenKind::Ident(s) if s == "AVG")));
        assert!(toks.iter().any(|t| matches!(&t.kind, TokenKind::Dollar)));
    }
}
