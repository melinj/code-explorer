//! OutlineEncoding (wire.ts): 3 bytes per line — indent, visible length, flags.

use crate::{LineTokens, token};

/// Base records for every line of a file. Port and thread bits are OR-ed in per window.
pub fn records(lines: &[&str], tokens: &[LineTokens]) -> Vec<u8> {
    let mut out = Vec::with_capacity(lines.len() * 3);
    for (i, line) in lines.iter().enumerate() {
        let (indent, len) = columns(line);
        let mut weight = [0u32; 16];
        if let Some(t) = tokens.get(i) {
            for t in t.as_chunks::<3>().0 {
                weight[(t[2] & 15) as usize] += t[1];
            }
        }
        let dominant = if len == 0 {
            token::BLANK
        } else {
            (1..15u32)
                .max_by_key(|&c| (weight[c as usize], std::cmp::Reverse(c)))
                .filter(|&c| weight[c as usize] > 0)
                .unwrap_or(token::PLAIN)
        };
        out.extend([indent.min(255) as u8, len.min(255) as u8, dominant as u8]);
    }
    out
}

/// Indent and visible length in columns (tabs count as 4).
fn columns(line: &str) -> (usize, usize) {
    let mut indent = 0;
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' => indent += 1,
            '\t' => indent += 4,
            _ => break,
        }
        chars.next();
    }
    let rest: String = chars.collect();
    (indent, rest.trim_end().chars().count())
}

pub const PORT_BIT: u8 = 0x40;
pub const THREAD_BIT: u8 = 0x80;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_have_indent_length_and_class() {
        let r = records(
            &["    return x", "", "\tfoo"],
            &[vec![4, 6, token::KEYWORD], vec![], vec![]],
        );
        assert_eq!(
            r,
            vec![
                4,
                8,
                token::KEYWORD as u8,
                0,
                0,
                token::BLANK as u8,
                4,
                3,
                0
            ]
        );
    }
}
