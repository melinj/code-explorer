//! P1 bus channel pack: publish/subscribe topics. Keys are dotted topics; `{…}` template tags and
//! folding holes are one-segment wildcards (`*`), transforms add an optional tail (`#`), and
//! producers meet consumers with MQTT matching (`*` = one segment, `#` = the rest).

use cv_channel::keyalg::{overlap, replace_braces};
use cv_channel::{ChannelKey, ChannelPack, EndpointFact, KeyExpr, KeyPart, Role, RuleSet};

pub struct BusPack {
    rules: RuleSet,
}

const RULES: [&str; 2] = [
    include_str!("../../../packs/channels/bus/python-bus.toml"),
    include_str!("../../../packs/channels/bus/yaml-topics.toml"),
];

impl BusPack {
    pub fn new() -> Self {
        let mut rules = RuleSet::default();
        for r in RULES {
            rules.extend(RuleSet::parse(r, None).expect("built-in bus rules"));
        }
        BusPack { rules }
    }
}

impl Default for BusPack {
    fn default() -> Self {
        Self::new()
    }
}

/// `world.{kind}.moved` → `world.*.moved`; a tail becomes a `#` segment.
pub fn topic(key: &KeyExpr) -> String {
    let mut s = String::new();
    for p in &key.parts {
        match p {
            KeyPart::Lit(l) => s.push_str(&replace_braces(l, "*")),
            KeyPart::Segment | KeyPart::Unknown { .. } | KeyPart::Ref { .. } => s.push('*'),
            KeyPart::Tail => {
                if !s.is_empty() && !s.ends_with('.') {
                    s.push('.');
                }
                s.push('#');
            }
        }
    }
    s.split('.')
        .map(|seg| {
            if seg.contains('#') {
                "#"
            } else if seg.contains('*') {
                "*"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

impl ChannelPack for BusPack {
    fn protocol(&self) -> &'static str {
        "bus"
    }
    fn rules(&self) -> &RuleSet {
        &self.rules
    }
    fn rules_mut(&mut self) -> &mut RuleSet {
        &mut self.rules
    }
    fn sides(&self) -> (&'static [Role], &'static [Role]) {
        (&[Role::Producer], &[Role::Consumer])
    }
    fn normalize(&self, key: &KeyExpr, _role: Role) -> ChannelKey {
        let t = topic(key);
        // A subscription to everything (`#`, `*.#`) says nothing about the wiring.
        if t.split('.').all(|s| s == "*" || s == "#") {
            return ChannelKey::Dynamic { hint: t };
        }
        if t.split('.').any(|s| s == "*" || s == "#") {
            ChannelKey::Pattern(t)
        } else {
            ChannelKey::Known(t)
        }
    }
    fn matches(&self, producer: &ChannelKey, consumer: &ChannelKey) -> bool {
        let (p, c) = (producer.text(), consumer.text());
        if matches!(
            (producer, consumer),
            (ChannelKey::Known(_), ChannelKey::Known(_))
        ) {
            return p == c;
        }
        let (a, b): (Vec<&str>, Vec<&str>) = (p.split('.').collect(), c.split('.').collect());
        overlap(&a, &b, "*", "#")
    }
    fn hub_label(&self, key: &ChannelKey) -> String {
        key.text().to_string()
    }
    fn link_kind(&self, producer: &EndpointFact, consumer: &EndpointFact) -> &'static str {
        if producer.from_config {
            "config"
        } else if consumer.from_config {
            "subscribe"
        } else {
            "publish"
        }
    }
    fn stub_kind(&self, role: Role) -> &'static str {
        if role == Role::Consumer {
            "subscribe"
        } else {
            "publish"
        }
    }
    fn dispatch_kind(&self) -> Option<&'static str> {
        Some("dispatch")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_normalize_and_match() {
        let p = BusPack::new();
        assert!(p.rules.endpoints.len() >= 3 && p.rules.config.len() == 2);
        let mut k = KeyExpr::lit("world.");
        k.push(KeyPart::Unknown {
            hint: "kind".into(),
        });
        k.push(KeyPart::Lit(".moved".into()));
        assert_eq!(
            p.normalize(&k, Role::Producer),
            ChannelKey::Pattern("world.*.moved".into())
        );
        let mut t = KeyExpr::lit("tool.done");
        t.push(KeyPart::Tail);
        assert_eq!(topic(&t), "tool.done.#");
        assert_eq!(
            topic(&KeyExpr::lit("world.turn.requested.{instance}")),
            "world.turn.requested.*"
        );
        let known = |s: &str| ChannelKey::Known(s.into());
        let pat = |s: &str| ChannelKey::Pattern(s.into());
        assert!(p.matches(&known("world.entity.moved"), &known("world.entity.moved")));
        assert!(p.matches(
            &known("map.district.build.requested"),
            &pat("map.district.build.*")
        ));
        assert!(p.matches(&pat("tool.done.#"), &known("tool.done")));
        assert!(!p.matches(&known("a.b"), &known("a.c")));
    }
}
