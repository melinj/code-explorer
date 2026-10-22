//! P2 HTTP channel pack: route handlers (FastAPI, Express, axum, cpp-httplib) meet clients (fetch,
//! axios, request() wrappers with prefixes, requests/httpx, reqwest, cpp-httplib) at
//! `METHOD /path/{}` hubs.
//!
//! Normalization: origins, query strings and fragments are dropped; path parameters in any spelling
//! (`{node_id}`, `${id}`, `:id`, `<id>`, a regex group `(\d+)`, a folded hole) become `{}`; a
//! leading base-URL hole is dropped. Matching: same method (or ANY) and the same segments, `{}`
//! matching any one segment.

use cv_channel::keyalg::{render, replace_braces};
use cv_channel::{ChannelKey, ChannelPack, EndpointFact, KeyExpr, Role, RuleSet};

pub struct HttpPack {
    rules: RuleSet,
}

const RULES: [&str; 6] = [
    include_str!("../../../packs/channels/http/python-fastapi.toml"),
    include_str!("../../../packs/channels/http/python-requests.toml"),
    include_str!("../../../packs/channels/http/typescript-fetch.toml"),
    include_str!("../../../packs/channels/http/typescript-express.toml"),
    include_str!("../../../packs/channels/http/rust-axum.toml"),
    include_str!("../../../packs/channels/http/cpp-httplib.toml"),
];

impl HttpPack {
    pub fn new() -> Self {
        let mut rules = RuleSet::default();
        for r in RULES {
            rules.extend(RuleSet::parse(r, None).expect("built-in http rules"));
        }
        HttpPack { rules }
    }
}

impl Default for HttpPack {
    fn default() -> Self {
        Self::new()
    }
}

const HOLE: &str = "\u{1}";

fn method(m: Option<&str>) -> &'static str {
    match m.map(|m| m.to_ascii_uppercase()).as_deref() {
        Some("GET") | None => "GET",
        Some("POST") => "POST",
        Some("PUT") => "PUT",
        Some("DELETE") => "DELETE",
        Some("PATCH") => "PATCH",
        Some("HEAD") => "HEAD",
        Some("OPTIONS") => "OPTIONS",
        Some("WEBSOCKET") | Some("WS") => "WS",
        _ => "ANY",
    }
}

/// The path of a key: `/api/node/{}`, or None if the key holds no path.
pub fn path(key: &KeyExpr) -> Option<String> {
    let raw = render(key, HOLE, HOLE);
    let mut s = raw.as_str();
    if let Some(i) = s.find("://") {
        s = &s[i + 3..];
        s = &s[s.find('/')?..];
    }
    // A leading base-URL hole (`${BASE}/api/x`): start at the first slash.
    if !s.starts_with('/') {
        s = &s[s.find('/')?..];
        if raw[..raw.len() - s.len()]
            .chars()
            .any(|c| c != HOLE.chars().next().unwrap())
        {
            // Text before the first slash that is not a hole (e.g. "user_id/x"): not a path.
            return None;
        }
    }
    let s = s.split(['?', '#']).next().unwrap_or("");
    let s = replace_braces(s, HOLE);
    let mut parts: Vec<&str> = s.split('/').filter(|x| !x.is_empty()).collect();
    // `/node/${id}/executions${qs}`: a hole glued to the end of the last segment is a query.
    if let Some(last) = parts.last_mut() {
        let t = last.trim_end_matches(HOLE);
        if !t.is_empty() && !t.contains(HOLE) {
            *last = t;
        }
    }
    let segs: Vec<String> = parts
        .into_iter()
        .map(|seg| {
            if seg.contains(HOLE)
                || seg.starts_with(':')
                || (seg.starts_with('<') && seg.ends_with('>'))
                // A regex group (cpp-httplib `R"(/n/(\d+))"`).
                || (seg.starts_with('(') && seg.ends_with(')'))
                || seg.starts_with('$')
            {
                "{}".to_string()
            } else {
                seg.to_string()
            }
        })
        .collect();
    Some(format!("/{}", segs.join("/")))
}

impl ChannelPack for HttpPack {
    fn protocol(&self) -> &'static str {
        "http"
    }
    fn rules(&self) -> &RuleSet {
        &self.rules
    }
    fn rules_mut(&mut self) -> &mut RuleSet {
        &mut self.rules
    }
    fn sides(&self) -> (&'static [Role], &'static [Role]) {
        (&[Role::Client], &[Role::Server])
    }
    fn accepts(&self, key: &KeyExpr, _role: Role) -> bool {
        // Fully dynamic keys stay (they become stubs); literal keys must hold a path.
        !key.has_lit() || path(key).is_some()
    }
    fn holes_are_exact(&self) -> bool {
        true
    }
    fn exact_wins(&self) -> bool {
        true
    }
    fn normalize(&self, key: &KeyExpr, _role: Role) -> ChannelKey {
        let Some(p) = path(key) else {
            return ChannelKey::Dynamic { hint: key.render() };
        };
        let m = method(key.method.as_deref());
        let k = format!("{m} {p}");
        if m == "ANY" || p.contains("{}") {
            ChannelKey::Pattern(k)
        } else {
            ChannelKey::Known(k)
        }
    }
    fn matches(&self, producer: &ChannelKey, consumer: &ChannelKey) -> bool {
        let (Some((pm, pp)), Some((cm, cp))) = (
            producer.text().split_once(' '),
            consumer.text().split_once(' '),
        ) else {
            return false;
        };
        if !(pm == cm || pm == "ANY" || cm == "ANY") {
            return false;
        }
        let (a, b): (Vec<&str>, Vec<&str>) = (pp.split('/').collect(), cp.split('/').collect());
        a.len() == b.len()
            && a.iter()
                .zip(&b)
                .all(|(x, y)| x == y || *x == "{}" || *y == "{}")
    }
    fn hub_label(&self, key: &ChannelKey) -> String {
        key.text().to_string()
    }
    fn link_kind(&self, _producer: &EndpointFact, _consumer: &EndpointFact) -> &'static str {
        "http-request"
    }
    fn stub_kind(&self, role: Role) -> &'static str {
        if role == Role::Server {
            "http-route"
        } else {
            "http-request"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_channel::KeyPart;

    fn key(parts: Vec<KeyPart>, m: Option<&str>) -> KeyExpr {
        KeyExpr {
            parts,
            method: m.map(String::from),
        }
    }

    #[test]
    fn prefix_and_template_meet_the_route() {
        let p = HttpPack::new();
        // Client: "/api" prefix + `/node/${id}?workflow=${w}`.
        let client = key(
            vec![
                KeyPart::Lit("/api/node/".into()),
                KeyPart::Segment,
                KeyPart::Lit("?workflow=".into()),
                KeyPart::Segment,
            ],
            None,
        );
        let route = key(
            vec![KeyPart::Lit("/api/node/{node_id}".into())],
            Some("get"),
        );
        let (c, r) = (
            p.normalize(&client, Role::Client),
            p.normalize(&route, Role::Server),
        );
        assert_eq!(r.text(), "GET /api/node/{}");
        assert_eq!(c.text(), "GET /api/node/{}");
        assert!(p.matches(&c, &r));
        let other = key(
            vec![KeyPart::Lit("/api/node/{node_id}".into())],
            Some("delete"),
        );
        assert!(!p.matches(&c, &p.normalize(&other, Role::Server)));
        // Origins and base-URL holes drop; express and flask parameters normalize.
        let abs = key(
            vec![KeyPart::Lit("http://localhost:8000/api/x/:id".into())],
            None,
        );
        assert_eq!(p.normalize(&abs, Role::Client).text(), "GET /api/x/{}");
        let based = key(
            vec![KeyPart::Segment, KeyPart::Lit("/api/<int:id>".into())],
            None,
        );
        assert_eq!(p.normalize(&based, Role::Client).text(), "GET /api/{}");
        assert!(!p.accepts(
            &key(vec![KeyPart::Lit("user_id".into())], None),
            Role::Client
        ));
        assert!(p.rules.endpoints.len() >= 6);
        let glued = key(
            vec![
                KeyPart::Lit("/node/".into()),
                KeyPart::Segment,
                KeyPart::Lit("/executions".into()),
                KeyPart::Unknown { hint: "qs".into() },
            ],
            None,
        );
        assert_eq!(
            p.normalize(&glued, Role::Client).text(),
            "GET /node/{}/executions"
        );
    }
}
