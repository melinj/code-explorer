//! P3 env channel pack: config and infra files define environment variables (dotenv, Dockerfile
//! ENV/ARG, compose, k8s, CI, shell `export`); code reads them (os.getenv, process.env,
//! std::env::var, getenv, and pydantic-settings `BaseSettings` fields honouring `env_prefix`).
//! Definers meet readers at one `env:NAME` hub per variable; names match case-insensitively
//! (pydantic-settings' default), with `*` holes from folded prefixes.

use cv_channel::{ChannelKey, ChannelPack, EndpointFact, KeyExpr, Role, RuleSet};

pub struct EnvPack {
    rules: RuleSet,
}

const RULES: [&str; 5] = [
    include_str!("../../../packs/channels/env/python-os.toml"),
    include_str!("../../../packs/channels/env/typescript-process.toml"),
    include_str!("../../../packs/channels/env/rust-std.toml"),
    include_str!("../../../packs/channels/env/cpp-getenv.toml"),
    include_str!("../../../packs/channels/env/infra-env.toml"),
];

impl EnvPack {
    pub fn new() -> Self {
        let mut rules = RuleSet::default();
        for r in RULES {
            rules.extend(RuleSet::parse(r, None).expect("built-in env rules"));
        }
        EnvPack { rules }
    }
}

impl Default for EnvPack {
    fn default() -> Self {
        Self::new()
    }
}

fn name(key: &KeyExpr) -> String {
    cv_channel::keyalg::render(key, "*", "*").to_ascii_uppercase()
}

fn glob(p: &str, s: &str) -> bool {
    match p.split_once('*') {
        None => p == s,
        Some((a, rest)) => {
            s.starts_with(a) && (0..=s.len() - a.len()).any(|i| glob(rest, &s[a.len() + i..]))
        }
    }
}

impl ChannelPack for EnvPack {
    fn protocol(&self) -> &'static str {
        "env"
    }
    fn rules(&self) -> &RuleSet {
        &self.rules
    }
    fn rules_mut(&mut self) -> &mut RuleSet {
        &mut self.rules
    }
    fn sides(&self) -> (&'static [Role], &'static [Role]) {
        (&[Role::Define], &[Role::Read])
    }
    fn accepts(&self, key: &KeyExpr, _role: Role) -> bool {
        let n = name(key);
        !key.has_lit()
            || (n
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '*')
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '*'))
    }
    fn normalize(&self, key: &KeyExpr, _role: Role) -> ChannelKey {
        let n = name(key);
        if n.contains('*') {
            ChannelKey::Pattern(n)
        } else {
            ChannelKey::Known(n)
        }
    }
    fn matches(&self, producer: &ChannelKey, consumer: &ChannelKey) -> bool {
        match (producer, consumer) {
            (ChannelKey::Pattern(p), ChannelKey::Known(c)) => glob(p, c),
            (ChannelKey::Known(p), ChannelKey::Pattern(c)) => glob(c, p),
            (a, b) => a.text() == b.text(),
        }
    }
    fn hub_label(&self, key: &ChannelKey) -> String {
        format!("env:{}", key.text())
    }
    fn link_kind(&self, _producer: &EndpointFact, _consumer: &EndpointFact) -> &'static str {
        "define"
    }
    fn stub_kind(&self, role: Role) -> &'static str {
        if role == Role::Define {
            "define"
        } else {
            "read"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_channel::KeyPart;

    #[test]
    fn names_match_case_insensitively_with_prefix_holes() {
        let p = EnvPack::new();
        let d = p.normalize(&KeyExpr::lit("DATABASE_URL"), Role::Define);
        let r = p.normalize(&KeyExpr::lit("database_url"), Role::Read);
        assert!(p.matches(&d, &r));
        let mut k = KeyExpr::default();
        k.push(KeyPart::Segment);
        k.push(KeyPart::Lit("_URL".into()));
        assert!(p.matches(&d, &p.normalize(&k, Role::Read)));
        assert!(!p.accepts(&KeyExpr::lit("not a name"), Role::Read));
        assert_eq!(p.hub_label(&d), "env:DATABASE_URL");
        assert!(p.rules.config.len() >= 8);
    }
}
