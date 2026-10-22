//! Fake source text with highlight tokens, generated per file from its seed, plus the silhouettes
//! (wire.ts OutlineEncoding) derived from it.

use crate::synth::{FileInfo, Rng};

// wire.ts TokenClass
const PLAIN: u32 = 0;
const KEYWORD: u32 = 1;
const STRING: u32 = 2;
const COMMENT: u32 = 3;
const NUMBER: u32 = 4;
const FUNCTION: u32 = 5;
const TYPE: u32 = 6;
const VARIABLE: u32 = 7;
const OPERATOR: u32 = 8;
const PUNCT: u32 = 9;
const DECORATOR: u32 = 10;
const PROPERTY: u32 = 12;
const BLANK: u32 = 15;

const IDENTS: [&str; 24] = [
    "state", "event", "district", "request", "payload", "store", "node", "result", "items",
    "config", "path", "entity", "position", "bus", "queue", "cache", "plan", "session", "target",
    "value", "key", "turn", "graph", "edge",
];
const TYPES: [&str; 12] = [
    "District", "Event", "Node", "Store", "Plan", "Request", "Session", "Graph", "Entity", "Turn",
    "Config", "Queue",
];

#[derive(Clone, Default)]
pub struct Line {
    pub text: String,
    pub tokens: Vec<u32>,
}

impl Line {
    fn push(&mut self, s: &str, cls: u32) {
        if cls != PLAIN && !s.is_empty() {
            self.tokens.extend([
                self.text.encode_utf16().count() as u32,
                s.encode_utf16().count() as u32,
                cls,
            ]);
        }
        self.text.push_str(s);
    }
}

fn pick<'a>(rng: &mut Rng, xs: &[&'a str]) -> &'a str {
    xs[(rng.next() % xs.len() as u64) as usize]
}

pub fn file_lines(file: &FileInfo) -> Vec<Line> {
    let mut rng = Rng::new(file.seed);
    let mut out = Vec::with_capacity(file.lines as usize);
    let mut depth: usize = 0;
    for _ in 0..file.lines {
        let mut l = Line::default();
        let roll = rng.f();
        if roll < 0.08 {
            out.push(l);
            continue;
        }
        let unit = if file.lang == "python" { 4 } else { 2 };
        l.push(&" ".repeat(depth * unit), PLAIN);
        match file.lang.as_str() {
            "python" => py_line(&mut rng, &mut l, &mut depth, roll),
            "markdown" => md_line(&mut rng, &mut l, roll),
            _ => yaml_line(&mut rng, &mut l, &mut depth, roll),
        }
        out.push(l);
    }
    out
}

fn py_line(rng: &mut Rng, l: &mut Line, depth: &mut usize, roll: f64) {
    let a = pick(rng, &IDENTS);
    let b = pick(rng, &IDENTS);
    let t = pick(rng, &TYPES);
    if *depth == 0 && roll < 0.35 {
        if roll < 0.12 {
            l.push("@traced", DECORATOR);
            l.push("(", PUNCT);
            l.push(&format!("\"{a}.{b}\""), STRING);
            l.push(")", PUNCT);
        } else if roll < 0.2 {
            l.push("class", KEYWORD);
            l.push(" ", PLAIN);
            l.push(&format!("{t}{}", pick(rng, &TYPES)), TYPE);
            l.push(":", PUNCT);
            *depth = 1;
        } else {
            l.push("def", KEYWORD);
            l.push(" ", PLAIN);
            l.push(&format!("{a}_{b}"), FUNCTION);
            l.push("(", PUNCT);
            l.push(b, VARIABLE);
            l.push(": ", PUNCT);
            l.push(t, TYPE);
            l.push(") -> ", PUNCT);
            l.push(t, TYPE);
            l.push(":", PUNCT);
            *depth = 1;
        }
        return;
    }
    if *depth == 0 {
        *depth = 1;
    }
    if roll < 0.18 && *depth < 4 {
        l.push(pick(rng, &["if", "for", "while", "with"]), KEYWORD);
        l.push(" ", PLAIN);
        l.push(a, VARIABLE);
        l.push(" ", PLAIN);
        l.push(pick(rng, &["is not", "in", "and"]), KEYWORD);
        l.push(" ", PLAIN);
        l.push(&format!("self.{b}"), PROPERTY);
        l.push(":", PUNCT);
        *depth += 1;
    } else if roll < 0.3 {
        l.push("return", KEYWORD);
        l.push(" ", PLAIN);
        l.push(a, VARIABLE);
        *depth = depth.saturating_sub(1 + (rng.next() % 2) as usize);
    } else if roll < 0.38 {
        l.push(&format!("# {a} {b} {}", pick(rng, &IDENTS)), COMMENT);
    } else if roll < 0.5 {
        l.push(a, VARIABLE);
        l.push(" = ", OPERATOR);
        l.push(&format!("{}", rng.range(0, 500)), NUMBER);
    } else {
        l.push(a, VARIABLE);
        l.push(" = ", OPERATOR);
        l.push(&format!("self.{b}"), PROPERTY);
        l.push(".", PUNCT);
        l.push(
            pick(rng, &["emit", "get", "put", "build", "resolve", "apply"]),
            FUNCTION,
        );
        l.push("(", PUNCT);
        l.push(pick(rng, &IDENTS), VARIABLE);
        l.push(", ", PUNCT);
        l.push(&format!("\"{}\"", pick(rng, &IDENTS)), STRING);
        l.push(")", PUNCT);
        if roll > 0.95 {
            *depth = depth.saturating_sub(1);
        }
    }
}

fn yaml_line(rng: &mut Rng, l: &mut Line, depth: &mut usize, roll: f64) {
    let a = pick(rng, &IDENTS);
    if roll < 0.3 && *depth < 4 {
        l.push(a, PROPERTY);
        l.push(":", PUNCT);
        *depth += 1;
    } else if roll < 0.75 {
        l.push(a, PROPERTY);
        l.push(": ", PUNCT);
        if roll < 0.5 {
            l.push(&format!("{}", rng.range(0, 9000)), NUMBER);
        } else {
            l.push(
                &format!("{}.{}", pick(rng, &IDENTS), pick(rng, &IDENTS)),
                STRING,
            );
        }
    } else if roll < 0.82 {
        l.push(&format!("# {a}"), COMMENT);
    } else {
        l.push("- ", PUNCT);
        l.push(a, STRING);
        if roll > 0.93 {
            *depth = depth.saturating_sub(1);
        }
    }
}

fn md_line(rng: &mut Rng, l: &mut Line, roll: f64) {
    if roll < 0.15 {
        l.push(
            &format!("## {} {}", pick(rng, &TYPES), pick(rng, &IDENTS)),
            KEYWORD,
        );
    } else {
        let n = rng.range(4, 14);
        let words: Vec<&str> = (0..n).map(|_| pick(rng, &IDENTS)).collect();
        l.push(&words.join(" "), PLAIN);
    }
}

/// OutlineEncoding records for lines `start..=end` (1-based), 3 bytes per line.
pub fn silhouette(lines: &[Line], start: u32, end: u32, port_lines: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(((end - start + 1) * 3) as usize);
    let blank = Line::default();
    for n in start..=end {
        // Edited files can be shorter than the window; missing lines are blank.
        let line = lines.get((n - 1) as usize).unwrap_or(&blank);
        let indent = line.text.len() - line.text.trim_start().len();
        let len = line.text.trim().len();
        let mut weight = [0u32; 16];
        for t in line.tokens.chunks(3) {
            weight[t[2] as usize] += t[1];
        }
        let dominant = if len == 0 {
            BLANK
        } else {
            (1..15)
                .max_by_key(|&c| weight[c as usize])
                .filter(|&c| weight[c as usize] > 0)
                .unwrap_or(PLAIN)
        };
        let port = if port_lines.contains(&n) { 0x40 } else { 0 };
        out.extend([
            indent.min(255) as u8,
            len.min(255) as u8,
            dominant as u8 | port,
        ]);
    }
    out
}
