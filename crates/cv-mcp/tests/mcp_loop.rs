//! B9 acceptance on the in-memory backend, through a real MCP client over a duplex pipe:
//! wait_for_events → list_threads {withCode} → apply_suggestion {reply} (amend and answer in one
//! call), edit_code {thread, reply}, and the UI's event stream then carries thread.upserted,
//! edit.updated and agent.attention. No bind_workspace: the served workspace is the default.

use std::sync::Arc;

use cv_mcp::fake::{CHANGE, FakeBackend};
use cv_mcp::{CvMcp, McpConfig, Profile};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig,
    GetPromptRequestParams, Implementation, ReadResourceRequestParams,
};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ClientConfig>;

async fn connect(backend: Arc<FakeBackend>, profile: Profile) -> Client {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let server = CvMcp::new(
        backend,
        McpConfig {
            profile,
            ..Default::default()
        },
    );
    tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server starts");
        let _ = running.waiting().await;
    });
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("claude-code", "1.0"),
    )
    .serve(client_io)
    .await
    .expect("client connects")
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Call a tool; returns (is_error, first block as JSON or Null, all text).
async fn call(c: &Client, name: &str, args: Value) -> (bool, Value, String) {
    let r = c
        .call_tool(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().cloned().unwrap_or_default()),
        )
        .await
        .expect("call_tool");
    let first = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .and_then(|t| serde_json::from_str(&t.text).ok())
        .unwrap_or(Value::Null);
    (r.is_error == Some(true), first, text(&r))
}

async fn ok(c: &Client, name: &str, args: Value) -> (Value, String) {
    let (is_err, v, t) = call(c, name, args).await;
    assert!(!is_err, "{name} failed: {t}");
    (v, t)
}

#[tokio::test]
async fn address_review_loop() {
    let fake = Arc::new(FakeBackend::sample());
    let c = connect(fake.clone(), Profile::Editor).await;

    // Start waiting from "now".
    let (first, _) = ok(
        &c,
        "wait_for_events",
        json!({ "timeoutS": 1, "kinds": ["thread.upserted"] }),
    )
    .await;
    assert_eq!(first["events"], json!([]));
    let cursor = first["cursor"].as_str().unwrap().to_string();

    // The human asks for a change, with a suggestion, assigned to the agent.
    let human = fake.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        human.human_thread(
            "src/bus.py",
            6,
            6,
            "Unbounded retries.\n```suggestion\n        retries = min(retries, 5)\n```",
            "suggestion",
            Some("agent"),
        );
    });
    let (evs, _) = ok(
        &c,
        "wait_for_events",
        json!({ "cursor": cursor, "kinds": ["thread.upserted"], "timeoutS": 10 }),
    )
    .await;
    // Events carry a ThreadSummary, not the whole thread.
    let t = &evs["events"][0]["thread"];
    let id = t["id"].as_str().unwrap().to_string();
    assert_eq!(t["assignee"], "agent");
    assert_eq!(t["preview"], "Unbounded retries.");
    assert!(t.get("anchor").is_none(), "{t}");

    let (open, _) = ok(
        &c,
        "list_threads",
        json!({ "assignee": "agent", "status": ["open"] }),
    )
    .await;
    assert_eq!(open.as_array().unwrap().len(), 1);
    assert_eq!(open[0]["preview"], "Unbounded retries.");

    // withCode: the thread, its comments and the code around it in one call.
    let (th, all) = ok(
        &c,
        "list_threads",
        json!({ "assignee": "agent", "status": ["open"], "withCode": true }),
    )
    .await;
    assert_eq!(th["id"], id.as_str());
    assert_eq!(
        th["suggestion"],
        json!({ "start": 6, "end": 6, "text": "        retries = min(retries, 5)" })
    );
    assert_eq!(th["comments"][0]["by"], "you");
    assert!(th.get("fingerprint").is_none() && th["comments"][0].get("id").is_none());
    assert!(all.contains("   6│         retries = 0"), "{all}");
    // The thread's own line is not repeated under the code.
    assert!(!all.contains(&format!("[{id} ")), "{all}");
    let blob_before = th["blob"].as_str().unwrap().to_string();

    let (g, all) = ok(&c, "get_thread", json!({ "thread": id, "contextLines": 2 })).await;
    assert_eq!(g["blob"], blob_before.as_str());
    assert_eq!(g["lines"], json!([4, 8]));
    assert!(all.contains("   6│         retries = 0"), "{all}");

    // Preview is exact and writes nothing.
    let (prev, _) = ok(
        &c,
        "apply_suggestion",
        json!({ "thread": id, "dryRun": true, "reply": "not posted on a preview" }),
    )
    .await;
    assert_eq!(prev["status"], "previewed");
    assert!(prev.get("thread").is_none());
    assert!(
        fake.file_text("src/bus.py")
            .unwrap()
            .contains("retries = 0")
    );

    // Apply and answer in one call.
    let (r, _) = ok(
        &c,
        "apply_suggestion",
        json!({ "thread": id, "reply": "Capped at 5." }),
    )
    .await;
    assert_eq!(r["status"], "applied");
    assert!(
        r.get("actor").is_none() && r.get("opBefore").is_none(),
        "{r}"
    );
    assert_eq!(
        r["thread"],
        json!({ "id": id, "status": "addressed", "comments": 2 })
    );
    let text_after = fake.file_text("src/bus.py").unwrap();
    assert!(text_after.contains("        retries = min(retries, 5)"));
    assert!(!text_after.contains("retries = 0"));
    let thread = fake.thread(&id).unwrap();
    let last = thread["comments"].as_array().unwrap().last().unwrap();
    assert_eq!(
        last["author"],
        json!({ "kind": "agent", "name": "claude", "client": "claude-code", "workspace": "default" })
    );
    assert_eq!(thread["producedEdits"].as_array().unwrap().len(), 1);

    // A stale base is rejected: the blob moved on with the amend.
    let (is_err, _, msg) = call(
        &c,
        "edit_code",
        json!({ "rev": "kxwqmnpo", "path": "src/bus.py", "baseBlob": blob_before,
                "patches": [{ "start": 1, "end": 1, "text": "class EventBus:" }] }),
    )
    .await;
    assert!(is_err && msg.contains("stale-base"), "{msg}");

    // What the UI saw.
    let log = fake.events_log();
    let types: Vec<&str> = log.iter().filter_map(|e| e["type"].as_str()).collect();
    assert!(types.contains(&"thread.upserted"));
    assert!(types.contains(&"edit.updated") && types.contains(&"op.changed"));
    let att: Vec<&Value> = log
        .iter()
        .filter(|e| e["type"] == "agent.attention")
        .collect();
    let tools: Vec<&str> = att.iter().filter_map(|e| e["tool"].as_str()).collect();
    for tool in ["list_threads", "get_thread", "apply_suggestion"] {
        assert!(tools.contains(&tool), "no attention for {tool}: {tools:?}");
    }
    let read = att.iter().find(|e| e["tool"] == "get_thread").unwrap();
    assert_eq!(read["agent"], "claude");
    // Attributed to the served workspace without a bind.
    assert_eq!(read["workspace"], "default");
    assert_eq!(read["windows"], json!(["w:src/bus.py"]));
    assert!(
        log.iter()
            .any(|e| e["type"] == "thread.upserted" && e["thread"]["status"] == "addressed")
    );
}

#[tokio::test]
async fn edit_code_answers_a_thread_in_one_call() {
    let fake = Arc::new(FakeBackend::sample());
    let c = connect(fake.clone(), Profile::Editor).await;
    let t = fake.human_thread(
        "src/world.py",
        7,
        7,
        "Move by the entity's speed.",
        "task",
        Some("agent"),
    );
    let id = t["id"].as_str().unwrap();
    let (meta, code) = ok(
        &c,
        "list_threads",
        json!({ "assignee": "agent", "withCode": true }),
    )
    .await;
    assert_eq!(meta["path"], "src/world.py");
    assert!(code.contains("   7│     entity.x += 1"), "{code}");

    // rev, path and baseBlob come from the thread and the code just shown.
    let (r, _) = ok(
        &c,
        "edit_code",
        json!({ "thread": id, "patches": [{ "start": 7, "end": 7, "text": "    entity.x += entity.speed" }],
                "reply": "Moves by entity.speed now." }),
    )
    .await;
    assert_eq!(r["status"], "applied");
    assert_eq!(r["thread"]["status"], "addressed");
    assert!(
        fake.file_text("src/world.py")
            .unwrap()
            .contains("entity.x += entity.speed")
    );
    // The file moved on since the agent last saw it: a second blind edit is stale.
    let (is_err, _, msg) = call(
        &c,
        "edit_code",
        json!({ "thread": id, "patches": [{ "start": 8, "end": 8, "text": "    pass" }] }),
    )
    .await;
    assert!(is_err && msg.contains("stale-base"), "{msg}");
    // After reading it again, the edit goes through; setStatus without reply only sets it.
    ok(&c, "get_thread", json!({ "thread": id })).await;
    let (r, _) = ok(
        &c,
        "edit_code",
        json!({ "thread": id, "patches": [{ "start": 8, "end": 8, "text": "    bus.emit(\"world.entity.moved\", entity)  # moved" }],
                "setStatus": "in-progress" }),
    )
    .await;
    assert_eq!(
        r["thread"],
        json!({ "id": id, "status": "in-progress", "comments": 2 })
    );

    // reply: body only, status only, neither.
    let (a, _) = ok(
        &c,
        "reply",
        json!({ "thread": id, "setStatus": "addressed" }),
    )
    .await;
    assert_eq!(a, json!({ "id": id, "status": "addressed", "comments": 2 }));
    let (a, _) = ok(&c, "reply", json!({ "thread": id, "body": "done" })).await;
    assert_eq!(a["comments"], 3);
    let (is_err, _, msg) = call(&c, "reply", json!({ "thread": id })).await;
    assert!(is_err && msg.contains("body"), "{msg}");
    // Typos in closed vocabularies are argument errors, not empty results.
    let (is_err, _, msg) = call(&c, "list_threads", json!({ "status": ["opne"] })).await;
    assert!(is_err && msg.contains("unknown variant"), "{msg}");
}

#[tokio::test]
async fn binding_another_workspace() {
    let fake = Arc::new(FakeBackend::sample());
    let c = connect(fake.clone(), Profile::Full).await;
    let (ws, _) = ok(&c, "list_workspaces", json!({})).await;
    let names: Vec<&str> = ws
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["name"].as_str())
        .collect();
    assert_eq!(names, ["default", "gym"]);
    assert!(ws[0].get("root").is_none() && ws[0]["lastOp"].get("op").is_none());
    assert_eq!(ws[0]["boundToThisSession"], true);
    let (b, _) = ok(&c, "bind_workspace", json!({ "workspace": "gym" })).await;
    assert_eq!(b, json!({ "bound": "gym", "stack": "trunk()..gym@" }));
    let (ws, _) = ok(&c, "list_workspaces", json!({})).await;
    let gym = ws
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "gym")
        .unwrap();
    assert_eq!(gym["agent"]["name"], "claude");
    assert_eq!(gym["boundToThisSession"], true);
    ok(&c, "read_code", json!({ "path": "src/bus.py" })).await;
    let log = fake.events_log();
    let read = log
        .iter()
        .find(|e| e["type"] == "agent.attention" && e["tool"] == "read_code")
        .unwrap();
    assert_eq!(read["workspace"], "gym");
}

#[tokio::test]
async fn edit_without_suggestion_and_reading_tools() {
    let fake = Arc::new(FakeBackend::sample());
    let c = connect(fake.clone(), Profile::Full).await;
    let (code, all) = ok(&c, "read_code", json!({ "path": "src/world.py" })).await;
    assert_eq!(code["start"], 1);
    // Ports name their one destination.
    assert!(
        all.contains("→ Bus.emit (src/bus.py:5)"),
        "ports inline: {all}"
    );
    let blob = code["blob"].as_str().unwrap();

    let (w, all) = ok(&c, "read_window", json!({ "window": "w:src/bus.py" })).await;
    assert_eq!(w["path"], "src/bus.py");
    assert!(
        all.contains("← Bus.emit") || all.contains("→ Bus.emit"),
        "{all}"
    );

    let (hits, _) = ok(&c, "search", json!({ "query": "world" })).await;
    assert!(!hits.as_array().unwrap().is_empty());
    assert!(
        hits[0]["window"].is_string() && hits[0].get("target").is_none(),
        "{hits}"
    );

    let (links, _) = ok(
        &c,
        "get_links",
        json!({ "target": "src/world.py:8", "dir": "out" }),
    )
    .await;
    assert_eq!(links["links"][0]["id"], "l1");
    assert_eq!(links["links"][0]["to"], "src/bus.py:5");
    // Another line of the same window has no links of its own.
    let (links, _) = ok(
        &c,
        "get_links",
        json!({ "target": "src/world.py:3", "dir": "out" }),
    )
    .await;
    assert_eq!(links["links"], json!([]), "{links}");
    let (hub, _) = ok(&c, "get_hub", json!({ "key": "world.entity.moved" })).await;
    assert_eq!(hub["protocol"], "bus");
    let (m, _) = ok(&c, "get_metrics", json!({ "target": "src/world.py" })).await;
    assert_eq!(m["coChanged"][0]["path"], "tests/test_world.py");
    assert!(m.get("timeline").is_none());
    assert_eq!(m["structure"]["window"]["external"], 1);
    assert_eq!(m["structure"]["file"]["tension"], 1.0);
    assert_eq!(m["structure"]["folder"]["spread"], 1.0);
    let (hs, _) = ok(&c, "get_hotspots", json!({ "lane": "code", "limit": 1 })).await;
    assert_eq!(hs.as_array().unwrap().len(), 1);
    let (st, _) = ok(&c, "get_structure", json!({ "limit": 5 })).await;
    assert_eq!(st["summary"]["divergence"], 0.4);
    assert_eq!(
        st["leaky"].as_array().unwrap().len(),
        1,
        "lib has too few boundary links"
    );
    assert_eq!(st["leaky"][0]["out"], 3);
    assert_eq!(st["splitOut"][0]["path"], "src/world.py");
    assert_eq!(st["move"][0]["to"], "lib");
    assert_eq!(st["tense"][0]["hotFiles"][0], "src/world.py");
    assert_eq!(st["gateways"][0]["key"], "src/util.py#file");
    assert_eq!(st["leaky"][0]["spread"], 1.0);
    assert_eq!(st["leaky"][0]["inside"], 0.4);
    let (vs, _) = ok(&c, "get_view_state", json!({})).await;
    assert_eq!(vs, json!({ "view": null }));

    // propose_edit → apply_edit.
    let (p, _) = ok(
        &c,
        "propose_edit",
        json!({ "rev": "kxwqmnpo", "path": "src/world.py", "baseBlob": blob,
                "patches": [{ "start": 7, "end": 7, "text": "    entity.x += entity.speed" }] }),
    )
    .await;
    assert_eq!(p["status"], "previewed");
    assert!(
        fake.file_text("src/world.py")
            .unwrap()
            .contains("entity.x += 1")
    );
    let (a, _) = ok(&c, "apply_edit", json!({ "edit": p["id"] })).await;
    assert_eq!(a["status"], "applied");
    assert!(
        fake.file_text("src/world.py")
            .unwrap()
            .contains("entity.x += entity.speed")
    );

    // A thread without a suggestion cannot be applied blindly.
    let t = fake.human_thread("src/world.py", 3, 3, "rename bus", "task", Some("agent"));
    let (is_err, _, msg) = call(&c, "apply_suggestion", json!({ "thread": t["id"] })).await;
    assert!(is_err && msg.contains("edit_code"), "{msg}");

    // Conflicts.
    fake.add_conflict(
        "tests/test_world.py",
        5,
        5,
        "    move(e)  # ours",
        "    move(e, 1)",
    );
    let (cs, _) = ok(&c, "list_conflicts", json!({})).await;
    assert_eq!(cs[0]["path"], "tests/test_world.py");
    let (d, _) = ok(
        &c,
        "get_conflict",
        json!({ "rev": "kxwq", "path": "tests/test_world.py" }),
    )
    .await;
    assert_eq!(d["regions"][0]["sides"][1]["text"], "    move(e, 1)");
    let resolved =
        "from world import move\n\n\ndef test_move(e):\n    move(e, 1)\n    assert e.x == 1";
    let (r, _) = ok(
        &c,
        "resolve_conflict",
        json!({ "rev": "kxwqmnpo", "path": "tests/test_world.py", "content": resolved }),
    )
    .await;
    assert_eq!(r["status"], "applied");
    assert_eq!(fake.file_text("tests/test_world.py").unwrap(), resolved);
    let (cs, _) = ok(&c, "list_conflicts", json!({})).await;
    assert_eq!(cs, json!([]));

    // Agent findings and focus.
    let c = connect(fake.clone(), Profile::Reviewer).await;
    let (t, _) = ok(
        &c,
        "create_thread",
        json!({ "anchor": { "path": "src/bus.py", "start": 7, "end": 11 }, "body": "Loop never retries.", "severity": "major" }),
    )
    .await;
    assert_eq!(t["kind"], "finding");
    assert_eq!(t["createdBy"], "agent");
    let (f, _) = ok(
        &c,
        "focus",
        json!({ "target": "w:src/bus.py", "lines": [7, 11], "note": "here" }),
    )
    .await;
    assert_eq!(f["delivered"], "suggested");
    assert!(
        fake.events_log()
            .iter()
            .any(|e| e["type"] == "focus.request" && e["target"]["window"] == "w:src/bus.py")
    );
}

#[tokio::test]
async fn profiles_gate_tools() {
    let fake = Arc::new(FakeBackend::sample());
    for (profile, n) in [
        (Profile::Reader, 16),
        (Profile::Reviewer, 25),
        (Profile::Editor, 15),
        (Profile::Full, 34),
    ] {
        let c = connect(fake.clone(), profile).await;
        let tools = c.list_all_tools().await.unwrap();
        assert_eq!(tools.len(), n, "{profile:?}");
        for t in &tools {
            assert_eq!(t.input_schema["type"], "object", "{} schema", t.name);
        }
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names.contains(&"apply_suggestion"),
            matches!(profile, Profile::Editor | Profile::Full)
        );
        assert_eq!(names.contains(&"create_thread"), profile != Profile::Reader);
        assert_eq!(names.contains(&"set_view"), profile != Profile::Reader);
        assert_eq!(
            names.contains(&"bind_workspace"),
            profile != Profile::Editor
        );
        assert!(names.contains(&"wait_for_events"));
    }
    let c = connect(fake.clone(), Profile::Reader).await;
    let (is_err, _, msg) = call(&c, "reply", json!({ "thread": "t1", "body": "x" })).await;
    assert!(is_err && msg.contains("reader profile"), "{msg}");
    // Schemas come from schemars: get_thread has a required thread and an optional contextLines.
    let c = connect(fake, Profile::Editor).await;
    let tools = c.list_all_tools().await.unwrap();
    let gt = tools.iter().find(|t| t.name == "get_thread").unwrap();
    assert_eq!(gt.input_schema["required"], json!(["thread"]));
    assert_eq!(
        gt.input_schema["properties"]["contextLines"]["type"],
        "integer"
    );
    let lt = tools.iter().find(|t| t.name == "list_threads").unwrap();
    assert_eq!(
        lt.input_schema["properties"]["assignee"]["enum"],
        json!(["agent", "human"])
    );
}
#[tokio::test]
async fn resources_and_prompts() {
    let fake = Arc::new(FakeBackend::sample());
    fake.human_thread("src/bus.py", 1, 1, "Name?", "question", None);
    let c = connect(fake, Profile::Reviewer).await;
    let rs = c.list_all_resources().await.unwrap();
    let uris: Vec<&str> = rs.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(
        uris,
        [
            "cv://view/state",
            "cv://threads?status=open",
            "cv://conflicts"
        ]
    );
    let r = c
        .read_resource(ReadResourceRequestParams::new("cv://threads?status=open"))
        .await
        .unwrap();
    let body = serde_json::to_value(&r.contents[0]).unwrap();
    let threads: Value = serde_json::from_str(body["text"].as_str().unwrap()).unwrap();
    assert_eq!(threads[0]["preview"], "Name?");
    let ps = c.list_all_prompts().await.unwrap();
    assert_eq!(ps.len(), 3);
    let p = c
        .get_prompt(
            GetPromptRequestParams::new("review_range").with_arguments(
                json!({ "revset": "trunk()..gym@" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let msg = serde_json::to_string(&p.messages).unwrap();
    assert!(msg.contains("trunk()..gym@") && msg.contains("create_thread"));
    let p = c
        .get_prompt(GetPromptRequestParams::new("address_review"))
        .await
        .unwrap();
    let msg = serde_json::to_string(&p.messages).unwrap();
    assert!(
        msg.contains("withCode") && msg.contains("edit_code {thread") && !msg.contains("On it"),
        "{msg}"
    );
}

/// §8: every tool call of the full profile shows up as agent.attention (tool name, the windows it
/// looked at where it has any), successful or not.
#[tokio::test]
async fn every_tool_call_records_attention() {
    let fake = Arc::new(FakeBackend::sample());
    let t = fake.human_thread(
        "src/bus.py",
        6,
        6,
        "Cap retries\n```suggestion\n        retries = 5\n```",
        "suggestion",
        Some("agent"),
    );
    let thread = t["id"].as_str().unwrap().to_string();
    fake.add_conflict("src/world.py", 7, 7, "entity.x += 1", "entity.x += 2");
    fake.set_view_state(json!({ "viewId": "v-snapshot", "focus": "w:src/bus.py",
        "viewport": { "rect": [0, 0, 1, 1], "zoom": 1 }, "lens": "none", "followAgent": false }));
    let c = connect(fake.clone(), Profile::Full).await;
    let args = |tool: &str| -> Value {
        match tool {
            "set_view" => json!({ "spec": { "mode": "snapshot", "rev": "@" } }),
            "focus" => json!({ "target": "src/bus.py:5" }),
            "create_tour" => {
                json!({ "title": "t", "steps": [{ "target": "src/bus.py:5", "note": "emit" }] })
            }
            "search" => json!({ "query": "emit" }),
            "read_window" => json!({ "window": "w:src/bus.py" }),
            "read_code" => json!({ "path": "src/world.py" }),
            "get_links" => json!({ "target": "src/world.py:8" }),
            "get_hub" => json!({ "key": "world.entity.moved" }),
            "get_metrics" => json!({ "target": "src/bus.py" }),
            "bind_workspace" => json!({ "workspace": "default" }),
            "get_thread" | "apply_suggestion" => json!({ "thread": thread }),
            "create_thread" => {
                json!({ "anchor": { "path": "src/world.py", "start": 1, "end": 1 }, "body": "why?" })
            }
            "reply" => json!({ "thread": thread, "body": "ok" }),
            "set_thread_status" => json!({ "thread": thread, "status": "addressed" }),
            "get_diff" => json!({ "revset": "trunk()..@" }),
            "get_interdiff" => json!({ "change": CHANGE, "from": "c0ffee" }),
            "edit_code" | "propose_edit" => {
                json!({ "path": "src/world.py", "content": "x = 1\n", "dryRun": true })
            }
            "apply_edit" | "undo_edit" => json!({ "edit": "e_missing" }),
            "get_conflict" | "resolve_conflict" => {
                json!({ "rev": CHANGE, "path": "src/world.py", "content": "x\n" })
            }
            "declare_link" => {
                json!({ "from": "src/world.py:8", "to": "src/bus.py:5", "kind": "call", "label": "emit" })
            }
            "rate_link" => json!({ "link": "l1", "verdict": "confirm" }),
            "label_cluster" => json!({ "cluster": "c1", "label": "bus" }),
            "wait_for_events" => json!({ "timeoutS": 1 }),
            _ => json!({}),
        }
    };
    let mut silent = Vec::new();
    for tool in cv_mcp::server::FULL {
        let before = fake.events_log().len();
        let (_is_err, _, _) = call(&c, tool, args(tool)).await;
        let new: Vec<Value> = fake.events_log()[before..]
            .iter()
            .filter(|e| e["type"] == "agent.attention" && e["tool"] == *tool)
            .cloned()
            .collect();
        match new.as_slice() {
            [] => silent.push(tool.to_string()),
            [e, ..] => {
                assert_eq!(e["agent"], "claude", "{tool}: {e}");
                assert!(
                    e["windows"].is_array() && e["at"].is_string(),
                    "{tool}: {e}"
                );
                // Reads that look at code name its windows.
                let expect_windows = matches!(
                    *tool,
                    "get_view_state"
                        | "read_window"
                        | "read_code"
                        | "get_links"
                        | "get_metrics"
                        | "get_hotspots"
                        | "list_conflicts"
                        | "get_thread"
                );
                if expect_windows {
                    assert!(
                        !e["windows"].as_array().unwrap().is_empty(),
                        "{tool} recorded no windows: {e}"
                    );
                }
            }
        }
    }
    assert!(
        silent.is_empty(),
        "tools without agent.attention: {silent:?}"
    );
}

/// N10 with the 2026-07-28 lifecycle: subscriptions/listen carries resource updates for the
/// accepted URIs only.
#[tokio::test]
async fn listen_subscription_gets_resource_updates() {
    use rmcp::model::{ProtocolVersion, ServerNotification, SubscriptionFilter};
    use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
    let fake = Arc::new(FakeBackend::sample());
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let server = CvMcp::new(fake.clone(), McpConfig::default());
    let probe = server.clone();
    tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server starts");
        let _ = running.waiting().await;
    });
    let c = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("claude-code", "1.0"),
    )
    .serve_with_lifecycle(
        client_io,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::LATEST],
        },
    )
    .await
    .expect("client connects");
    let mut sub = c
        .peer()
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["cv://threads?status=open", "cv://nope"])
                .build(),
        )
        .await
        .expect("listen");
    assert_eq!(
        sub.acknowledged().resource_subscriptions,
        Some(vec!["cv://threads?status=open".to_string()])
    );
    // The SDK acknowledges a listen before running the handler that registers it: wait for the
    // registration (the watcher then follows the log from the cursor read just before it).
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while probe.subscriber_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "listen never registered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    fake.human_thread("src/bus.py", 1, 1, "Name?", "question", None);
    let n = tokio::time::timeout(std::time::Duration::from_secs(10), sub.next())
        .await
        .expect("an update within 10 s")
        .expect("stream ok")
        .expect("a notification");
    match n {
        ServerNotification::ResourceUpdatedNotification(u) => {
            assert_eq!(u.params.uri, "cv://threads?status=open")
        }
        other => panic!("unexpected {other:?}"),
    }
}
