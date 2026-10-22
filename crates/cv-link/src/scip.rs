//! Tier 2 (DESIGN §4.5): SCIP ingestion.
//!
//! An `index.scip` (SCIP protobuf, <https://github.com/sourcegraph/scip>) found at the indexed
//! revision gives precise links: every reference occurrence of a symbol defined in another window
//! becomes a link at confidence 1.0 with provenance "scip", and `is_implementation` relationships
//! become `implement` links. Only the fields needed for that are decoded, by a small protobuf
//! reader ([`decode`]); unknown fields are skipped, so newer SCIP versions still load.
//!
//! Field numbers (scip.proto): `Index { documents = 2 }`; `Document { relative_path = 1,
//! occurrences = 2, symbols = 3 }`; `Occurrence { range = 1 (packed int32), symbol = 2,
//! symbol_roles = 3 }`; `SymbolInformation { symbol = 1, relationships = 4 }`;
//! `Relationship { symbol = 1, is_implementation = 3 }`. Role bit 0x1 = definition.

use std::collections::{BTreeMap, HashMap};

use cv_lang::RefKind;

use crate::{FileIn, LinkOut, window_at};

/// One occurrence: 1-based line, symbol, whether it defines the symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct Occurrence {
    pub line: u32,
    pub symbol: String,
    pub definition: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub path: String,
    pub occurrences: Vec<Occurrence>,
    /// (symbol, symbol it implements).
    pub implements: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Index {
    pub documents: Vec<Document>,
}

/// Where SCIP indexes are looked for at the indexed revision, in order.
pub const PATHS: [&str; 2] = ["index.scip", ".cv/index.scip"];

const DEFINITION: u64 = 0x1;

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.b.get(self.i).ok_or("truncated varint")?;
            self.i += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("varint too long".into())
    }

    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.varint()? as usize;
        let end = self.i.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or("truncated field")?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }

    /// Next (field number, wire type), or None at the end.
    fn key(&mut self) -> Result<Option<(u64, u8)>, String> {
        if self.i >= self.b.len() {
            return Ok(None);
        }
        let k = self.varint()?;
        Ok(Some((k >> 3, (k & 7) as u8)))
    }

    fn skip(&mut self, wire: u8) -> Result<(), String> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => self.i += 8,
            2 => {
                self.bytes()?;
            }
            5 => self.i += 4,
            w => return Err(format!("unsupported wire type {w}")),
        }
        if self.i > self.b.len() {
            return Err("truncated field".into());
        }
        Ok(())
    }

    /// Calls `f(field, wire, reader)` for each field; `f` returns false to skip it.
    fn fields(
        b: &'a [u8],
        mut f: impl FnMut(u64, u8, &mut Reader<'a>) -> Result<bool, String>,
    ) -> Result<(), String> {
        let mut r = Reader { b, i: 0 };
        while let Some((field, wire)) = r.key()? {
            if !f(field, wire, &mut r)? {
                r.skip(wire)?;
            }
        }
        Ok(())
    }
}

fn string(r: &mut Reader) -> Result<String, String> {
    Ok(String::from_utf8_lossy(r.bytes()?).into_owned())
}

fn occurrence(b: &[u8]) -> Result<Option<Occurrence>, String> {
    let (mut line, mut symbol, mut roles) = (None, String::new(), 0u64);
    Reader::fields(b, |field, wire, r| {
        Ok(match (field, wire) {
            (1, 2) => {
                // Packed range: [start_line, start_char, (end_line,) end_char].
                let mut p = Reader {
                    b: r.bytes()?,
                    i: 0,
                };
                if p.i < p.b.len() {
                    line = Some(p.varint()? as u32);
                }
                true
            }
            (1, 0) => {
                let v = r.varint()? as u32;
                line.get_or_insert(v);
                true
            }
            (2, 2) => {
                symbol = string(r)?;
                true
            }
            (3, 0) => {
                roles = r.varint()?;
                true
            }
            _ => false,
        })
    })?;
    Ok(line.filter(|_| !symbol.is_empty()).map(|l| Occurrence {
        line: l + 1,
        symbol,
        definition: roles & DEFINITION != 0,
    }))
}

fn symbol_info(b: &[u8], out: &mut Vec<(String, String)>) -> Result<(), String> {
    let mut sym = String::new();
    let mut implemented: Vec<String> = Vec::new();
    Reader::fields(b, |field, wire, r| {
        Ok(match (field, wire) {
            (1, 2) => {
                sym = string(r)?;
                true
            }
            (4, 2) => {
                let (mut target, mut is_impl) = (String::new(), false);
                Reader::fields(r.bytes()?, |f, w, rr| {
                    Ok(match (f, w) {
                        (1, 2) => {
                            target = string(rr)?;
                            true
                        }
                        (3, 0) => {
                            is_impl = rr.varint()? != 0;
                            true
                        }
                        _ => false,
                    })
                })?;
                if is_impl && !target.is_empty() {
                    implemented.push(target);
                }
                true
            }
            _ => false,
        })
    })?;
    if !sym.is_empty() {
        out.extend(implemented.into_iter().map(|t| (sym.clone(), t)));
    }
    Ok(())
}

fn document(b: &[u8]) -> Result<Document, String> {
    let mut d = Document::default();
    Reader::fields(b, |field, wire, r| {
        Ok(match (field, wire) {
            (1, 2) => {
                d.path = string(r)?;
                true
            }
            (2, 2) => {
                if let Some(o) = occurrence(r.bytes()?)? {
                    d.occurrences.push(o);
                }
                true
            }
            (3, 2) => {
                symbol_info(r.bytes()?, &mut d.implements)?;
                true
            }
            _ => false,
        })
    })?;
    Ok(d)
}

/// Decodes the parts of a SCIP `Index` that links need.
pub fn decode(bytes: &[u8]) -> Result<Index, String> {
    let mut ix = Index::default();
    Reader::fields(bytes, |field, wire, r| {
        Ok(match (field, wire) {
            (2, 2) => {
                ix.documents.push(document(r.bytes()?)?);
                true
            }
            _ => false,
        })
    })?;
    Ok(ix)
}

/// Readable name of a SCIP symbol: its last descriptor (`pkg/mod/Class#method().` → `Class.method`).
fn display(symbol: &str) -> String {
    let desc = symbol.rsplit(' ').next().unwrap_or(symbol);
    let parts: Vec<&str> = desc
        .split(['/', '#', '.'])
        .map(|p| p.split('(').next().unwrap_or(p).trim_matches('`'))
        .filter(|p| !p.is_empty() && !p.starts_with('[') && *p != ")")
        .collect();
    let member = desc.contains('#') && !desc.ends_with('#');
    match parts.as_slice() {
        [.., a, b] if member => format!("{a}.{b}"),
        [.., b] => b.to_string(),
        [] => symbol.to_string(),
    }
}

/// Precise links from a SCIP index over the project's files and windows. Local symbols
/// (`local N`) and occurrences outside indexed files are skipped. One link per (from window,
/// to window, kind) at the first reference line.
pub fn links(ix: &Index, files: &[FileIn]) -> Vec<LinkOut> {
    let by_path: HashMap<&str, usize> =
        files.iter().enumerate().map(|(i, f)| (f.path, i)).collect();
    let mut defs: HashMap<&str, (usize, u32)> = HashMap::new();
    for d in &ix.documents {
        let Some(&fi) = by_path.get(d.path.as_str()) else {
            continue;
        };
        for o in d.occurrences.iter().filter(|o| o.definition) {
            if !o.symbol.starts_with("local ") {
                defs.entry(o.symbol.as_str()).or_insert((fi, o.line));
            }
        }
    }
    let mut out: BTreeMap<(usize, usize, u8), LinkOut> = BTreeMap::new();
    let mut add = |from: (usize, u32), to: (usize, u32), kind: RefKind, symbol: &str| {
        let (Some(fw), Some(tw)) = (
            window_at(&files[from.0].windows, from.1),
            window_at(&files[to.0].windows, to.1),
        ) else {
            return;
        };
        if fw == tw {
            return;
        }
        out.entry((fw, tw, kind as u8)).or_insert_with(|| LinkOut {
            kind,
            from_window: fw,
            from_line: from.1,
            to_window: tw,
            to_line: to.1,
            confidence: 1.0,
            label: display(symbol),
            provenance: "scip",
        });
    };
    for d in &ix.documents {
        let Some(&fi) = by_path.get(d.path.as_str()) else {
            continue;
        };
        for o in d.occurrences.iter().filter(|o| !o.definition) {
            if let Some(&to) = defs.get(o.symbol.as_str()) {
                // Calls, and references to types, terms and fields: each is a dependency, drawn as
                // a call edge (the code-link kinds the layout weighs are call/inherit/implement).
                add((fi, o.line), to, RefKind::Call, &o.symbol);
            }
        }
        for (sym, iface) in &d.implements {
            if let (Some(&from), Some(&to)) = (defs.get(sym.as_str()), defs.get(iface.as_str())) {
                add(from, to, RefKind::Implement, iface);
            }
        }
    }
    out.into_values().collect()
}

/// Lines of each file (by input index) that a SCIP index covers with a reference: lexical
/// candidates on these lines give way to SCIP.
pub fn covered_lines(ix: &Index, files: &[FileIn]) -> HashMap<usize, Vec<u32>> {
    let by_path: HashMap<&str, usize> =
        files.iter().enumerate().map(|(i, f)| (f.path, i)).collect();
    let mut out: HashMap<usize, Vec<u32>> = HashMap::new();
    for d in &ix.documents {
        if let Some(&fi) = by_path.get(d.path.as_str()) {
            let v = out.entry(fi).or_default();
            v.extend(d.occurrences.iter().map(|o| o.line));
            v.sort_unstable();
            v.dedup();
        }
    }
    out
}

/// Merges SCIP links into lexical ones: SCIP wins for the same (from, to, kind), and lexical
/// candidates (confidence ≤ 0.5) on lines SCIP covers are dropped.
pub fn merge(
    lexical: Vec<LinkOut>,
    scip: Vec<LinkOut>,
    covered: &HashMap<usize, Vec<u32>>,
    files: &[FileIn],
) -> Vec<LinkOut> {
    let file_of_window: HashMap<usize, usize> = files
        .iter()
        .enumerate()
        .flat_map(|(fi, f)| f.windows.iter().map(move |w| (w.2, fi)))
        .collect();
    let mut out: BTreeMap<(usize, usize, u8), LinkOut> = BTreeMap::new();
    for l in scip {
        out.insert((l.from_window, l.to_window, l.kind as u8), l);
    }
    for l in lexical {
        if l.confidence <= 0.5
            && file_of_window
                .get(&l.from_window)
                .and_then(|fi| covered.get(fi))
                .is_some_and(|lines| lines.binary_search(&l.from_line).is_ok())
        {
            continue;
        }
        out.entry((l.from_window, l.to_window, l.kind as u8))
            .or_insert(l);
    }
    out.into_values().collect()
}

/// Test support: encodes an [`Index`] as SCIP protobuf (the fields [`decode`] reads, plus a
/// metadata field it must skip).
pub fn encode(ix: &Index) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    fn field(out: &mut Vec<u8>, n: u64, bytes: &[u8]) {
        varint(out, n << 3 | 2);
        varint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }
    fn uint(out: &mut Vec<u8>, n: u64, v: u64) {
        varint(out, n << 3);
        varint(out, v);
    }
    let mut out = Vec::new();
    // Metadata { version = 1, tool_info = 2 { name = 1 } }: skipped by the decoder.
    let mut tool = Vec::new();
    field(&mut tool, 1, b"cv-test");
    let mut meta = Vec::new();
    uint(&mut meta, 1, 0);
    field(&mut meta, 2, &tool);
    field(&mut out, 1, &meta);
    for d in &ix.documents {
        let mut doc = Vec::new();
        field(&mut doc, 4, b"any-language"); // language: skipped
        field(&mut doc, 1, d.path.as_bytes());
        for o in &d.occurrences {
            let mut occ = Vec::new();
            let mut range = Vec::new();
            for v in [o.line as u64 - 1, 4, 12] {
                varint(&mut range, v);
            }
            field(&mut occ, 1, &range);
            field(&mut occ, 2, o.symbol.as_bytes());
            if o.definition {
                uint(&mut occ, 3, DEFINITION);
            }
            field(&mut doc, 2, &occ);
        }
        for (sym, iface) in &d.implements {
            let mut rel = Vec::new();
            field(&mut rel, 1, iface.as_bytes());
            uint(&mut rel, 3, 1);
            let mut info = Vec::new();
            field(&mut info, 1, sym.as_bytes());
            field(&mut info, 4, &rel);
            field(&mut doc, 3, &info);
        }
        field(&mut out, 2, &doc);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occ(line: u32, symbol: &str, definition: bool) -> Occurrence {
        Occurrence {
            line,
            symbol: symbol.into(),
            definition,
        }
    }

    const HELPER: &str = "scip-python python app 0.1 `lib`/helper().";
    const BASE: &str = "scip-python python app 0.1 `lib`/Base#";
    const IMPL: &str = "scip-python python app 0.1 `app`/Impl#";

    fn sample() -> Index {
        Index {
            documents: vec![
                Document {
                    path: "lib.py".into(),
                    occurrences: vec![occ(1, HELPER, true), occ(20, BASE, true)],
                    implements: vec![],
                },
                Document {
                    path: "app.py".into(),
                    occurrences: vec![
                        occ(3, HELPER, false),
                        occ(5, "local 3", false),
                        occ(30, IMPL, true),
                        occ(12, HELPER, false),
                    ],
                    implements: vec![(IMPL.into(), BASE.into())],
                },
                Document {
                    path: "not/indexed.py".into(),
                    occurrences: vec![occ(1, HELPER, false)],
                    implements: vec![],
                },
            ],
        }
    }

    #[test]
    fn decodes_what_it_encodes_and_skips_unknown_fields() {
        let ix = sample();
        let bytes = encode(&ix);
        assert_eq!(decode(&bytes).unwrap(), ix);
        assert!(decode(&bytes[..bytes.len() - 3]).is_err());
        assert_eq!(display(HELPER), "helper");
        assert_eq!(display("s p m 1 `a`/Bus#emit()."), "Bus.emit");
    }

    #[test]
    fn references_become_precise_links_and_replace_candidates() {
        let ix = decode(&encode(&sample())).unwrap();
        let files = vec![
            FileIn {
                path: "lib.py",
                lane: "code",
                lang: Some("python"),
                conv: cv_lang::Conventions::NONE,
                facts: None,
                imports: vec![],
                windows: vec![(1, 10, 0), (11, 40, 1)],
            },
            FileIn {
                path: "app.py",
                lane: "code",
                lang: Some("python"),
                conv: cv_lang::Conventions::NONE,
                facts: None,
                imports: vec![],
                windows: vec![(1, 10, 2), (11, 25, 3), (26, 40, 4)],
            },
        ];
        let ls = links(&ix, &files);
        let got: Vec<(usize, u32, usize, u32, &str, &str)> = ls
            .iter()
            .map(|l| {
                (
                    l.from_window,
                    l.from_line,
                    l.to_window,
                    l.to_line,
                    l.kind.as_str(),
                    l.label.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (2, 3, 0, 1, "call", "helper"),
                (3, 12, 0, 1, "call", "helper"),
                (4, 30, 1, 20, "implement", "Base"),
            ]
        );
        assert!(
            ls.iter()
                .all(|l| l.confidence == 1.0 && l.provenance == "scip")
        );

        // A lexical candidate on a covered line gives way; one elsewhere stays; a precise lexical
        // link for the same pair is replaced by SCIP's.
        let lex = |from: usize, line: u32, to: usize, c: f64| LinkOut {
            kind: RefKind::Call,
            from_window: from,
            from_line: line,
            to_window: to,
            to_line: 1,
            confidence: c,
            label: "x".into(),
            provenance: "tree-sitter",
        };
        let covered = covered_lines(&ix, &files);
        let merged = merge(
            vec![lex(2, 3, 1, 0.5), lex(2, 8, 1, 0.5), lex(2, 3, 0, 0.85)],
            ls,
            &covered,
            &files,
        );
        assert_eq!(merged.len(), 4);
        assert!(
            merged
                .iter()
                .any(|l| l.from_line == 8 && l.confidence == 0.5)
        );
        assert!(
            merged
                .iter()
                .filter(|l| (l.from_window, l.to_window) == (2, 0))
                .all(|l| l.provenance == "scip")
        );
    }
}
