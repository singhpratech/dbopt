use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokKind {
    Word,
    Number,
    String,
    Punct,
    Comment,
    Whitespace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token<'a> {
    pub kind: TokKind,
    pub text: &'a str,
    pub start: u32,
    pub end: u32,
    pub line: u32,
    pub col: u32,
}

pub fn tokenize(src: &str) -> Vec<Token<'_>> {
    let mut out = Vec::with_capacity(src.len() / 6);
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut col = 1u32;

    let pos = |i: usize| i as u32;

    while i < bytes.len() {
        let start = i;
        let start_line = line;
        let start_col = col;
        let b = bytes[i];

        let (kind, advance, newlines, last_line_chars) = match b {
            b' ' | b'\t' | b'\r' | b'\n' => {
                let mut j = i;
                let mut nl = 0u32;
                let mut last = 0u32;
                while j < bytes.len() {
                    match bytes[j] {
                        b'\n' => { nl += 1; last = 0; j += 1; }
                        b' ' | b'\t' | b'\r' => { last += 1; j += 1; }
                        _ => break,
                    }
                }
                (TokKind::Whitespace, j - i, nl, last)
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] != b'\n' { j += 1; }
                (TokKind::Comment, j - i, 0, (j - i) as u32)
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                let mut j = i + 2;
                let mut nl = 0u32;
                let mut last = 2u32;
                while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                    if bytes[j] == b'\n' { nl += 1; last = 0; } else { last += 1; }
                    j += 1;
                }
                if j + 1 < bytes.len() { j += 2; last += 2; }
                (TokKind::Comment, j - i, nl, last)
            }
            b'\'' => {
                let mut j = i + 1;
                while j < bytes.len() {
                    if bytes[j] == b'\'' {
                        if j + 1 < bytes.len() && bytes[j + 1] == b'\'' { j += 2; continue; }
                        j += 1; break;
                    }
                    j += 1;
                }
                (TokKind::String, j - i, 0, (j - i) as u32)
            }
            b'[' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b']' { j += 1; }
                if j < bytes.len() { j += 1; }
                (TokKind::Word, j - i, 0, (j - i) as u32)
            }
            b if b.is_ascii_alphabetic() || b == b'_' || b == b'@' || b == b'#' => {
                let mut j = i + 1;
                while j < bytes.len() {
                    let c = bytes[j];
                    if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c == b'#' { j += 1; } else { break; }
                }
                (TokKind::Word, j - i, 0, (j - i) as u32)
            }
            b if b.is_ascii_digit() => {
                let mut j = i + 1;
                while j < bytes.len() {
                    let c = bytes[j];
                    if c.is_ascii_digit() || c == b'.' { j += 1; } else { break; }
                }
                (TokKind::Number, j - i, 0, (j - i) as u32)
            }
            _ => (TokKind::Punct, 1, 0, 1),
        };

        let text = std::str::from_utf8(&bytes[start..start + advance]).unwrap_or("");
        if kind != TokKind::Whitespace {
            out.push(Token {
                kind,
                text,
                start: pos(start),
                end: pos(start + advance),
                line: start_line,
                col: start_col,
            });
        }
        i += advance;
        if newlines > 0 {
            line += newlines;
            col = last_line_chars + 1;
        } else {
            col += last_line_chars;
        }
    }
    out
}

pub fn word_eq_ci(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| x.eq_ignore_ascii_case(&y))
}

pub fn is_keyword(text: &str, kw: &str) -> bool {
    word_eq_ci(text.trim_matches(|c| c == '[' || c == ']'), kw)
}
