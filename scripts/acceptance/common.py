"""Shared helpers for the acceptance harnesses (scripts/acceptance/*.py), which measure cv on a real repo.

Every harness works on a throwaway jj clone of that repo (never the original; see prepare.sh)
and talks to `cv serve` over HTTP. Reads only: cv indexes the snapshot at @ and never writes to
the repo; its review.db lives under ~/.local/share/code-viewer/<repo id>/.
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
RESULTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results")
CV = os.environ.get("CV_BIN", os.path.join(ROOT, "target", "release", "cv"))
PORT = int(os.environ.get("CV_ACCEPT_PORT", "7997"))
BASE = f"http://127.0.0.1:{PORT}/api/v1"


def http(method, path, body=None, timeout=600):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"} if data else {})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read() or b"null")


def get(path, **kw):
    return http("GET", path, **kw)


class Server:
    """`cv serve <repo> --no-auth --no-ui --port PORT`, started unless one already answers."""

    def __init__(self, repo, env=None, log=None, args=()):
        self.repo, self.env, self.proc, self.args = repo, env or {}, None, list(args)
        self.log = log or os.path.join(RESULTS, "..", ".cv-serve.log")

    def __enter__(self):
        try:
            get("/repo", timeout=2)
            sys.exit(f"port {PORT} is busy: stop that server (or set CV_ACCEPT_PORT)")
        except Exception:
            pass
        if not os.path.exists(CV):
            sys.exit(f"{CV} missing: cargo build --release -p cv-server")
        env = dict(os.environ, **self.env)
        self.proc = subprocess.Popen(
            [CV, "serve", self.repo, "--no-auth", "--no-ui", "--port", str(PORT), *self.args],
            stdout=open(self.log, "w"), stderr=subprocess.STDOUT, env=env)
        t0 = time.time()
        while time.time() - t0 < 900:
            if self.proc.poll() is not None:
                sys.exit(f"cv serve exited; see {self.log}")
            try:
                get("/repo", timeout=2)
                break
            except Exception:
                time.sleep(1)
        print(f"[cv] serving {self.repo} on {PORT} after {time.time() - t0:.0f}s", file=sys.stderr)
        return self

    def __exit__(self, *exc):
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def view(self):
        return http("POST", "/views", {"mode": "snapshot", "rev": "@"})["id"]


def head_commit(repo):
    """The revision a result was measured on, for result headers.

    prepare.sh clones the repo and runs `jj new 'trunk()'`, so the clone's @ is an empty commit
    that exists only in that clone; its id alone cannot be found in the source repo later. Name
    the parent too, which is the source repo's trunk commit, and say when @ is not empty (local
    edits in the clone)."""
    out = subprocess.run(["jj", "--ignore-working-copy", "--at-op", "@", "--no-pager", "log", "--no-graph",
                          "-r", "@", "-T",
                          'commit_id.short(12) ++ " " ++ parents.map(|c| c.commit_id().short(12)).join(",") '
                          '++ " " ++ if(empty, "empty", "edited")'],
                         cwd=repo, capture_output=True, text=True, check=True).stdout.split()
    at, parents, state = out[0], out[1], out[2]
    if state == "empty":
        return f"trunk {parents} (the clone's empty @ {at})"
    return f"{at} (edited in the clone; parent {parents})"


def json_stream(text):
    """The JSON objects printed one after another (axes.py, config_fields.py)."""
    dec, i, out = json.JSONDecoder(), 0, []
    while True:
        rest = text[i:].lstrip()
        if not rest:
            return out
        obj, n = dec.raw_decode(rest)
        out.append(obj)
        i = len(text) - len(rest) + n


def reference(script, repo, cache, *args, timeout=3600):
    """Run docs/analysis/<script> on repo (or reuse `cache` if it exists); returns its JSON objects."""
    if not (cache and os.path.exists(cache)):
        out = subprocess.run([sys.executable, "-I", os.path.join(ROOT, "docs", "analysis", script), repo,
                              *map(str, args)], capture_output=True, text=True, check=True, timeout=timeout)
        if cache:
            with open(cache, "w") as f:
                f.write(out.stdout)
        text = out.stdout
    else:
        text = open(cache).read()
    return json_stream(text)


class Table:
    """Rows of (criterion, reference, cv, tolerance, ok) printed and written to results/<name>.txt."""

    def __init__(self, name, title):
        self.name, self.title, self.rows, self.notes = name, title, [], []

    def row(self, what, ref, got, tol, ok):
        self.rows.append((what, str(ref), str(got), str(tol), "PASS" if ok else "FAIL" if ok is False else "info"))

    def note(self, s):
        self.notes.append(s)

    def render(self):
        hdr = ("criterion", "reference", "cv", "tolerance", "result")
        w = [max(len(r[i]) for r in self.rows + [hdr]) for i in range(5)]
        lines = [self.title, ""]
        for r in [hdr, tuple("-" * x for x in w)] + self.rows:
            lines.append("  ".join(c.ljust(w[i]) for i, c in enumerate(r)).rstrip())
        lines += [""] + self.notes
        fails = sum(r[4] == "FAIL" for r in self.rows)
        passes = sum(r[4] == "PASS" for r in self.rows)
        lines.append(f"\n{passes} PASS, {fails} FAIL")
        return "\n".join(lines) + "\n"

    def failed(self):
        return sum(r[4] == "FAIL" for r in self.rows)

    def write(self):
        os.makedirs(RESULTS, exist_ok=True)
        text = self.render()
        print(text)
        with open(os.path.join(RESULTS, self.name + ".txt"), "w") as f:
            f.write(text)
        return text


def within(ref, got, rel=0.0, abs_=0):
    return abs(got - ref) <= max(abs_, rel * abs(ref))
