//! A compact binary form of [`BlobFacts`] for the parse cache (B3: content-addressed by blob ×
//! pack version, stored in index.db). Varints and length-prefixed strings; [`VERSION`] changes
//! whenever the layout does, and cache keys include it.

use crate::keys::ConfigKeyFact;
use crate::{BlobFacts, ImportFact, RefFact, RefKind, SymbolFact, SymbolKind};

/// Bumped on any change to the encoding (or to the fact types).
pub const VERSION: u32 = 1;

const SYMBOL_KINDS: [SymbolKind; 11] = [
    SymbolKind::Function,
    SymbolKind::Method,
    SymbolKind::Class,
    SymbolKind::Interface,
    SymbolKind::Struct,
    SymbolKind::Enum,
    SymbolKind::Module,
    SymbolKind::Constant,
    SymbolKind::Type,
    SymbolKind::Test,
    SymbolKind::ConfigBlock,
];
const REF_KINDS: [RefKind; 3] = [RefKind::Call, RefKind::Inherit, RefKind::Implement];

struct W(Vec<u8>);

impl W {
    fn u(&mut self, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.0.push(b);
                return;
            }
            self.0.push(b | 0x80);
        }
    }
    fn s(&mut self, s: &str) {
        self.u(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }
    /// Optional value: 0 for none, else value + 1.
    fn opt(&mut self, v: Option<usize>) {
        self.u(v.map_or(0, |x| x as u64 + 1));
    }
    fn opt_s(&mut self, s: Option<&str>) {
        match s {
            None => self.u(0),
            Some(s) => {
                self.u(1);
                self.s(s);
            }
        }
    }
}

struct R<'a> {
    b: &'a [u8],
    i: usize,
}

impl R<'_> {
    fn u(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.b.get(self.i)?;
            self.i += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    fn n(&mut self) -> Option<usize> {
        let v = self.u()? as usize;
        // A length can never exceed the bytes left (each item takes at least one byte).
        (v <= self.b.len() - self.i.min(self.b.len())).then_some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        u32::try_from(self.u()?).ok()
    }
    fn s(&mut self) -> Option<String> {
        let n = self.n()?;
        let s = std::str::from_utf8(self.b.get(self.i..self.i + n)?).ok()?;
        self.i += n;
        Some(s.to_string())
    }
    fn opt(&mut self) -> Option<Option<usize>> {
        Some(match self.u()? {
            0 => None,
            v => Some(v as usize - 1),
        })
    }
    fn opt_s(&mut self) -> Option<Option<String>> {
        Some(match self.u()? {
            0 => None,
            _ => Some(self.s()?),
        })
    }
}

pub fn encode(f: &BlobFacts) -> Vec<u8> {
    let mut w = W(Vec::with_capacity(64 + f.tokens.len() * 16));
    w.u(u64::from(VERSION));
    w.u(u64::from(f.lines));
    w.u(u64::from(f.syntax_errors));
    w.u(f.symbols.len() as u64);
    for s in &f.symbols {
        w.u(SYMBOL_KINDS.iter().position(|k| *k == s.kind).unwrap_or(0) as u64);
        w.s(&s.name);
        w.s(&s.qualname);
        w.opt(s.parent);
        w.u(u64::from(s.start));
        w.u(u64::from(s.end));
        w.u(u64::from(s.name_line));
    }
    w.u(f.refs.len() as u64);
    for r in &f.refs {
        w.u(REF_KINDS.iter().position(|k| *k == r.kind).unwrap_or(0) as u64);
        w.s(&r.name);
        w.opt_s(r.qualifier.as_deref());
        w.u(u64::from(r.line));
        w.opt(r.enclosing);
    }
    w.u(f.imports.len() as u64);
    for i in &f.imports {
        w.s(&i.local);
        w.s(&i.module);
        w.opt_s(i.imported.as_deref());
        w.u(u64::from(i.line));
    }
    w.u(f.tokens.len() as u64);
    for line in &f.tokens {
        w.u(line.len() as u64);
        // Triples [start, len, class]: starts delta-coded within the line.
        let mut prev = 0u32;
        for t in line.chunks(3) {
            match *t {
                [start, len, class] => {
                    w.u(u64::from(start.wrapping_sub(prev)));
                    w.u(u64::from(len));
                    w.u(u64::from(class));
                    prev = start;
                }
                _ => t.iter().for_each(|&x| w.u(u64::from(x))),
            }
        }
    }
    w.u(f.config_keys.len() as u64);
    for k in &f.config_keys {
        w.s(&k.path);
        w.s(&k.key);
        w.u(u64::from(k.line));
        w.opt_s(k.value.as_deref());
        w.u(u64::from(k.item));
    }
    w.0
}

/// `None` for a different [`VERSION`] or damaged bytes (the caller re-parses).
pub fn decode(b: &[u8]) -> Option<BlobFacts> {
    let mut r = R { b, i: 0 };
    if r.u32()? != VERSION {
        return None;
    }
    let mut f = BlobFacts {
        lines: r.u32()?,
        syntax_errors: r.u32()?,
        ..Default::default()
    };
    for _ in 0..r.n()? {
        f.symbols.push(SymbolFact {
            kind: *SYMBOL_KINDS.get(r.u()? as usize)?,
            name: r.s()?,
            qualname: r.s()?,
            parent: r.opt()?,
            start: r.u32()?,
            end: r.u32()?,
            name_line: r.u32()?,
        });
    }
    for _ in 0..r.n()? {
        f.refs.push(RefFact {
            kind: *REF_KINDS.get(r.u()? as usize)?,
            name: r.s()?,
            qualifier: r.opt_s()?,
            line: r.u32()?,
            enclosing: r.opt()?,
        });
    }
    for _ in 0..r.n()? {
        f.imports.push(ImportFact {
            local: r.s()?,
            module: r.s()?,
            imported: r.opt_s()?,
            line: r.u32()?,
        });
    }
    let n = r.n()?;
    f.tokens.reserve(n);
    for _ in 0..n {
        let len = r.n()?;
        let mut line = Vec::with_capacity(len);
        let mut prev = 0u32;
        for _ in 0..len / 3 {
            let start = prev.wrapping_add(r.u32()?);
            line.extend([start, r.u32()?, r.u32()?]);
            prev = start;
        }
        for _ in 0..len % 3 {
            line.push(r.u32()?);
        }
        f.tokens.push(line);
    }
    for _ in 0..r.n()? {
        f.config_keys.push(ConfigKeyFact {
            path: r.s()?,
            key: r.s()?,
            line: r.u32()?,
            value: r.opt_s()?,
            item: r.u()? != 0,
        });
    }
    (r.i == b.len()).then_some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_field() {
        let f = BlobFacts {
            lines: 3,
            syntax_errors: 1,
            symbols: vec![
                SymbolFact {
                    kind: SymbolKind::Class,
                    name: "Bus".into(),
                    qualname: "pkg.Bus".into(),
                    parent: None,
                    start: 1,
                    end: 3,
                    name_line: 1,
                },
                SymbolFact {
                    kind: SymbolKind::ConfigBlock,
                    name: "émit".into(),
                    qualname: "pkg.Bus.émit".into(),
                    parent: Some(0),
                    start: 2,
                    end: 3,
                    name_line: 2,
                },
            ],
            refs: vec![RefFact {
                kind: RefKind::Implement,
                name: "Base".into(),
                qualifier: Some("self".into()),
                line: 2,
                enclosing: Some(1),
            }],
            imports: vec![ImportFact {
                local: "x".into(),
                module: "..m".into(),
                imported: Some("*".into()),
                line: 1,
            }],
            tokens: vec![vec![0, 3, 1, 4, 3, 6], vec![], vec![8, 2, 2, 1, 1, 9, 7]],
            config_keys: vec![ConfigKeyFact {
                path: "a.b".into(),
                key: "b".into(),
                line: 3,
                value: None,
                item: true,
            }],
        };
        let b = encode(&f);
        let g = decode(&b).unwrap();
        assert_eq!(format!("{f:?}"), format!("{g:?}"));
        assert!(decode(&b[..b.len() - 1]).is_none());
        let mut other = b.clone();
        other[0] = 99;
        assert!(decode(&other).is_none());
        assert!(decode(&[]).is_none());
    }
}
