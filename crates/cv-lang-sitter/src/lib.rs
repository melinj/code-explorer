//! Shared tree-sitter plumbing for language packs: UTF-16 columns, per-line token emission and the
//! `.scm` query runner ([`query`]). Depends on tree-sitter itself but on no grammar; only
//! `cv-lang-*` packs use it.

pub mod query;
pub mod sites;

use cv_lang::{LineTokens, token};
use tree_sitter::{Node, Point};

/// Byte offsets of line starts, for byte → UTF-16 column conversion.
pub struct Text<'a> {
    pub src: &'a str,
    starts: Vec<usize>,
    /// Per line: only ASCII bytes, so UTF-16 columns equal byte columns (the common case).
    ascii: Vec<bool>,
}

impl<'a> Text<'a> {
    pub fn new(src: &'a str) -> Self {
        let mut starts = vec![0];
        let mut ascii = Vec::new();
        let mut line_ascii = true;
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
                ascii.push(line_ascii);
                line_ascii = true;
            } else if b >= 0x80 {
                line_ascii = false;
            }
        }
        ascii.push(line_ascii);
        Text { src, starts, ascii }
    }

    /// Number of lines as editors count them (a trailing newline does not open a new line).
    pub fn line_count(&self) -> u32 {
        let n = self.starts.len();
        if n > 1 && self.starts[n - 1] == self.src.len() {
            (n - 1) as u32
        } else {
            n as u32
        }
    }

    fn line_end(&self, row: usize) -> usize {
        let end = self
            .starts
            .get(row + 1)
            .map(|s| s - 1)
            .unwrap_or(self.src.len());
        if end > self.starts[row] && self.src.as_bytes()[end - 1] == b'\r' {
            end - 1
        } else {
            end
        }
    }

    /// UTF-16 column of a byte column on a row (clamped to the line).
    pub fn col16(&self, row: usize, byte_col: usize) -> u32 {
        let Some(&s) = self.starts.get(row) else {
            return 0;
        };
        let end = (s + byte_col).min(self.line_end(row));
        if self.ascii[row] {
            return (end - s) as u32;
        }
        let mut e = end;
        while e > s && !self.src.is_char_boundary(e) {
            e -= 1;
        }
        self.src[s..e].encode_utf16().count() as u32
    }

    pub fn row_len16(&self, row: usize) -> u32 {
        let Some(&s) = self.starts.get(row) else {
            return 0;
        };
        if self.ascii[row] {
            return (self.line_end(row) - s) as u32;
        }
        self.src[s..self.line_end(row)].encode_utf16().count() as u32
    }

    pub fn node_text(&self, n: Node) -> &'a str {
        &self.src[n.byte_range()]
    }
}

/// Collects tokens per line. Spans must arrive in document order; overlaps are clipped.
pub struct Tokens<'t, 'a> {
    text: &'t Text<'a>,
    pub lines: Vec<LineTokens>,
    last_end: Vec<u32>,
}

impl<'t, 'a> Tokens<'t, 'a> {
    pub fn new(text: &'t Text<'a>) -> Self {
        let n = text.line_count() as usize;
        Tokens {
            text,
            lines: vec![Vec::new(); n],
            last_end: vec![0; n],
        }
    }

    pub fn span(&mut self, start: Point, end: Point, cls: u32) {
        if cls == token::PLAIN {
            return;
        }
        for row in start.row..=end.row.min(self.lines.len().saturating_sub(1)) {
            let a = if row == start.row {
                self.text.col16(row, start.column)
            } else {
                0
            };
            let b = if row == end.row {
                self.text.col16(row, end.column)
            } else {
                self.text.row_len16(row)
            };
            let a = a.max(self.last_end[row]);
            if b > a {
                self.lines[row].extend([a, b - a, cls]);
                self.last_end[row] = b;
            }
        }
    }

    pub fn node(&mut self, n: Node, cls: u32) {
        self.span(n.start_position(), n.end_position(), cls);
    }
}

/// Class for anonymous (keyword/punctuation/operator) leaves, shared by C-like and Python grammars
/// (the generated anonymous-token blocks of their `highlights.scm`, see [`query::anonymous_block`]).
pub fn anonymous_class(kind: &str) -> u32 {
    if kind.chars().all(|c| c.is_ascii_alphabetic() || c == '_') && !kind.is_empty() {
        token::KEYWORD
    } else if matches!(
        kind,
        "(" | ")" | "[" | "]" | "{" | "}" | "," | "." | ":" | ";" | "${" | "`" | "\"" | "'"
    ) {
        token::PUNCTUATION
    } else {
        token::OPERATOR
    }
}

/// 1-based line of a node's first row.
pub fn line(n: Node) -> u32 {
    n.start_position().row as u32 + 1
}

/// 1-based inclusive last line; a node ending at column 0 ends on the previous line.
pub fn end_line(n: Node) -> u32 {
    let e = n.end_position();
    if e.column == 0 && e.row > n.start_position().row {
        e.row as u32
    } else {
        e.row as u32 + 1
    }
}

/// Number of ERROR / MISSING nodes.
pub fn count_errors(root: Node) -> u32 {
    if !root.has_error() {
        return 0;
    }
    let mut n = 0;
    let mut stack = vec![root];
    while let Some(x) = stack.pop() {
        if x.is_error() || x.is_missing() {
            n += 1;
            continue;
        }
        if x.has_error() {
            let mut c = x.walk();
            stack.extend(x.children(&mut c));
        }
    }
    n
}
