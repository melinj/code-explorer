//! D6: configurable grade cut points, from `.cv/grades.toml` at the indexed revision (read from
//! the commit's tree like `.cv/lanes.toml`, never the working directory):
//!
//! ```toml
//! [heat]            # shares of change mass, hottest files first (DESIGN §4.7)
//! hot = 0.50        # Hot holds the first 50% of the mass
//! warm = 0.25       # Warm the next 25%
//! mild = 0.15       # Mild the next 15%; Cool is the rest with any change
//!
//! [connectedness]   # shares of the windows with dependents, most connected first (§4.8)
//! core = 0.10
//! high = 0.15
//! medium = 0.25     # Low is the rest
//! ```
//!
//! Every key is optional and defaults to the values above. A table's shares must each lie in
//! (0, 1] and sum to at most 1; a table that breaks this, has an unknown key or a wrong type is
//! ignored with a warning (its defaults kept, the other table still applies). Bad TOML or an
//! unknown table (or top-level key) keeps every default (DESIGN §4.7). A typo never stops indexing.

use cv_link::conn::ConnCuts;

pub const CONFIG_PATH: &str = ".cv/grades.toml";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradeConfig {
    /// Cumulative change-mass shares closing Hot, Warm and Mild (cv-churn `Limits::mass_cuts`).
    pub mass_cuts: [f64; 3],
    pub conn: ConnCuts,
}

impl Default for GradeConfig {
    fn default() -> Self {
        GradeConfig {
            mass_cuts: cv_churn::MASS_CUTS,
            conn: ConnCuts::default(),
        }
    }
}

/// Reads one table's three shares; `None` (with warnings) when the table is unusable.
fn shares(
    doc: &toml_edit::DocumentMut,
    table: &str,
    keys: [&str; 3],
    default: [f64; 3],
    warnings: &mut Vec<String>,
) -> Option<[f64; 3]> {
    let Some(item) = doc.get(table) else {
        return Some(default);
    };
    let Some(t) = item.as_table_like() else {
        warnings.push(format!("[{table}] is not a table"));
        return None;
    };
    let mut out = default;
    for (k, v) in t.iter() {
        let Some(i) = keys.iter().position(|x| *x == k) else {
            // §4.7: an unknown key (likely a typo) voids the whole table, like a bad value.
            warnings.push(format!("[{table}] unknown key {k}; table ignored"));
            return None;
        };
        match v.as_float().or_else(|| v.as_integer().map(|n| n as f64)) {
            Some(x) if x > 0.0 && x <= 1.0 => out[i] = x,
            _ => {
                warnings.push(format!("[{table}] {k} must be a share in (0, 1]"));
                return None;
            }
        }
    }
    if out.iter().sum::<f64>() > 1.0 + 1e-9 {
        warnings.push(format!("[{table}] shares sum to more than 1"));
        return None;
    }
    Some(out)
}

impl GradeConfig {
    /// Parses `.cv/grades.toml`; returns the config and the problems found.
    pub fn parse(text: &str) -> (GradeConfig, Vec<String>) {
        let mut warnings = Vec::new();
        let mut cfg = GradeConfig::default();
        let doc: toml_edit::DocumentMut = match text.parse() {
            Ok(d) => d,
            Err(e) => {
                warnings.push(format!("bad TOML: {e}"));
                return (cfg, warnings);
            }
        };
        // An unknown table (e.g. `[heats]`) keeps every default: what was meant is unclear.
        let unknown: Vec<String> = doc
            .iter()
            .map(|(k, _)| k)
            .filter(|k| *k != "heat" && *k != "connectedness")
            .map(|k| format!("unknown table [{k}]; using the default grades"))
            .collect();
        if !unknown.is_empty() {
            warnings.extend(unknown);
            return (cfg, warnings);
        }
        let d = cfg.mass_cuts;
        let heat_default = [d[0], d[1] - d[0], d[2] - d[1]];
        if let Some([hot, warm, mild]) = shares(
            &doc,
            "heat",
            ["hot", "warm", "mild"],
            heat_default,
            &mut warnings,
        ) {
            cfg.mass_cuts = [hot, hot + warm, hot + warm + mild];
        }
        let c = cfg.conn;
        if let Some([core, high, medium]) = shares(
            &doc,
            "connectedness",
            ["core", "high", "medium"],
            [c.core, c.high, c.medium],
            &mut warnings,
        ) {
            cfg.conn = ConnCuts { core, high, medium };
        }
        (cfg, warnings)
    }
}

/// The grade config of a revision (defaults when it has no `.cv/grades.toml`); problems are logged.
pub fn grade_config(repo: &cv_vcs::JjRepo, commit: &str) -> GradeConfig {
    let Ok(Some(blob)) = repo.blob_at(commit, CONFIG_PATH) else {
        return GradeConfig::default();
    };
    let text = match repo.read_blob(CONFIG_PATH, &blob) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            eprintln!("warning: {CONFIG_PATH}: {e}; using the default grades");
            return GradeConfig::default();
        }
    };
    let (cfg, warnings) = GradeConfig::parse(&text);
    for w in warnings {
        eprintln!(
            "warning: {CONFIG_PATH} at {}: {w}",
            &commit[..commit.len().min(12)]
        );
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_partial_tables_and_bad_input() {
        let (c, w) = GradeConfig::parse("");
        assert_eq!((c, w.len()), (GradeConfig::default(), 0));
        let (c, w) =
            GradeConfig::parse("[heat]\nhot = 0.6\n\n[connectedness]\ncore = 0.05\nmedium = 0.3\n");
        assert!(w.is_empty(), "{w:?}");
        assert!((c.mass_cuts[0] - 0.6).abs() < 1e-9);
        assert!((c.mass_cuts[1] - 0.85).abs() < 1e-9);
        assert!((c.mass_cuts[2] - 1.0).abs() < 1e-9);
        assert_eq!(
            c.conn,
            ConnCuts {
                core: 0.05,
                high: 0.15,
                medium: 0.3
            }
        );
        // Shares over 1, a bad value, an unknown key and table, and broken TOML.
        let (c, w) =
            GradeConfig::parse("[heat]\nhot = 0.9\nwarm = 0.2\n[connectedness]\ncore = \"x\"\n");
        assert_eq!(c, GradeConfig::default());
        assert_eq!(w.len(), 2);
        // An unknown key voids its own table only: the other table still applies.
        let (c, w) =
            GradeConfig::parse("[heat]\nhot = 0.6\n[connectedness]\nhigh = 0.2\ncolor = 1\n");
        assert_eq!(c.conn, ConnCuts::default());
        assert!((c.mass_cuts[0] - 0.6).abs() < 1e-9, "{c:?}");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("unknown key color"), "{w:?}");
        // An unknown table keeps every default, even next to valid [heat] and [connectedness]
        // tables that would otherwise apply.
        for text in [
            "[grade]\n[heat]\nhot = 0.6\n[connectedness]\ncore = 0.05\n",
            "[heat]\nhot = 0.6\n[connectedness]\ncore = 0.05\n[heats]\nhot = 0.7\n",
            "hot = 0.6\n[heat]\nhot = 0.6\n",
        ] {
            let (c, w) = GradeConfig::parse(text);
            assert_eq!(c, GradeConfig::default(), "{text}");
            assert_eq!(w.len(), 1, "{text}: {w:?}");
            assert!(w[0].contains("unknown table"), "{w:?}");
        }
        // The same valid tables without the unknown one do apply (so the check above is not vacuous).
        let (c, w) = GradeConfig::parse("[heat]\nhot = 0.6\n[connectedness]\ncore = 0.05\n");
        assert!(w.is_empty() && c != GradeConfig::default() && (c.conn.core - 0.05).abs() < 1e-9);
        let (c, w) = GradeConfig::parse("[heat");
        assert_eq!((c, w.len()), (GradeConfig::default(), 1));
    }
}
