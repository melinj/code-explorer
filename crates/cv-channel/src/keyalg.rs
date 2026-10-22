//! Key algebra shared by the matchers: rendering a resolved [`KeyExpr`] into segments, and
//! overlap tests between segment patterns (MQTT-style `*` = one segment, `#` = the rest).

use cv_lang::keys::{KeyExpr, KeyPart};

/// Renders a resolved key into text, with `hole` for each one-segment hole and `tail` for a tail.
/// Template tags written inside literals (`{instance}`, `${id}`, `:id`, `<id>`) are left as they
/// are: protocol packs decide what they mean.
pub fn render(key: &KeyExpr, hole: &str, tail: &str) -> String {
    let mut s = String::new();
    for p in &key.parts {
        match p {
            KeyPart::Lit(l) => s.push_str(l),
            KeyPart::Segment | KeyPart::Unknown { .. } | KeyPart::Ref { .. } => s.push_str(hole),
            KeyPart::Tail => s.push_str(tail),
        }
    }
    s
}

/// Replaces `{…}` template tags inside literal text with `with`.
pub fn replace_braces(s: &str, with: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '{' => {
                if depth == 0 {
                    out.push_str(with);
                }
                depth += 1;
            }
            '}' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            _ => out.push(c),
        }
    }
    out
}

/// Do two segment patterns overlap? `one` matches exactly one segment on either side and `rest`
/// matches zero or more trailing segments (MQTT `#` includes the parent level).
pub fn overlap(a: &[&str], b: &[&str], one: &str, rest: &str) -> bool {
    let (mut i, mut j) = (0, 0);
    loop {
        let (x, y) = (a.get(i), b.get(j));
        match (x, y) {
            (Some(&x), _) if x == rest => return true,
            (_, Some(&y)) if y == rest => return true,
            (None, None) => return true,
            (None, _) | (_, None) => return false,
            (Some(&x), Some(&y)) => {
                if !(x == y || x == one || y == one) {
                    return false;
                }
            }
        }
        i += 1;
        j += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(s: &str) -> Vec<&str> {
        s.split('.').collect()
    }

    #[test]
    fn mqtt_overlap() {
        let o = |a: &str, b: &str| overlap(&seg(a), &seg(b), "*", "#");
        assert!(o("world.entity.moved", "world.entity.moved"));
        assert!(o("world.entity.moved", "world.*.moved"));
        assert!(o("map.district.build.requested", "map.district.build.#"));
        assert!(o("tool.done.#", "tool.done"));
        assert!(o("tool.done.#", "tool.done.x1"));
        assert!(!o("world.entity.moved", "world.entity"));
        assert!(!o("a.b", "a.c"));
        assert!(o("world.turn.requested.*", "world.turn.requested.i7"));
        assert!(!o("world.turn.requested.*", "world.turn.requested"));
    }

    #[test]
    fn braces() {
        assert_eq!(replace_braces("a.{instance}.b", "*"), "a.*.b");
        assert_eq!(replace_braces("/node/{node_id}", "{}"), "/node/{}");
    }
}
