//! AnnotatedCode (mcp.ts): numbered lines with ports and threads inline, and ```suggestion parsing.

use serde_json::{Value, json};

/// One inline note after a line: a port label or a hub reference.
pub struct LineNote {
    pub line: u32,
    pub text: String,
}

/// Format `lines` (numbered from `start`) with port notes at line ends and threads under their last
/// anchored line:
/// ```text
///   41│     envelope = EventEnvelope(topic=ENTITY_MOVED, payload=moved)
///   42│     bus.emit(envelope)                  ⇢ hub bus:world.entity.moved
///     ┆ [t_12 open · assigned agent] "Cap retries at 5" (suggestion)
/// ```
pub fn format(start: u32, lines: &[String], notes: &[LineNote], threads: &[Value]) -> String {
    let end = start + lines.len().saturating_sub(1) as u32;
    let width = end.to_string().len().max(4);
    let mut out = String::new();
    for (i, text) in lines.iter().enumerate() {
        let n = start + i as u32;
        let mut row = format!("{n:>width$}│ {text}");
        let here: Vec<&str> = notes
            .iter()
            .filter(|p| p.line == n)
            .map(|p| p.text.as_str())
            .collect();
        if !here.is_empty() {
            let pad = 48usize.saturating_sub(row.chars().count()).max(2);
            row.push_str(&" ".repeat(pad));
            row.push_str(&here.join(" · "));
        }
        out.push_str(&row);
        out.push('\n');
        for t in threads {
            let a = &t["anchor"];
            let t_end = a["end"].as_u64().unwrap_or(0) as u32;
            let t_start = a["start"].as_u64().unwrap_or(0) as u32;
            // Under the thread's last line, or under the first shown line when it ends above.
            let anchor_line = t_end.clamp(start, end);
            if anchor_line != n || t_start > end {
                continue;
            }
            out.push_str(&" ".repeat(width));
            out.push_str("┆ ");
            out.push_str(&thread_line(t));
            out.push('\n');
        }
    }
    out
}

/// `[t_12 open · assigned agent · lines 40–42] "Cap retries at 5" (suggestion, 2 comments)`
pub fn thread_line(t: &Value) -> String {
    let a = &t["anchor"];
    let mut tags = vec![
        t["id"].as_str().unwrap_or("?").to_string(),
        t["status"].as_str().unwrap_or("?").to_string(),
    ];
    if let Some(who) = t["assignee"].as_str() {
        tags.push(format!("assigned {who}"));
    }
    tags.push(format!(
        "lines {}–{}",
        a["start"].as_u64().unwrap_or(0),
        a["end"].as_u64().unwrap_or(0)
    ));
    let first = t["comments"][0]["body"].as_str().unwrap_or("");
    let first_line = first.lines().next().unwrap_or("");
    let n = t["comments"].as_array().map(|c| c.len()).unwrap_or(0);
    format!(
        "[{}] \"{}\" ({}, {} comment{})",
        tags.join(" · "),
        first_line.chars().take(100).collect::<String>(),
        t["kind"].as_str().unwrap_or("comment"),
        n,
        if n == 1 { "" } else { "s" }
    )
}

/// The replacement in the newest ```suggestion block of a thread, if any.
pub fn suggestion_text(thread: &Value) -> Option<String> {
    let comments = thread["comments"].as_array()?;
    comments
        .iter()
        .rev()
        .find_map(|c| parse_suggestion(c["body"].as_str()?))
}

/// The body of the first ```suggestion fenced block in a markdown comment.
pub fn parse_suggestion(body: &str) -> Option<String> {
    let mut lines = body.lines();
    loop {
        let l = lines.next()?;
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("```")
            && rest.trim() == "suggestion"
        {
            let fence_indent = l.len() - t.len();
            let mut out: Vec<&str> = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    return Some(out.join("\n"));
                }
                // Strip the fence's own indentation (markdown list nesting), not the code's.
                let strip = l
                    .chars()
                    .take(fence_indent)
                    .take_while(|c| *c == ' ')
                    .count();
                out.push(&l[strip..]);
            }
            return Some(out.join("\n"));
        }
    }
}

/// The LinePatch a thread's suggestion makes: replace the anchored lines.
pub fn suggestion_patch(thread: &Value) -> Option<Value> {
    let text = suggestion_text(thread)?;
    let a = &thread["anchor"];
    Some(json!({ "start": a["start"], "end": a["end"], "text": text }))
}

/// ThreadSummary from a Thread.
pub fn summary(t: &Value) -> Value {
    let a = &t["anchor"];
    let mut s = json!({
        "id": t["id"], "status": t["status"], "kind": t["kind"], "path": a["path"],
        "start": a["start"], "end": a["end"],
        "comments": t["comments"].as_array().map(|c| c.len()).unwrap_or(0),
        "preview": t["comments"][0]["body"].as_str().unwrap_or("").lines().next().unwrap_or(""),
    });
    if let Some(who) = t["assignee"].as_str() {
        s["assignee"] = json!(who);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_suggestion_blocks() {
        let body = "Cap it.\n```suggestion\n    retries = min(retries, 5)\n```\nthanks";
        assert_eq!(
            parse_suggestion(body).as_deref(),
            Some("    retries = min(retries, 5)")
        );
        assert_eq!(parse_suggestion("no block\n```python\nx\n```"), None);
        let t = json!({ "anchor": { "start": 3, "end": 4 }, "comments": [
            { "body": "```suggestion\nold\n```" }, { "body": "```suggestion\nnew\nlines\n```" }
        ]});
        assert_eq!(
            suggestion_patch(&t),
            Some(json!({ "start": 3, "end": 4, "text": "new\nlines" }))
        );
    }

    #[test]
    fn formats_ports_and_threads() {
        let lines = vec!["a = 1".to_string(), "bus.emit(a)".to_string()];
        let notes = [LineNote {
            line: 11,
            text: "⇢ hub world.moved".into(),
        }];
        let t = json!({ "id": "t1", "status": "open", "assignee": "agent", "kind": "task",
            "anchor": { "start": 10, "end": 10 }, "comments": [{ "body": "Rename a" }] });
        let s = format(10, &lines, &notes, &[t]);
        let rows: Vec<&str> = s.lines().collect();
        assert_eq!(rows[0], "  10│ a = 1");
        assert_eq!(
            rows[1],
            "    ┆ [t1 · open · assigned agent · lines 10–10] \"Rename a\" (task, 1 comment)"
        );
        assert!(rows[2].starts_with("  11│ bus.emit(a)") && rows[2].ends_with("⇢ hub world.moved"));
    }
}
