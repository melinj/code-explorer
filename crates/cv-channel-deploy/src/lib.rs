//! P4 deploy channel pack: infra that runs code. Dockerfile `CMD`/`ENTRYPOINT`/`RUN`, compose
//! `command`, CI `run:`/`script:` and shell scripts link directly (no hub) to the module, script or
//! test path each command runs: `python main.py`, `python -m pkg.mod`, `uvicorn pkg.app:app`,
//! `pytest tests/`, `bash scripts/x.sh`, `node x.js`. A directory (a test suite) links to its
//! `conftest.py`/`__init__.py`, else its first file.

use cv_channel::{
    ChannelKey, ChannelLink, ChannelPack, EndpointFact, KeyExpr, Output, Project, Role, RuleSet,
    Target,
};

pub struct DeployPack {
    rules: RuleSet,
}

const RULES: [&str; 1] = [include_str!(
    "../../../packs/channels/deploy/infra-commands.toml"
)];

impl DeployPack {
    pub fn new() -> Self {
        let mut rules = RuleSet::default();
        for r in RULES {
            rules.extend(RuleSet::parse(r, None).expect("built-in deploy rules"));
        }
        DeployPack { rules }
    }
}

impl Default for DeployPack {
    fn default() -> Self {
        Self::new()
    }
}

/// What a command runs, as written: a file path, a module, or a test path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    Path(String),
    Module(String),
    Tests(String),
}

fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn base(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}

/// Flags of common runners that take a value.
fn takes_value(prog: &str, flag: &str) -> bool {
    if flag.contains('=') {
        return false;
    }
    match prog {
        "pytest" | "py.test" => matches!(
            flag,
            "-n" | "-k"
                | "-m"
                | "-p"
                | "-c"
                | "-o"
                | "-W"
                | "--dist"
                | "--maxfail"
                | "--rootdir"
                | "--durations"
                | "--junitxml"
                | "--cov"
                | "--cov-report"
                | "--ignore"
                | "--deselect"
                | "--timeout"
                | "--log-level"
                | "--tb"
                | "-r"
        ),
        "uvicorn" | "gunicorn" | "hypercorn" => matches!(
            flag,
            "--host"
                | "--port"
                | "--workers"
                | "-w"
                | "-b"
                | "--bind"
                | "--http"
                | "--log-level"
                | "-k"
                | "--worker-class"
                | "--app-dir"
                | "--env-file"
                | "--loop"
                | "--timeout"
        ),
        "python" => matches!(flag, "-W" | "-X" | "-Q"),
        _ => false,
    }
}

/// Splits a command line into simple commands and returns what each one runs.
pub fn runs(cmd: &str) -> Vec<Run> {
    let mut out = Vec::new();
    for simple in cmd.split(['\n', ';', '|', '&']) {
        let ws = words(simple);
        let mut i = 0;
        // Wrappers and env assignments in front of the real program.
        while i < ws.len() {
            let w = base(&ws[i]);
            let assignment = ws[i].contains('=') && !ws[i].starts_with('-');
            if assignment
                || matches!(
                    w,
                    "exec"
                        | "sudo"
                        | "time"
                        | "nohup"
                        | "env"
                        | "tini"
                        | "dumb-init"
                        | "--"
                        | "npx"
                        | "xvfb-run"
                        | "command"
                        | "then"
                        | "do"
                        | "else"
                        | "!"
                        | "("
                        | "{"
                )
            {
                i += 1;
            } else if matches!(
                w,
                "uv" | "poetry"
                    | "pipenv"
                    | "pdm"
                    | "hatch"
                    | "rye"
                    | "conda"
                    | "yarn"
                    | "pnpm"
                    | "npm"
                    | "bunx"
            ) && ws.get(i + 1).is_some_and(|n| n == "run" || n == "exec")
            {
                i += 2;
                while i < ws.len() && ws[i].starts_with("--") {
                    i += 1;
                }
            } else if w == "timeout" {
                i += 2;
            } else if i > 0 && w.starts_with('-') {
                i += 1;
            } else {
                break;
            }
        }
        let Some(prog_w) = ws.get(i) else { continue };
        let prog = base(prog_w);
        let prog = if prog.starts_with("python") {
            "python"
        } else {
            prog
        };
        let args = &ws[i + 1..];
        let positional = |args: &[String]| -> Vec<String> {
            let mut v = Vec::new();
            let mut j = 0;
            while j < args.len() {
                let a = &args[j];
                if a.starts_with('-') {
                    j += if takes_value(prog, a) { 2 } else { 1 };
                    continue;
                }
                v.push(a.clone());
                j += 1;
            }
            v
        };
        match prog {
            "python" => {
                if let Some(m) = args.iter().position(|a| a == "-m") {
                    if let Some(module) = args.get(m + 1) {
                        if module == "pytest" {
                            for p in positional(&args[m + 2..]) {
                                out.push(Run::Tests(p));
                            }
                        } else {
                            out.push(Run::Module(module.clone()));
                        }
                    }
                } else if !args.iter().any(|a| a == "-c")
                    && let Some(p) = positional(args).into_iter().next()
                {
                    out.push(Run::Path(p));
                }
            }
            "uvicorn" | "gunicorn" | "hypercorn" => {
                if let Some(app) = positional(args).into_iter().next() {
                    out.push(Run::Module(
                        app.split(':').next().unwrap_or(&app).to_string(),
                    ));
                }
            }
            "pytest" | "py.test" => {
                for p in positional(args) {
                    out.push(Run::Tests(p));
                }
            }
            "bash" | "sh" | "zsh" | "source" | "." | "node" | "tsx" | "ts-node" | "bun"
            | "deno" => {
                let pos = positional(args);
                let p = if prog == "deno" || prog == "bun" {
                    pos.into_iter().find(|a| a != "run")
                } else {
                    pos.into_iter().next()
                };
                if let Some(p) = p {
                    out.push(Run::Path(p));
                }
            }
            _ if prog_w.contains('/')
                || [".sh", ".py", ".js", ".ts", ".mjs"]
                    .iter()
                    .any(|e| prog_w.ends_with(e)) =>
            {
                out.push(Run::Path(prog_w.clone()))
            }
            _ => {}
        }
    }
    out
}

/// Resolves what a command runs to a file (and whether it is a directory's representative).
fn resolve(project: &Project, from_dir: &str, run: &Run) -> Option<(usize, bool)> {
    let join = |d: &str, p: &str| {
        let p = p.trim_start_matches("./");
        cv_lang::normalize_path(&if d.is_empty() {
            p.to_string()
        } else {
            format!("{d}/{p}")
        })
    };
    let dirs: Vec<&str> = if from_dir.is_empty() {
        vec![""]
    } else {
        vec![from_dir, ""]
    };
    match run {
        Run::Path(p) | Run::Tests(p) => {
            let p = p.split("::").next().unwrap_or(p).trim_end_matches('/');
            if p.is_empty() || p.starts_with('$') || p.starts_with('/') && !p.starts_with("/app/") {
                return None;
            }
            let p = p.strip_prefix("/app/").unwrap_or(p);
            for d in &dirs {
                let Some(full) = join(d, p) else { continue };
                if let Some(f) = project.file(&full) {
                    return Some((f, false));
                }
                let under = project.files_under(&full);
                if !under.is_empty() && matches!(run, Run::Tests(_)) {
                    for rep in ["conftest.py", "__init__.py"] {
                        if let Some(f) = project.file(&format!("{full}/{rep}")) {
                            return Some((f, true));
                        }
                    }
                    return Some((under[0], true));
                }
            }
            None
        }
        Run::Module(m) => {
            let p = m.replace('.', "/");
            for d in &dirs {
                for root in ["", "src/"] {
                    for c in [
                        format!("{root}{p}.py"),
                        format!("{root}{p}/__main__.py"),
                        format!("{root}{p}/__init__.py"),
                    ] {
                        if let Some(full) = join(d, &c)
                            && let Some(f) = project.file(&full)
                        {
                            return Some((f, false));
                        }
                    }
                }
            }
            None
        }
    }
}

impl ChannelPack for DeployPack {
    fn protocol(&self) -> &'static str {
        "deploy"
    }
    fn rules(&self) -> &RuleSet {
        &self.rules
    }
    fn rules_mut(&mut self) -> &mut RuleSet {
        &mut self.rules
    }
    fn sides(&self) -> (&'static [Role], &'static [Role]) {
        (&[Role::Deploy], &[])
    }
    fn normalize(&self, key: &KeyExpr, _role: Role) -> ChannelKey {
        ChannelKey::Known(key.render())
    }
    fn matches(&self, producer: &ChannelKey, consumer: &ChannelKey) -> bool {
        producer == consumer
    }
    fn hub_label(&self, key: &ChannelKey) -> String {
        key.text().to_string()
    }
    fn link_kind(&self, _producer: &EndpointFact, _consumer: &EndpointFact) -> &'static str {
        "deploy"
    }
    fn stub_kind(&self, _role: Role) -> &'static str {
        "deploy"
    }
    fn link_all(&self, project: &Project, endpoints: &[EndpointFact]) -> Option<Output> {
        let mut out = Output::default();
        let mut seen = std::collections::HashSet::new();
        for e in endpoints.iter().filter(|e| e.role == Role::Deploy) {
            let cmd = e.key.render();
            let dir = cv_lang::dirname(project.files[e.file].path);
            for r in runs(&cmd) {
                let Some((f, rep)) = resolve(project, dir, &r) else {
                    continue;
                };
                if f == e.file || !seen.insert((e.file, e.line, f)) {
                    continue;
                }
                let label = match &r {
                    Run::Path(p) => p.clone(),
                    Run::Module(m) => format!("-m {m}"),
                    Run::Tests(p) => format!("pytest {p}"),
                };
                out.links.push(ChannelLink {
                    protocol: "deploy",
                    kind: "deploy",
                    from_file: e.file,
                    from_line: e.line,
                    to: Target::Site { file: f, line: 1 },
                    confidence: if rep { 0.85 } else { 0.95 } * e.confidence,
                    label,
                    hub: None,
                    rule: e.rule.clone(),
                });
            }
        }
        out.notes.push(format!(
            "deploy: {} commands, {} links",
            endpoints.len(),
            out.links.len()
        ));
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_name_what_they_run() {
        assert_eq!(
            runs("/usr/bin/tini -- /usr/local/bin/uv run python main.py --port 8000"),
            vec![Run::Path("main.py".into())]
        );
        assert_eq!(
            runs("uv run pytest -n 8 --dist loadscope tests/"),
            vec![Run::Tests("tests/".into())]
        );
        assert_eq!(
            runs("cd x && uv run uvicorn src.web.app:app --host 0.0.0.0 --port 80"),
            vec![Run::Module("src.web.app".into())]
        );
        assert_eq!(
            runs("E2E_FAKE_LLM=1 uv run python tests/e2e/run_suite.py \\"),
            vec![Run::Path("tests/e2e/run_suite.py".into())]
        );
        assert_eq!(
            runs("python -m pkg.tool --x"),
            vec![Run::Module("pkg.tool".into())]
        );
        assert_eq!(
            runs("bash scripts/gen.sh"),
            vec![Run::Path("scripts/gen.sh".into())]
        );
        assert_eq!(
            runs("uv run --no-env-file python main.py"),
            vec![Run::Path("main.py".into())]
        );
        assert!(runs("echo hi && uv run ruff check src").is_empty());
    }
}
