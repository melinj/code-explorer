#!/usr/bin/env python3
"""Record request/response pairs from a running cv serve or cv-mock (WORKSTREAMS C0 contract replay).

    python3 -I scripts/replay/record.py BASE_URL OUT_DIR [--token T] [--scenario linear|conflict|workspaces|mock|resync]

Walks the routes of docs/contracts/http.ts in a fixed order, feeding ids from earlier responses into
later requests, and writes one JSON file per route call: {route, method, path, status, body?, response}.
`route` is the key of `Routes` in http.ts, which check.mjs uses to type-check the bodies.

Recordings are normalized so re-recording the same fixture is stable: arrays are cut to their first
MAX_ITEMS elements (longer than any tuple in the contract), long strings are cut, and volatile values
(timestamps, op ids, generated ids, the repo root) are replaced by placeholders of the same JSON type.
"""

import json
import math
import os
import base64
import re
import socket
import struct
import sys
import urllib.error
import urllib.parse
import urllib.request

MAX_ITEMS = 6
MAX_STR = 160


def main():
    args = sys.argv[1:]
    if len(args) < 2:
        print(__doc__)
        sys.exit(2)
    base, out = args[0].rstrip("/"), args[1]
    token = None
    scenario = "linear"
    if "--token" in args:
        token = args[args.index("--token") + 1]
    if "--scenario" in args:
        scenario = args[args.index("--scenario") + 1]
    os.makedirs(out, exist_ok=True)
    for f in os.listdir(out):
        if f.endswith(".json"):
            os.remove(os.path.join(out, f))
    rec = Recorder(base, out, token)
    {"linear": walk_linear, "conflict": walk_conflict, "workspaces": walk_workspaces, "mock": walk_mock,
     "resync": walk_resync}[scenario](rec)
    rec.finish()


class Recorder:
    def __init__(self, base, out, token):
        self.base, self.out, self.token = base, out, token
        self.seen = {}
        self.volatile = {}
        self.count = 0
        self.root = None

    def call(self, route, path, body=None, query=None, name=None, expect=None):
        """route: 'GET /api/v1/repo' (the Routes key); path: concrete path."""
        method = route.split(" ", 1)[0]
        url = self.base + path
        if query:
            url += "?" + urllib.parse.urlencode(query)
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        if self.token:
            req.add_header("Authorization", "Bearer " + self.token)
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                status, raw = r.status, r.read()
        except urllib.error.HTTPError as e:
            status, raw = e.code, e.read()
        except Exception as e:  # noqa: BLE001 — record transport failures as such
            status, raw = 0, json.dumps({"transport": str(e)}).encode()
        try:
            resp = json.loads(raw) if raw else None
        except ValueError:
            resp = {"nonJson": raw[:200].decode("utf-8", "replace")}
        slug = name or slugify(route)
        n = self.seen.get(slug, 0)
        self.seen[slug] = n + 1
        if n:
            slug = f"{slug}-{n + 1}"
        self.count += 1
        entry = {"route": route, "method": method, "path": path, "status": status}
        if query:
            entry["query"] = query
        if body is not None:
            entry["body"] = body
        if expect is not None:
            entry["expectStatus"] = expect
        entry["response"] = resp
        self.pending = getattr(self, "pending", [])
        self.pending.append((f"{self.count:02d}-{slug}.json", entry))
        print(f"  {status} {route}  {path}", file=sys.stderr)
        return status, resp

    def ws_hello(self, query=None, name="ws-hello"):
        """Opens WS /api/v1/ws (optionally ?cursor=…), records the first frame (the hello) and closes.
        A minimal RFC 6455 client: the server's frames are unmasked text frames."""
        u = urllib.parse.urlparse(self.base)
        path = "/api/v1/ws" + ("?" + urllib.parse.urlencode(query) if query else "")
        key = base64.b64encode(os.urandom(16)).decode()
        head = (f"GET {path} HTTP/1.1\r\nHost: {u.netloc}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n")
        if self.token:
            head += f"Authorization: Bearer {self.token}\r\n"
        status, resp = 0, None
        try:
            with socket.create_connection((u.hostname, u.port or 80), timeout=30) as sk:
                sk.sendall((head + "\r\n").encode())
                buf = b""
                while b"\r\n\r\n" not in buf:
                    chunk = sk.recv(4096)
                    if not chunk:
                        raise OSError("closed during the handshake")
                    buf += chunk
                hdr, buf = buf.split(b"\r\n\r\n", 1)
                status = int(hdr.split(b" ", 2)[1])

                def need(n):
                    nonlocal buf
                    while len(buf) < n:
                        chunk = sk.recv(65536)
                        if not chunk:
                            raise OSError("closed mid-frame")
                        buf += chunk
                    out, buf = buf[:n], buf[n:]
                    return out

                if status == 101:
                    b0, b1 = need(2)
                    n = b1 & 0x7F
                    if n == 126:
                        n = struct.unpack(">H", need(2))[0]
                    elif n == 127:
                        n = struct.unpack(">Q", need(8))[0]
                    payload = need(n)
                    resp = json.loads(payload) if (b0 & 0x0F) == 1 else {"opcode": b0 & 0x0F}
                    try:  # close frame (masked, as a client must send)
                        sk.sendall(bytes([0x88, 0x80]) + os.urandom(4))
                    except OSError:
                        pass
        except Exception as e:  # noqa: BLE001
            status, resp = 0, {"transport": str(e)}
        self.count += 1
        n = self.seen.get(name, 0)
        self.seen[name] = n + 1
        slug = name if not n else f"{name}-{n + 1}"
        entry = {"route": "WS /api/v1/ws", "method": "GET", "path": path, "status": status, "response": resp}
        if query:
            entry["query"] = query
        self.pending = getattr(self, "pending", [])
        self.pending.append((f"{self.count:02d}-{slug}.json", entry))
        print(f"  {status} WS /api/v1/ws  {path}", file=sys.stderr)
        return status, resp

    def finish(self):
        # Ids minted by POST/PUT calls (threads, comments, edits, ...) may embed a clock: replace each
        # with a numbered placeholder, everywhere it occurs as a whole string or a path segment.
        known, minted = set(), {}
        for _, entry in self.pending:
            if entry["method"] in ("POST", "PUT"):
                for v in ids_in(entry.get("response")):
                    if v not in known and v not in minted and len(v) >= 10:
                        minted[v] = f"<id{len(minted) + 1}>"
            known.update(ids_in(entry.get("response")))
        for fname, entry in self.pending:
            entry = replace_ids(entry, minted)
            entry = normalize(entry, self.root)
            with open(os.path.join(self.out, fname), "w") as f:
                json.dump(entry, f, indent=1, sort_keys=True, ensure_ascii=False)
                f.write("\n")
        print(f"recorded {len(self.pending)} calls into {self.out}", file=sys.stderr)


def ids_in(v):
    if isinstance(v, dict):
        for k, x in v.items():
            if k == "id" and isinstance(x, str):
                yield x
            else:
                yield from ids_in(x)
    elif isinstance(v, list):
        for x in v:
            yield from ids_in(x)


def replace_ids(v, minted, key=None):
    if isinstance(v, dict):
        return {k: replace_ids(x, minted, k) for k, x in v.items()}
    if isinstance(v, list):
        return [replace_ids(x, minted) for x in v]
    if isinstance(v, str):
        if v in minted:
            return minted[v]
        if key == "path" and v.startswith("/api/"):
            return "/".join(minted.get(seg, seg) for seg in v.split("/"))
    return v


def slugify(route):
    m, p = route.split(" ", 1)
    p = p.replace("/api/v1/", "").replace(":", "").replace("*", "")
    return (m.lower() + "-" + re.sub(r"[^a-zA-Z0-9]+", "-", p)).strip("-")


ISO = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?$")
TIME_KEYS = {"createdAt", "updatedAt", "time", "committerTime", "lastSeen", "at", "startedAt", "finishedAt"}
VOLATILE_KEYS = {
    "createdAt", "updatedAt", "at", "time", "ts", "lastSeen", "opBefore", "opAfter", "op", "lastOp",
    "cursor", "seq", "session", "token", "committerTime", "since", "startedAt", "finishedAt", "tookMs", "ms", "elapsedMs",
}


def normalize(v, root, key=None):
    if isinstance(v, dict):
        return {k: normalize(x, root, k) for k, x in v.items()}
    if isinstance(v, list):
        return [normalize(x, root, key) for x in v[:MAX_ITEMS]]
    if isinstance(v, str):
        if root and root in v:
            v = v.replace(root, "<ROOT>")
        if key in TIME_KEYS and not ISO.match(v):
            return "<non-iso-time>"  # ids.ts: times are ISO-8601 strings with offset; check.mjs flags it
        if key in VOLATILE_KEYS or ISO.match(v):
            return "<volatile>" if not ISO.match(v) else "2026-01-01T00:00:00Z"
        if len(v) > MAX_STR:
            return v[:MAX_STR]
        return v
    if isinstance(v, float):
        # Parallel sums differ in the last bits between runs: keep 6 significant digits.
        return 0.0 if key in VOLATILE_KEYS else float(f"{v:.6g}")
    if isinstance(v, int) and not isinstance(v, bool) and key in VOLATILE_KEYS:
        return 0
    return v


# ── walks ────────────────────────────────────────────────────────────────────────────────────────

def first(xs, pred=lambda x: True):
    for x in xs or []:
        if pred(x):
            return x
    return None


def tile_of(world):
    size = 4096.0
    z = 0
    span = max(world.get("w", 1), world.get("h", 1), 1)
    while size * 2**z < span and z < 12:
        z += 1
    s = size * 2**z
    return z, math.floor(world.get("x", 0) / s), math.floor(world.get("y", 0) / s)


def walk_common_view(r, spec, frag_path=None, search_q="circle"):
    """Routes that need a view: returns (viewId, bootstrap, tile windows)."""
    st, view = r.call("POST /api/v1/views", "/api/v1/views", body=spec)
    vid = view.get("id") if isinstance(view, dict) else None
    if not vid:
        return None, None, []
    q = urllib.parse.quote(vid, safe="")
    r.call("GET /api/v1/views/:viewId", f"/api/v1/views/{q}")
    _, boot = r.call("GET /api/v1/views/:viewId/bootstrap", f"/api/v1/views/{q}/bootstrap")
    boot = boot if isinstance(boot, dict) else {}
    z, x, y = tile_of(boot.get("world") or {})
    _, tile = r.call("GET /api/v1/views/:viewId/tiles/:z/:x/:y", f"/api/v1/views/{q}/tiles/{z}/{x}/{y}")
    wins = (tile or {}).get("windows") or []
    lv = (tile or {}).get("layoutVersion", 0)
    ids = [w["id"] for w in wins[:8]]
    _, batch = r.call(
        "POST /api/v1/views/:viewId/windows:batch",
        f"/api/v1/views/{q}/windows:batch",
        body={"ids": ids, "parts": ["outline", "diff"], "layoutVersion": lv},
    )
    if wins:
        w0 = wins[0]
        r.call(
            "POST /api/v1/views/:viewId/windows/:windowId/expand",
            f"/api/v1/views/{q}/windows/{urllib.parse.quote(w0['id'], safe='')}/expand",
            body={"before": 3, "after": 3},
        )
        if w0.get("blob"):
            r.call("GET /api/v1/blobs/:blobId/text", f"/api/v1/blobs/{w0['blob']}/text", query={"from": 1, "to": 20})
    r.call("GET /api/v1/views/:viewId/metrics", f"/api/v1/views/{q}/metrics", query={"level": "file"})
    r.call("GET /api/v1/views/:viewId/metrics", f"/api/v1/views/{q}/metrics", query={"level": "cluster"}, name="get-views-viewId-metrics-cluster")
    fp = frag_path or (wins[0].get("path") if wins else None)
    if fp:
        r.call("GET /api/v1/views/:viewId/fragments", f"/api/v1/views/{q}/fragments", query={"path": fp})
    r.call("GET /api/v1/search", "/api/v1/search", query={"q": search_q, "viewId": vid, "limit": 5})
    if wins:
        target = first(wins, lambda w: w.get("ports")) or wins[0]
        _, links = r.call(
            "GET /api/v1/links", "/api/v1/links", query={"viewId": vid, "target": target["id"], "dir": "both"}
        )
        hubs = (boot.get("hubs") or []) + ((links or {}).get("hubs") or [] if isinstance(links, dict) else [])
        if hubs:
            r.call("GET /api/v1/hubs/:hubId", f"/api/v1/hubs/{urllib.parse.quote(hubs[0]['id'], safe='')}", query={"viewId": vid})
        lk = first((links or {}).get("links") if isinstance(links, dict) else [])
        if lk:
            r.call("POST /api/v1/links/:linkId/rate", f"/api/v1/links/{urllib.parse.quote(lk['id'], safe='')}/rate",
                   body={"verdict": "confirmed", "viewId": vid})
        if len(wins) > 1:
            r.call("POST /api/v1/links", "/api/v1/links",
                   body={"from": wins[0]["id"], "to": wins[1]["id"], "kind": "reference", "label": "replay", "viewId": vid})
    if wins and wins[0].get("key") and wins[0].get("rect"):
        w0 = wins[0]
        r.call("PATCH /api/v1/views/:viewId/layout", f"/api/v1/views/{q}/layout",
               body={"moves": [{"key": w0["key"], "x": w0["rect"]["x"] + 90, "y": w0["rect"]["y"], "pinned": False}],
                     "baseLayoutVersion": lv})
        r.call("PATCH /api/v1/views/:viewId/layout", f"/api/v1/views/{q}/layout",
               body={"moves": [], "baseLayoutVersion": lv + 1000}, name="patch-views-viewId-layout-stale", expect=409)
    cl = first(boot.get("clusters"))
    if cl:
        r.call("PUT /api/v1/clusters/:clusterId/label", f"/api/v1/clusters/{urllib.parse.quote(cl['id'], safe='')}/label",
               body={"label": "Replay cluster"})
    return vid, boot, wins


def walk_repo(r):
    _, repo = r.call("GET /api/v1/repo", "/api/v1/repo")
    if isinstance(repo, dict) and isinstance(repo.get("root"), str):
        r.root = repo["root"]
    _, ws = r.call("GET /api/v1/workspaces", "/api/v1/workspaces")
    r.workspaces = ws if isinstance(ws, list) else []
    r.call("GET /api/v1/lanes", "/api/v1/lanes")
    return repo


def walk_compare(r):
    """POST /workspaces:compare: a pair with an overlap warning (200 ViewRef), else the first two
    workspaces (400 no-overlap), and an unknown workspace (404)."""
    ws = getattr(r, "workspaces", [])
    pair = None
    for w in ws:
        for o in w.get("overlaps") or []:
            if o.get("workspace") and not pair:
                pair = (w["name"], o["workspace"])
    if pair:
        r.call("POST /api/v1/workspaces:compare", "/api/v1/workspaces:compare", body={"a": pair[0], "b": pair[1]})
    elif len(ws) >= 2:
        r.call("POST /api/v1/workspaces:compare", "/api/v1/workspaces:compare",
               body={"a": ws[0]["name"], "b": ws[1]["name"]}, name="post-workspacescompare-no-overlap", expect=400)
    if ws:
        r.call("POST /api/v1/workspaces:compare", "/api/v1/workspaces:compare", body={"a": ws[0]["name"], "b": "nope"},
               name="post-workspacescompare-unknown", expect=404)


def walk_history(r, range_revset):
    _, revs = r.call("GET /api/v1/revisions", "/api/v1/revisions", query={"revset": "::@", "limit": 10})
    r.call("POST /api/v1/revsets:validate", "/api/v1/revsets:validate", body={"revset": range_revset})
    r.call("POST /api/v1/revsets:validate", "/api/v1/revsets:validate", body={"revset": "trunk("}, name="post-revsets-validate-bad")
    r.call("PUT /api/v1/ranges/:name", "/api/v1/ranges/replay", body={"name": "replay", "revset": range_revset, "createdBy": {"kind": "human", "name": "you"}})
    r.call("GET /api/v1/ranges", "/api/v1/ranges")
    r.call("DELETE /api/v1/ranges/:name", "/api/v1/ranges/replay")
    return (revs or {}).get("rows") or [] if isinstance(revs, dict) else []


def walk_review(r, path, rev, start, end):
    _, th = r.call("POST /api/v1/threads", "/api/v1/threads",
                   body={"anchor": {"path": path, "rev": rev, "start": start, "end": end}, "body": "Replay: why 80?", "kind": "question"})
    r.call("GET /api/v1/threads", "/api/v1/threads")
    if isinstance(th, dict) and th.get("id"):
        tid = urllib.parse.quote(th["id"], safe="")
        r.call("GET /api/v1/threads/:threadId", f"/api/v1/threads/{tid}")
        r.call("POST /api/v1/threads/:threadId/comments", f"/api/v1/threads/{tid}/comments", body={"body": "Tuned on purpose."})
        r.call("PATCH /api/v1/threads/:threadId", f"/api/v1/threads/{tid}", body={"status": "in-progress", "assignee": "agent"})
        r.call("POST /api/v1/reviews:submit", "/api/v1/reviews:submit", body={"threads": [th["id"]], "note": "replay"})
    return th


def walk_agent(r, spec, workspace, site=None, window=None):
    site = site or {"path": "src/shapes.py", "line": 30}
    r.call("POST /api/v1/agent/attention", "/api/v1/agent/attention",
           body={"agent": "replay", "workspace": workspace, "tool": "get_window", "windows": [window] if window else [], "note": "replay"})
    r.call("POST /api/v1/agent/focus", "/api/v1/agent/focus", body={"target": site, "lines": [site["line"], site["line"] + 2], "note": "look"})
    r.call("POST /api/v1/agent/tours", "/api/v1/agent/tours", body={"title": "Replay tour", "steps": [{"target": site, "note": "start here"}]})
    r.call("GET /api/v1/agent/permissions", "/api/v1/agent/permissions")
    r.call("PUT /api/v1/agent/permissions", "/api/v1/agent/permissions", body={"moveCamera": True})
    r.call("POST /api/v1/agent/bind", "/api/v1/agent/bind", body={"agent": "replay", "client": "contract-replay", "workspace": workspace})
    r.call("POST /api/v1/agent/view", "/api/v1/agent/view", body={"spec": spec})
    r.call("GET /api/v1/agent/view-state", "/api/v1/agent/view-state")
    r.call("GET /api/v1/tours", "/api/v1/tours")
    r.call("GET /api/v1/events", "/api/v1/events", query={"cursor": 0, "timeoutS": 0})


def walk_edit(r, target, path, content):
    _, ed = r.call("POST /api/v1/edits", "/api/v1/edits",
                   body={"target": target, "files": [{"path": path, "content": content}], "mode": "amend", "dryRun": True})
    if isinstance(ed, dict) and ed.get("id"):
        r.call("GET /api/v1/edits/:editId", f"/api/v1/edits/{urllib.parse.quote(ed['id'], safe='')}")
    return ed


def walk_linear(r):
    walk_repo(r)
    rows = walk_history(r, 'description(substring:"B:")::description(substring:"D:")')
    spec = {"mode": "snapshot", "rev": "@"}
    vid, boot, wins = walk_common_view(r, spec, frag_path="src/shapes.py")
    # Non-convex range view: B | D.
    rspec = {"mode": "range", "revset": 'description(substring:"B:") | description(substring:"D:")',
             "options": {"diff": "cumulative", "contextHops": 1, "contextDisplay": "minimized", "contextLines": 3, "ignoreWhitespace": False}}
    st, rv = r.call("POST /api/v1/views", "/api/v1/views", body=rspec, name="post-views-range")
    if isinstance(rv, dict) and rv.get("id"):
        q = urllib.parse.quote(rv["id"], safe="")
        r.call("GET /api/v1/views/:viewId/bootstrap", f"/api/v1/views/{q}/bootstrap", name="get-views-viewId-bootstrap-range")
    d = first(rows, lambda x: str((x.get("rev") or {}).get("description", "")).startswith("D:"))
    d = (d or {}).get("rev") or {}
    dchange = d.get("change")
    th = walk_review(r, "src/shapes.py", dchange or "@-", 30, 32)
    if dchange:
        commit = d.get("commit")
        r.call("PUT /api/v1/reviews/marks/:change", f"/api/v1/reviews/marks/{dchange}", body={"commit": commit})
        r.call("GET /api/v1/interdiff", "/api/v1/interdiff", query={"change": dchange, "from": commit})
    r.call("GET /api/v1/diff", "/api/v1/diff", query={"from": 'description(substring:"B:")', "to": 'description(substring:"D:")'})
    r.call("GET /api/v1/diff", "/api/v1/diff", query={"from": "root()", "to": "@", "fileset": "src"}, name="get-diff-fileset")
    ed = walk_edit(r, "@-", "config/app.yaml", "name: fixture\nretries: 7\nfeatures:\n  fast: true\n")
    walk_agent(r, spec, "default", window=wins[0]["id"] if wins else None)
    if isinstance(ed, dict) and ed.get("id"):  # writes: the fixture is a throwaway copy
        eid = urllib.parse.quote(ed["id"], safe="")
        r.call("POST /api/v1/edits/:editId/apply", f"/api/v1/edits/{eid}/apply")
        r.call("POST /api/v1/edits/:editId/undo", f"/api/v1/edits/{eid}/undo")
    r.call("GET /api/v1/conflicts", "/api/v1/conflicts")


def walk_conflict(r):
    walk_repo(r)
    _, cs = r.call("GET /api/v1/conflicts", "/api/v1/conflicts", query={"revset": "all()"})
    c = first(cs) if isinstance(cs, list) else None
    if c:
        commit = c.get("commit")
        path = c.get("path")
        r.call("GET /api/v1/conflicts/:commit/*path", f"/api/v1/conflicts/{commit}/{path}")
        r.call("POST /api/v1/revsets:validate", "/api/v1/revsets:validate", body={"revset": "conflicts()"})
    walk_common_view(r, {"mode": "snapshot", "rev": "@"}, search_q="speed")
    if c:  # writes last: the fixture is a throwaway copy
        r.call("POST /api/v1/conflicts/:commit/*path/resolve", f"/api/v1/conflicts/{c.get('commit')}/{c.get('path')}/resolve",
               body={"content": "def speed():\n    return 3\n\n\ndef name():\n    return \"d\"\n"})


def walk_workspaces(r):
    walk_repo(r)
    r.call("POST /api/v1/agent/bind", "/api/v1/agent/bind", body={"agent": "replay", "client": "contract-replay", "workspace": "agent"})
    r.call("GET /api/v1/workspaces", "/api/v1/workspaces", name="get-workspaces-after-bind")
    r.call("POST /api/v1/agent/bind", "/api/v1/agent/bind", body={"agent": "replay", "workspace": "nope"},
           name="post-agent-bind-unknown", expect=404)
    walk_compare(r)
    walk_common_view(r, {"mode": "snapshot", "rev": "@", "options": {"liveWorkingCopy": True}}, search_q="greet")
    walk_common_view(r, {"mode": "snapshot", "rev": "ws2@", "options": {"workspace": "ws2"}}, search_q="bye")


def walk_mock(r):
    walk_repo(r)
    walk_compare(r)
    rows = walk_history(r, "trunk()..@")
    spec = {"mode": "snapshot", "rev": "@"}
    vid, boot, wins = walk_common_view(r, spec)
    w = first(wins, lambda x: x.get("path"))
    row = (first(rows) or {}).get("rev") or {}
    change = row.get("change")
    th = walk_review(r, (w or {}).get("path", "src/a.py"), change or "@", 1, 2)
    if change:
        r.call("PUT /api/v1/reviews/marks/:change", f"/api/v1/reviews/marks/{change}", body={"commit": row.get("commit")})
        r.call("GET /api/v1/interdiff", "/api/v1/interdiff", query={"change": change, "from": row.get("commit")})
    r.call("GET /api/v1/diff", "/api/v1/diff", query={"from": "trunk()", "to": "@"})
    ed = walk_edit(r, change or "@", (w or {}).get("path", "src/a.py"), "x = 1\n")
    if isinstance(ed, dict) and ed.get("id"):
        eid = urllib.parse.quote(ed["id"], safe="")
        r.call("POST /api/v1/edits/:editId/apply", f"/api/v1/edits/{eid}/apply")
        r.call("POST /api/v1/edits/:editId/undo", f"/api/v1/edits/{eid}/undo")
    walk_agent(r, spec, "default", site={"path": (w or {}).get("path", "src/a.py"), "line": 1}, window=(w or {}).get("id"))
    r.ws_hello()
    _, cs = r.call("GET /api/v1/conflicts", "/api/v1/conflicts")
    c = first(cs) if isinstance(cs, list) else None
    if c:
        r.call("GET /api/v1/conflicts/:commit/*path", f"/api/v1/conflicts/{c.get('commit')}/{c.get('path')}")
        r.call("POST /api/v1/conflicts/:commit/*path/resolve", f"/api/v1/conflicts/{c.get('commit')}/{c.get('path')}/resolve",
               body={"content": "resolved\n"})


def walk_resync(r):
    """cv serve on a repo whose event log was aged past retention (contract-replay.sh ages it and
    restarts the server): a cursor older than the oldest kept event gets `resync: true` from both
    GET /events and the WS hello; a fresh connection gets a plain hello."""
    r.call("GET /api/v1/events", "/api/v1/events", query={"cursor": 1, "timeoutS": 0}, name="get-events-resync")
    r.ws_hello({"cursor": 1}, name="ws-hello-resync")
    r.ws_hello()


if __name__ == "__main__":
    main()
