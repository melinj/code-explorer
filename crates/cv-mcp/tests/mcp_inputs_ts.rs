//! D3 for MCP: the tool input schemas the agent sees (schemars, slimmed by `all_tools`) are rendered
//! as TypeScript into `docs/contracts/generated/mcp/` (and the web copy), where `check.ts` compares
//! each one with `McpTools[tool]["in"]` of the hand-written spec (docs/contracts/mcp.ts) at compile
//! time (`tsc --noEmit` in web/). This test fails when the committed files are stale; with
//! `CV_GEN_CONTRACTS=1` (scripts/gen-contracts.sh) it writes them. It also checks the profiles
//! against `MCP_PROFILES`.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const OUT_DIRS: [&str; 2] = [
    "docs/contracts/generated/mcp",
    "web/src/contracts/generated/mcp",
];

const HEADER: &str = "// Generated from the cv-mcp tool input schemas by scripts/gen-contracts.sh. Do not edit.\n// The hand-written spec is docs/contracts/mcp.ts (McpTools[*].in); check.ts compares the two.\n";

/// Tools whose input Rust accepts more loosely than the spec states (spec input assignable to the
/// generated one, not the reverse), with the reason.
const IN_ONLY: &[(&str, &str)] = &[(
    "set_view",
    "range options: Rust fills every RangeOptions field the spec requires with its default",
)];

/// A JSON Schema (as slimmed for the agent) as a TypeScript type.
fn ts(s: &Value) -> String {
    let Value::Object(o) = s else {
        // `true`: anything.
        return "JsonValue".into();
    };
    if let Some(c) = o.get("const") {
        return c.to_string();
    }
    if let Some(Value::Array(e)) = o.get("enum") {
        return e
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(" | ");
    }
    for k in ["oneOf", "anyOf"] {
        if let Some(Value::Array(alts)) = o.get(k) {
            let mut base = o.clone();
            base.remove(k);
            let alts: Vec<String> = alts
                .iter()
                .map(|a| {
                    // Alternatives inherit the shared keywords (e.g. `type: object`).
                    let mut m = base.clone();
                    if let Value::Object(a) = a {
                        for (k, v) in a {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    format!("({})", ts(&Value::Object(m)))
                })
                .collect();
            return alts.join(" | ");
        }
    }
    let ty = match o.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(|t| t.as_str().map(String::from))
            .collect(),
        _ if o.contains_key("properties") => vec!["object".into()],
        _ => return "JsonValue".into(),
    };
    ty.iter()
        .map(|t| match t.as_str() {
            "string" => "string".into(),
            "integer" | "number" => "number".into(),
            "boolean" => "boolean".into(),
            "null" => "null".into(),
            "array" => array(o),
            "object" => object(o),
            other => panic!("unexpected schema type {other}"),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn array(o: &serde_json::Map<String, Value>) -> String {
    if let Some(Value::Array(items)) = o.get("prefixItems") {
        return format!("[{}]", items.iter().map(ts).collect::<Vec<_>>().join(", "));
    }
    let item = o.get("items").map(ts).unwrap_or_else(|| "JsonValue".into());
    match (o.get("minItems"), o.get("maxItems")) {
        (Some(a), Some(b)) if a == b && a.as_u64().is_some_and(|n| (1..=4).contains(&n)) => {
            let n = a.as_u64().unwrap() as usize;
            format!("[{}]", vec![item; n].join(", "))
        }
        _ => format!("Array<{item}>"),
    }
}

fn object(o: &serde_json::Map<String, Value>) -> String {
    let props = o.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = o
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match props {
        None => "Record<string, never>".into(),
        Some(p) if p.is_empty() => "Record<string, never>".into(),
        Some(p) => {
            let fields: Vec<String> = p
                .iter()
                .map(|(name, s)| {
                    let opt = if required.contains(&name.as_str()) {
                        ""
                    } else {
                        "?"
                    };
                    format!("{name}{opt}: {}", ts(s))
                })
                .collect();
            format!("{{ {} }}", fields.join("; "))
        }
    }
}

fn generate() -> Vec<(String, String)> {
    let tools = cv_mcp::server::all_tools();
    let mut body = String::from("/** Each MCP tool's input as its JSON Schema states it. */\n");
    body.push_str("export interface McpInputs {\n");
    let mut check = String::from(HEADER);
    check.push_str(
        "// Exact: mutually assignable (U<T> widens brands and literal unions, as in ../check.ts) and the\n// same top-level field names. In: the spec's input is accepted by Rust.\n\n",
    );
    check.push_str("import type { McpInputs } from \"./inputs\";\n");
    check.push_str("import type { McpTools } from \"../../mcp\";\n");
    check.push_str("import type { AllKeys, Assert, Exact, Extends, U } from \"../check\";\n\n");
    check.push_str("export type _ToolNames = Assert<Exact<keyof McpInputs, keyof McpTools>>;\n");
    for t in &tools {
        let name = t.name.to_string();
        let schema = Value::Object((*t.input_schema).clone());
        body.push_str(&format!("  {name}: {};\n", ts(&schema)));
        let (g, s) = (
            format!("McpInputs[\"{name}\"]"),
            format!("McpTools[\"{name}\"][\"in\"]"),
        );
        let assertion = if IN_ONLY.iter().any(|(n, _)| *n == name) {
            format!("Extends<U<{s}>, U<{g}>>")
        } else {
            format!("Exact<U<{g}>, U<{s}>>")
        };
        check.push_str(&format!("export type _{name} = Assert<{assertion}>;\n"));
        check.push_str(&format!(
            "export type _{name}Keys = Assert<Exact<AllKeys<{g}>, AllKeys<{s}>>>;\n"
        ));
    }
    body.push_str("}\n");
    let mut inputs = String::from(HEADER);
    if body.contains("JsonValue") {
        inputs.push_str("import type { JsonValue } from \"../json\";\n");
    }
    inputs.push('\n');
    inputs.push_str(&body);
    let loose: Vec<String> = IN_ONLY
        .iter()
        .map(|(n, why)| format!("// {n} (in): {why}\n"))
        .collect();
    if !loose.is_empty() {
        check.push('\n');
        check.push_str(&loose.concat());
    }
    vec![("inputs.ts".into(), inputs), ("check.ts".into(), check)]
}

#[test]
fn mcp_inputs_ts_is_current() {
    let files = generate();
    let write = std::env::var("CV_GEN_CONTRACTS").is_ok_and(|v| v == "1");
    let mut stale = Vec::new();
    for dir in OUT_DIRS {
        let dir = repo_root().join(dir);
        if write {
            std::fs::create_dir_all(&dir).unwrap();
        }
        for (name, text) in &files {
            let path = dir.join(name);
            if write {
                std::fs::write(&path, text).unwrap();
            } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                stale.push(path.display().to_string());
            }
        }
    }
    assert!(
        stale.is_empty(),
        "generated MCP input types are stale; run scripts/gen-contracts.sh and commit the result:\n  {}",
        stale.join("\n  ")
    );
}

/// `MCP_PROFILES` in mcp.ts lists the same tools, in the same order, as the Rust profiles.
#[test]
fn profiles_match_mcp_ts() {
    use cv_mcp::server::{EDITOR, FULL, READER, REVIEWER};
    let spec = std::fs::read_to_string(repo_root().join("docs/contracts/mcp.ts")).unwrap();
    let start = spec
        .find("export const MCP_PROFILES")
        .expect("MCP_PROFILES");
    let block = &spec[start..];
    let block = &block[..block.find("} as const").expect("end of MCP_PROFILES")];
    for (name, rust) in [
        ("reader", READER),
        ("reviewer", REVIEWER),
        ("editor", EDITOR),
        ("full", FULL),
    ] {
        let line = block
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{name}:")))
            .unwrap_or_else(|| panic!("profile {name} missing in mcp.ts"));
        let listed: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
        assert_eq!(listed, rust, "profile {name}: mcp.ts vs cv-mcp");
    }
    // Every tool in the full profile has a schema, and nothing else does.
    let mut names: Vec<String> = cv_mcp::server::all_tools()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    let mut full: Vec<String> = FULL.iter().map(|s| s.to_string()).collect();
    full.sort();
    assert_eq!(names, full);
}
