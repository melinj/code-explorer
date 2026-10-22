//! Rules as data (DESIGN §4.6, Appendix B): endpoint queries per language pack, config-file rules
//! and key transforms, read from TOML. Built-in rule files live in
//! `packs/channels/<protocol>/<lang>-<framework>.toml`; project rules in `.cv/links.toml` use the
//! same tables under a protocol prefix (`[[bus.endpoints]]`, `[[env.settings]]`, …).

use globset::{Glob, GlobMatcher};
use regex::Regex;
use toml_edit::{DocumentMut, Item, Table};

use crate::Role;

#[derive(Debug, Clone)]
pub struct EndpointRule {
    pub id: String,
    /// Language pack id the query is written for.
    pub lang: String,
    pub role: Role,
    /// tree-sitter query; captures `@key` (folded) or `@name` (literal text), `@prefix`, `@scope`,
    /// `@handler`, `@method`, `@line`.
    pub query: String,
    pub method: Option<String>,
    /// Static prefix, or `$NAME`: the value of constant NAME in the same file.
    pub prefix: Option<String>,
    /// Pre-filter: only files containing one of these strings run the query.
    pub hints: Vec<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFrom {
    /// The last path segment (`ENV.PORT` → `PORT`).
    Leaf,
    /// The value (`topic: world.entity.moved`).
    Value,
    /// A `NAME=value` list item → `NAME`.
    ItemName,
}

#[derive(Debug, Clone)]
pub struct ConfigRule {
    pub id: String,
    pub glob: GlobMatcher,
    pub glob_src: String,
    /// `a.b.c` exact, `*` one segment, `$..x` any prefix (JSONPath-like).
    pub path: String,
    pub role: Role,
    pub key_from: KeyFrom,
    /// Match plain list items (`- value`) instead of keys.
    pub items: bool,
    /// The value (or derived key) must match this.
    pub value_pattern: Option<Regex>,
    /// `key=substring`: the nearest preceding sibling-or-ancestor key `key` has a value containing
    /// the substring (e.g. `config_id=pub` for a publishing workflow node).
    pub when: Option<(String, String)>,
    pub confidence: f32,
}

#[derive(Debug, Clone)]
pub struct KeyTransform {
    /// Function that rewrites a key, e.g. "scoped_topic".
    pub function: String,
    /// Result template, e.g. "{0}.*?" (an optional extra segment).
    pub result: String,
}

#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    pub endpoints: Vec<EndpointRule>,
    pub config: Vec<ConfigRule>,
    pub transforms: Vec<KeyTransform>,
}

impl RuleSet {
    pub fn extend(&mut self, other: RuleSet) {
        self.endpoints.extend(other.endpoints);
        self.config.extend(other.config);
        self.transforms.extend(other.transforms);
    }

    /// Parses a rules file. With `section`, reads the tables under that protocol prefix
    /// (`.cv/links.toml`); without, the top-level tables (a built-in rule file).
    pub fn parse(src: &str, section: Option<&str>) -> Result<RuleSet, String> {
        let doc: DocumentMut = src.parse().map_err(|e| format!("{e}"))?;
        let root: &Table = match section {
            None => doc.as_table(),
            Some(s) => match doc.get(s).and_then(Item::as_table) {
                Some(t) => t,
                None => return Ok(RuleSet::default()),
            },
        };
        let mut out = RuleSet::default();
        for t in tables(root, "endpoints") {
            out.endpoints.push(endpoint(t)?);
        }
        for t in tables(root, "settings") {
            out.endpoints.extend(settings(t)?);
        }
        for t in tables(root, "config") {
            out.config.push(config(t)?);
        }
        for t in tables(root, "transforms") {
            out.transforms.push(KeyTransform {
                function: req(t, "fn").or_else(|_| req(t, "function"))?,
                result: req(t, "result")?,
            });
        }
        Ok(out)
    }
}

fn tables<'a>(root: &'a Table, name: &str) -> Vec<&'a Table> {
    match root.get(name) {
        Some(Item::ArrayOfTables(a)) => a.iter().collect(),
        Some(Item::Table(t)) => vec![t],
        _ => vec![],
    }
}

fn opt(t: &Table, k: &str) -> Option<String> {
    t.get(k).and_then(|v| v.as_str()).map(String::from)
}

fn req(t: &Table, k: &str) -> Result<String, String> {
    opt(t, k).ok_or_else(|| format!("rule is missing `{k}`"))
}

fn conf(t: &Table) -> f32 {
    t.get("confidence")
        .and_then(|v| v.as_float())
        .map(|f| f as f32)
        .unwrap_or(1.0)
}

fn hints(t: &Table) -> Vec<String> {
    t.get("hints")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn role(t: &Table) -> Result<Role, String> {
    let r = req(t, "role")?;
    Role::parse(&r).ok_or_else(|| format!("unknown role {r:?}"))
}

fn endpoint(t: &Table) -> Result<EndpointRule, String> {
    Ok(EndpointRule {
        id: req(t, "id")?,
        lang: req(t, "lang")?,
        role: role(t)?,
        query: req(t, "query")?,
        method: opt(t, "method"),
        prefix: opt(t, "prefix"),
        hints: hints(t),
        confidence: conf(t),
    })
}

/// `[[env.settings]]`: every annotated field of a subclass of `base_class` reads the env var of
/// the same name, after the class's `env_prefix` (pydantic-settings).
fn settings(t: &Table) -> Result<Vec<EndpointRule>, String> {
    let id = req(t, "id")?;
    let lang = req(t, "lang")?;
    let base = req(t, "base_class")?;
    let rule = |suffix: &str, query: String| EndpointRule {
        id: format!("{id}{suffix}"),
        lang: lang.clone(),
        role: Role::Read,
        query,
        method: None,
        prefix: None,
        hints: vec![base.clone()],
        confidence: conf(t),
    };
    Ok(vec![
        rule(
            "",
            format!(
                r#"(class_definition name: (identifier) @scope
                     superclasses: (argument_list [(identifier) (attribute)] @_b (#match? @_b "(^|\.){base}$"))
                     body: (block (expression_statement (assignment left: (identifier) @name type: (_)))))"#
            ),
        ),
        rule(
            ".prefix",
            format!(
                r#"(class_definition name: (identifier) @scope
                     superclasses: (argument_list [(identifier) (attribute)] @_b (#match? @_b "(^|\.){base}$"))
                     body: (block (expression_statement (assignment left: (identifier) @_m (#eq? @_m "model_config")
                       right: (call arguments: (argument_list (keyword_argument name: (identifier) @_k (#eq? @_k "env_prefix") value: (_) @prefix)))))))
                   (class_definition name: (identifier) @scope
                     superclasses: (argument_list [(identifier) (attribute)] @_b (#match? @_b "(^|\.){base}$"))
                     body: (block (class_definition name: (identifier) @_c (#eq? @_c "Config")
                       body: (block (expression_statement (assignment left: (identifier) @_e (#eq? @_e "env_prefix") right: (_) @prefix))))))"#
            ),
        ),
    ])
}

fn config(t: &Table) -> Result<ConfigRule, String> {
    let glob_src = req(t, "glob")?;
    let glob = Glob::new(&glob_src)
        .map_err(|e| format!("bad glob {glob_src:?}: {e}"))?
        .compile_matcher();
    let key_from = match opt(t, "key_from").as_deref() {
        None | Some("value") => KeyFrom::Value,
        Some("leaf") => KeyFrom::Leaf,
        Some("item_name") => KeyFrom::ItemName,
        Some(x) => return Err(format!("unknown key_from {x:?}")),
    };
    let value_pattern = match opt(t, "value_pattern") {
        Some(p) => Some(Regex::new(&p).map_err(|e| format!("bad value_pattern: {e}"))?),
        None => None,
    };
    let when = opt(t, "when").and_then(|w| {
        w.split_once('=')
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
    });
    Ok(ConfigRule {
        id: req(t, "id")?,
        glob,
        glob_src,
        path: req(t, "path")?,
        role: role(t)?,
        key_from,
        items: t.get("items").and_then(|v| v.as_bool()).unwrap_or(false),
        value_pattern,
        when,
        confidence: conf(t),
    })
}

/// JSONPath-like key path match: `a.b` exact, `*` one segment, a leading `$..` any prefix,
/// a leading `$.` is the root.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let segs: Vec<&str> = if path.is_empty() {
        vec![]
    } else {
        path.split('.').collect()
    };
    if let Some(rest) = pattern.strip_prefix("$..") {
        let pat: Vec<&str> = rest.split('.').collect();
        return segs.len() >= pat.len() && seg_match(&pat, &segs[segs.len() - pat.len()..]);
    }
    let rest = pattern.strip_prefix("$.").unwrap_or(pattern);
    let pat: Vec<&str> = rest.split('.').collect();
    pat.len() == segs.len() && seg_match(&pat, &segs)
}

fn seg_match(pat: &[&str], segs: &[&str]) -> bool {
    pat.iter().zip(segs).all(|(p, s)| *p == "*" || p == s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_builtin_and_project_rules() {
        let src = r#"
[[bus.endpoints]]
id = "bus.on"
lang = "python"
role = "consumer"
query = '''(call) @key'''

[[bus.transforms]]
fn = "scoped_topic"
result = "{0}.*?"

[[bus.config]]
id = "workflow.topic"
glob = "config/workflows/**/*.yaml"
path = "$..topic"
role = "consumer"

[[env.settings]]
id = "pydantic-settings"
lang = "python"
base_class = "BaseSettings"
"#;
        let bus = RuleSet::parse(src, Some("bus")).unwrap();
        assert_eq!(bus.endpoints.len(), 1);
        assert_eq!(bus.transforms[0].function, "scoped_topic");
        assert!(bus.config[0].glob.is_match("config/workflows/a/b.yaml"));
        let env = RuleSet::parse(src, Some("env")).unwrap();
        assert_eq!(env.endpoints.len(), 2);
        assert_eq!(env.endpoints[0].role, Role::Read);
        assert!(
            RuleSet::parse(src, Some("http"))
                .unwrap()
                .endpoints
                .is_empty()
        );
    }

    #[test]
    fn json_path_like_matching() {
        assert!(path_matches("$..topic", "pool.params.topic"));
        assert!(path_matches("$..topic", "topic"));
        assert!(!path_matches("$..topic", "topics"));
        assert!(path_matches(
            "services.*.environment.*",
            "services.api.environment.PORT"
        ));
        assert!(!path_matches(
            "services.*.environment.*",
            "services.api.environment"
        ));
        assert!(path_matches("*", "PORT"));
        assert!(path_matches("$.*", "PORT"));
        assert!(path_matches("ENV.*", "ENV.PORT"));
        assert!(path_matches("$..env.name", "spec.containers.env.name"));
    }
}
