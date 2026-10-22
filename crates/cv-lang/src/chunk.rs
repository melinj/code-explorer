//! Snapshot chunking (DESIGN §4.4):
//! - a file of ≤ 120 lines is one window;
//! - a declaration of ≥ 12 lines gets its own window;
//! - small declarations and the lines between them merge into chunks of ≤ 500 lines;
//! - a declaration over 500 lines splits into a header window (`<qualname>:head`: its lines before
//!   the first member) plus member windows (containers), or into parts.

use crate::{FILE_CHIP_SLOTS, SymbolFact, hash};

pub const SMALL_FILE_LINES: u32 = 120;
pub const OWN_WINDOW_LINES: u32 = 12;
pub const MAX_WINDOW_LINES: u32 = 500;
/// Key suffix of the header window of a declaration over 500 lines (`K:head`).
pub const HEAD_SUFFIX: &str = ":head";

fn is_head(w: &WindowSpec) -> bool {
    w.kind == "chunk" && w.key.contains(HEAD_SUFFIX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSpec {
    /// model.ts WindowKind: "symbol" or "chunk".
    pub kind: &'static str,
    /// 1-based inclusive.
    pub start: u32,
    pub end: u32,
    /// Index into the file's symbols: the declaration, or the first one inside a chunk.
    pub symbol: Option<usize>,
    /// Revision-independent key suffix (after `<path>#`).
    pub key: String,
}

enum Seg {
    Big(usize),
    Small {
        start: u32,
        end: u32,
        sym: Option<usize>,
    },
}

/// Chip slot of a split file: stable per path (wire.ts FILE_CHIP_SLOTS).
pub fn chip_slot(path: &str) -> u32 {
    (hash(path) % FILE_CHIP_SLOTS as u64) as u32
}

pub fn chunk(lines: u32, symbols: &[SymbolFact]) -> Vec<WindowSpec> {
    let lines = lines.max(1);
    if lines <= SMALL_FILE_LINES {
        return vec![WindowSpec {
            kind: "chunk",
            start: 1,
            end: lines,
            symbol: None,
            key: "file".into(),
        }];
    }
    let top: Vec<usize> = (0..symbols.len())
        .filter(|&i| symbols[i].parent.is_none())
        .collect();
    let mut out = Vec::new();
    chunk_range(1, lines, &top, symbols, &mut out);
    merge_tiny(&mut out);
    out
}

fn chunk_range(
    lo: u32,
    hi: u32,
    level: &[usize],
    symbols: &[SymbolFact],
    out: &mut Vec<WindowSpec>,
) {
    let mut order: Vec<usize> = level.to_vec();
    order.sort_by_key(|&i| (symbols[i].start, i));
    let mut segs = Vec::new();
    let mut cursor = lo;
    for i in order {
        let s = &symbols[i];
        let (start, end) = (s.start.max(lo), s.end.min(hi));
        if start < cursor || end < start {
            continue;
        }
        if start > cursor {
            segs.push(Seg::Small {
                start: cursor,
                end: start - 1,
                sym: None,
            });
        }
        if end - start + 1 >= OWN_WINDOW_LINES {
            segs.push(Seg::Big(i));
        } else {
            segs.push(Seg::Small {
                start,
                end,
                sym: Some(i),
            });
        }
        cursor = end + 1;
    }
    if cursor <= hi {
        segs.push(Seg::Small {
            start: cursor,
            end: hi,
            sym: None,
        });
    }

    // Emit, merging consecutive small segments into chunks of ≤ 500 lines.
    let mut pending: Option<WindowSpec> = None;
    let flush = |pending: &mut Option<WindowSpec>, out: &mut Vec<WindowSpec>| {
        if let Some(p) = pending.take() {
            out.push(p);
        }
    };
    for seg in segs {
        match seg {
            Seg::Small { start, end, sym } => {
                if let Some(p) = pending.as_mut() {
                    if end - p.start < MAX_WINDOW_LINES {
                        p.end = end;
                        if p.symbol.is_none() {
                            p.symbol = sym;
                        }
                        continue;
                    }
                    flush(&mut pending, out);
                }
                // A small segment can itself exceed 500 lines (e.g. a long module preamble).
                let mut s = start;
                while s <= end {
                    let e = (s + MAX_WINDOW_LINES - 1).min(end);
                    if e < end {
                        flush(&mut pending, out);
                        out.push(WindowSpec {
                            kind: "chunk",
                            start: s,
                            end: e,
                            symbol: sym,
                            key: String::new(),
                        });
                    } else {
                        pending = Some(WindowSpec {
                            kind: "chunk",
                            start: s,
                            end: e,
                            symbol: sym,
                            key: String::new(),
                        });
                    }
                    s = e + 1;
                }
            }
            Seg::Big(i) => {
                flush(&mut pending, out);
                let s = &symbols[i];
                let (start, end) = (s.start.max(lo), s.end.min(hi));
                let len = end - start + 1;
                if len <= MAX_WINDOW_LINES {
                    out.push(WindowSpec {
                        kind: "symbol",
                        start,
                        end,
                        symbol: Some(i),
                        key: s.qualname.clone(),
                    });
                    continue;
                }
                let children: Vec<usize> = (0..symbols.len())
                    .filter(|&c| symbols[c].parent == Some(i))
                    .collect();
                if s.kind.is_container() && !children.is_empty() {
                    // Header window: the declaration's lines before its first member (signature,
                    // docstring, fields), named after the declaration; never merged away.
                    let first = children
                        .iter()
                        .map(|&c| symbols[c].start)
                        .min()
                        .unwrap_or(start)
                        .clamp(start, end + 1);
                    if first > start {
                        let head = first - start;
                        let parts = head.div_ceil(MAX_WINDOW_LINES);
                        let per = head.div_ceil(parts);
                        for k in 0..parts {
                            let a = start + k * per;
                            let b = (a + per - 1).min(first - 1);
                            out.push(WindowSpec {
                                kind: "chunk",
                                start: a,
                                end: b,
                                symbol: Some(i),
                                key: if k == 0 {
                                    format!("{}{HEAD_SUFFIX}", s.qualname)
                                } else {
                                    format!("{}{HEAD_SUFFIX}{}", s.qualname, k + 1)
                                },
                            });
                        }
                    }
                    if first <= end {
                        chunk_range(first, end, &children, symbols, out);
                    }
                } else {
                    let parts = len.div_ceil(MAX_WINDOW_LINES);
                    let per = len.div_ceil(parts);
                    for k in 0..parts {
                        let a = start + k * per;
                        let b = (a + per - 1).min(end);
                        out.push(WindowSpec {
                            kind: "chunk",
                            start: a,
                            end: b,
                            symbol: Some(i),
                            key: format!("{}:part{}", s.qualname, k + 1),
                        });
                    }
                }
            }
        }
    }
    flush(&mut pending, out);
}

/// Chunks under 12 lines (blank lines, a stray comment, imports) join a neighbour when it stays
/// within 500 lines, so no window is a sliver.
fn merge_tiny(out: &mut Vec<WindowSpec>) {
    let mut i = 0;
    while i < out.len() {
        let w = &out[i];
        let tiny = w.kind == "chunk" && !is_head(w) && w.end - w.start + 1 < OWN_WINDOW_LINES;
        if tiny && out.len() > 1 {
            let (start, end) = (w.start, w.end);
            if i + 1 < out.len()
                && !is_head(&out[i + 1])
                && out[i + 1].end - start < MAX_WINDOW_LINES
            {
                out[i + 1].start = start;
                out.remove(i);
                continue;
            }
            if i > 0 && !is_head(&out[i - 1]) && end - out[i - 1].start < MAX_WINDOW_LINES {
                out[i - 1].end = end;
                out.remove(i);
                continue;
            }
        }
        i += 1;
    }
}

/// Fills in chunk keys: `chunk:<first symbol>` or `chunk:after:<previous window key>`, unique per file.
pub fn assign_keys(specs: &mut [WindowSpec], symbols: &[SymbolFact]) {
    let mut seen = std::collections::HashSet::new();
    let mut prev = String::from("top");
    for w in specs.iter_mut() {
        if w.key.is_empty() {
            w.key = match w.symbol {
                Some(s) => format!("chunk:{}", symbols[s].qualname),
                None => format!("chunk:after:{prev}"),
            };
        }
        let base = w.key.clone();
        let mut n = 2;
        while !seen.insert(w.key.clone()) {
            w.key = format!("{base}~{n}");
            n += 1;
        }
        prev = base;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolKind;

    fn sym(
        name: &str,
        start: u32,
        end: u32,
        parent: Option<usize>,
        kind: SymbolKind,
    ) -> SymbolFact {
        SymbolFact {
            kind,
            name: name.into(),
            qualname: name.into(),
            parent,
            start,
            end,
            name_line: start,
        }
    }

    fn check_cover(ws: &[WindowSpec], lines: u32) {
        let mut next = 1;
        for w in ws {
            assert_eq!(w.start, next, "{ws:?}");
            assert!(
                w.end >= w.start && w.end - w.start < MAX_WINDOW_LINES,
                "{w:?}"
            );
            next = w.end + 1;
        }
        assert_eq!(next, lines + 1);
    }

    #[test]
    fn small_file_is_one_window() {
        let ws = chunk(120, &[sym("f", 1, 50, None, SymbolKind::Function)]);
        assert_eq!(ws.len(), 1);
        assert_eq!(
            (ws[0].start, ws[0].end, ws[0].key.as_str()),
            (1, 120, "file")
        );
    }

    #[test]
    fn big_declarations_get_windows_and_small_ones_merge() {
        let syms = vec![
            sym("a", 5, 10, None, SymbolKind::Function),
            sym("b", 12, 80, None, SymbolKind::Function),
            sym("c", 82, 85, None, SymbolKind::Function),
            sym("d", 87, 96, None, SymbolKind::Function),
            sym("e", 100, 200, None, SymbolKind::Function),
        ];
        let mut ws = chunk(203, &syms);
        assign_keys(&mut ws, &syms);
        check_cover(&ws, 203);
        let keys: Vec<&str> = ws.iter().map(|w| w.key.as_str()).collect();
        // 1–11 (with a) is tiny and joins b; c and d form a chunk; the tail joins e.
        assert_eq!(keys, vec!["b", "chunk:c", "e"]);
    }

    #[test]
    fn oversized_class_splits_into_members_and_long_functions_into_parts() {
        let syms = vec![
            sym("K", 1, 900, None, SymbolKind::Class),
            sym("K.m1", 3, 400, Some(0), SymbolKind::Method),
            sym("K.m2", 401, 900, Some(0), SymbolKind::Method),
            sym("f", 901, 2000, None, SymbolKind::Function),
        ];
        let mut ws = chunk(2000, &syms);
        assign_keys(&mut ws, &syms);
        check_cover(&ws, 2000);
        let keys: Vec<&str> = ws.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["K:head", "K.m1", "K.m2", "f:part1", "f:part2", "f:part3"]
        );
        // The 2-line header stays its own window instead of joining K.m1.
        assert_eq!((ws[0].start, ws[0].end, ws[0].symbol), (1, 2, Some(0)));
    }

    #[test]
    fn oversized_class_gets_header_window_and_merged_small_members() {
        // K: 30 header lines (class line, docstring, fields), then 60 small methods of 10 lines
        // each (merged into chunks of ≤ 500), then one big method.
        let mut syms = vec![sym("K", 1, 750, None, SymbolKind::Class)];
        for k in 0..60u32 {
            let a = 31 + k * 10;
            syms.push(sym(
                &format!("K.s{k}"),
                a,
                a + 9,
                Some(0),
                SymbolKind::Method,
            ));
        }
        syms.push(sym("K.big", 631, 750, Some(0), SymbolKind::Method));
        let mut ws = chunk(750, &syms);
        assign_keys(&mut ws, &syms);
        check_cover(&ws, 750);
        let keys: Vec<&str> = ws.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys, vec!["K:head", "chunk:K.s0", "chunk:K.s50", "K.big"]);
        assert_eq!((ws[0].start, ws[0].end), (1, 30));
        assert_eq!((ws[1].start, ws[1].end), (31, 530));
        assert_eq!((ws[2].start, ws[2].end), (531, 630));
        // Keys are stable when the header grows (every member shifts down).
        let mut syms2 = syms.clone();
        syms2[0].end += 5;
        for m in &mut syms2[1..] {
            m.start += 5;
            m.end += 5;
        }
        let mut ws2 = chunk(755, &syms2);
        assign_keys(&mut ws2, &syms2);
        check_cover(&ws2, 755);
        assert_eq!(
            ws2.iter().map(|w| &w.key).collect::<Vec<_>>(),
            ws.iter().map(|w| &w.key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn oversized_declaration_without_members_splits_into_parts() {
        let syms = vec![sym("K", 1, 1100, None, SymbolKind::Class)];
        let mut ws = chunk(1100, &syms);
        assign_keys(&mut ws, &syms);
        check_cover(&ws, 1100);
        let keys: Vec<&str> = ws.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys, vec!["K:part1", "K:part2", "K:part3"]);
    }

    #[test]
    fn member_at_first_line_means_no_header_window() {
        let syms = vec![
            sym("K", 1, 600, None, SymbolKind::Class),
            sym("K.a", 1, 300, Some(0), SymbolKind::Method),
            sym("K.b", 301, 600, Some(0), SymbolKind::Method),
        ];
        let mut ws = chunk(600, &syms);
        assign_keys(&mut ws, &syms);
        check_cover(&ws, 600);
        let keys: Vec<&str> = ws.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys, vec!["K.a", "K.b"]);
    }

    #[test]
    fn plain_text_splits_into_parts() {
        let ws = chunk(1200, &[]);
        check_cover(&ws, 1200);
        assert_eq!(ws.len(), 3);
    }
}
