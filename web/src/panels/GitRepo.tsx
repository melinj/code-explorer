import { useEffect, useRef, useState } from "react";
import type { GitCommitDetail, SetupInfo } from "../contracts/http";
import type { GraphRow } from "../contracts/model";
import { DARK, LIGHT, applyCssVars } from "../canvas/theme";
import { Live } from "../data/live";
import { GraphPanel } from "./GraphPanel";
import { ago } from "./Panels";
import { useEscape } from "./useEscape";

const JJ_INSTALL = "https://jj-vcs.github.io/jj/latest/install-and-setup/";
/** While the server restarts and indexes the new jj repository, ask /setup this often. */
const POLL_MS = 1000;

/** GET /setup; undefined when the server does not answer it (an older server: assume jj). */
export async function loadSetup(): Promise<SetupInfo | undefined> {
  const r = await fetch("/api/v1/setup").catch(() => undefined);
  if (!r?.ok) return undefined;
  return (await r.json().catch(() => undefined)) as SetupInfo | undefined;
}

type Phase = { kind: "idle" } | { kind: "confirm" } | { kind: "running" } | { kind: "restarting"; since: number; message: string } | { kind: "failed"; message: string };

/**
 * `cv serve` on a plain git repository (http.ts GET /setup, vcs "git"): the history, read-only, and
 * what is missing for everything else, with a button that runs `jj git init --colocate` there.
 * The server restarts itself on the new jj repository; this page waits for it, then reloads into
 * the full app. Running the command in a terminal works too: the server notices `.jj` and restarts.
 */
export function GitRepoScreen({ setup }: { setup: SetupInfo }) {
  const live = useRef(new Live()).current; // never started: git mode has no events
  const [dark, setDark] = useState(() => matchMedia("(prefers-color-scheme: dark)").matches);
  const [width, setWidth] = useState(() => Math.max(360, Math.round(window.innerWidth * 0.5)));
  const [picked, setPicked] = useState<GraphRow>();
  const [max, setMax] = useState(false);
  const [detail, setDetail] = useState<GitCommitDetail>();
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const [copied, setCopied] = useState(false);
  const [, tick] = useState(0);
  const name = setup.root.split("/").filter(Boolean).pop() ?? setup.root;
  const command = `cd ${JSON.stringify(setup.root)} && ${setup.initCommand}`;

  useEffect(() => {
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    applyCssVars(dark ? DARK : LIGHT);
  }, [dark]);
  useEffect(() => {
    document.title = `${name} · Code Viewer (git)`;
  }, [name]);

  // The commit picked in the graph: its files.
  useEffect(() => {
    setDetail(undefined);
    if (!picked) return;
    let alive = true;
    fetch(`/api/v1/git/commits/${picked.rev.commit}`)
      .then((r) => (r.ok ? r.json() : undefined))
      .then((d) => alive && d && setDetail(d))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [picked?.rev.commit]);

  // jj set up (here or in a terminal): wait for the full server, then reload into the app.
  useEffect(() => {
    const t = setInterval(async () => {
      tick((n) => n + 1);
      const s = await loadSetup().catch(() => undefined);
      if (s?.vcs === "jj") location.reload();
      else if (!s && phase.kind !== "restarting") setPhase({ kind: "restarting", since: Date.now(), message: "The server is restarting with jj" });
    }, POLL_MS);
    return () => clearInterval(t);
  }, [phase.kind]);

  const init = async () => {
    setPhase({ kind: "running" });
    try {
      const r = await fetch("/api/v1/setup:init-jj", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ confirm: true }) });
      const b = await r.json().catch(() => ({}));
      if (!r.ok) throw new Error(b.error?.message ?? b.message ?? r.statusText);
      setPhase(b.restarting ? { kind: "restarting", since: Date.now(), message: b.message } : { kind: "failed", message: b.message });
    } catch (e) {
      setPhase({ kind: "failed", message: (e as Error).message });
    }
  };
  const copy = () => {
    navigator.clipboard?.writeText(command).then(() => (setCopied(true), setTimeout(() => setCopied(false), 1500))).catch(() => {});
  };

  return (
    <div className="app git-app">
      <header className="top">
        <h1>Code Viewer</h1>
        <span className="chip mono" title={setup.root}>
          {name}
        </span>
        <span className="chip warn">git only · jj not set up</span>
        <div className="grow" />
        <button className="ghost" onClick={() => setDark((d) => !d)} title="Toggle theme">
          {dark ? "Light" : "Dark"}
        </button>
      </header>
      <main className={`git-main ${max ? "graph-max" : ""}`} style={{ ["--graph-w" as string]: `${width}px` }}>
        <GraphPanel live={live} width={width} onResize={setWidth} vcs="git" onPick={(rows) => setPicked(rows[0])} expanded={max} onExpand={setMax} />
        <section className="git-side">
          <div className="card setup-card" role="region" aria-label="Set up jj">
            <h2>Set up jj to open the map</h2>
            <p>
              <b className="mono">{setup.root}</b> is a git repository. Code Viewer builds its map, reviews, edits and the graph operations on <a href="https://jj-vcs.github.io/jj/">jj</a>, which works alongside git in the same folder. Until it is set up, you can browse the history here, read-only.
            </p>
            <ul className="small">
              <li>
                Runs <code>{setup.initCommand}</code> in the repository: it adds a <code>.jj</code> folder next to <code>.git</code>. Your branches become jj bookmarks.
              </li>
              <li>git keeps working. After jj commands, git's HEAD is detached; that is normal in a colocated repository.</li>
              <li>
                To go back, delete the <code>.jj</code> folder. Nothing in <code>.git</code> is rewritten.
              </li>
            </ul>
            {!setup.jj.installed ? (
              <div className="warn-box" role="alert">
                jj is not installed on this machine (or not on the server's PATH). <a href={JJ_INSTALL}>Install jj</a>, then reload this page.
              </div>
            ) : (
              <div className="small muted">jj {setup.jj.version} found.</div>
            )}
            {phase.kind === "restarting" ? (
              <div className="setup-wait" role="status">
                <span className="spinner" aria-hidden /> {phase.message}. This page reloads by itself once the repository is indexed (a minute on a large one; {Math.round((Date.now() - phase.since) / 1000)} s so far).
              </div>
            ) : (
              <div className="row setup-actions">
                <button className="primary" disabled={!setup.jj.installed || phase.kind === "running"} onClick={() => setPhase({ kind: "confirm" })}>
                  {phase.kind === "running" ? "Initialising…" : "Initialise jj here"}
                </button>
                <span className="small muted">or in a terminal:</span>
              </div>
            )}
            <div className="cmd">
              <code className="mono">{command}</code>
              <button className="small" onClick={copy}>
                {copied ? "Copied" : "Copy"}
              </button>
            </div>
            {phase.kind === "failed" && (
              <div className="err" role="alert">
                {phase.message}
              </div>
            )}
          </div>
          <CommitDetail row={picked} detail={detail} />
        </section>
      </main>
      {phase.kind === "confirm" && <ConfirmInit setup={setup} onCancel={() => setPhase({ kind: "idle" })} onConfirm={init} />}
      <footer className="status mono">
        <span>git mode · {setup.root}</span>
      </footer>
    </div>
  );
}

function ConfirmInit({ setup, onCancel, onConfirm }: { setup: SetupInfo; onCancel: () => void; onConfirm: () => void }) {
  useEscape(onCancel);
  return (
    <div className="modal-back" onClick={onCancel}>
      <div className="modal" role="alertdialog" aria-label="Initialise jj" onClick={(e) => e.stopPropagation()}>
        <h2>Initialise jj in this repository?</h2>
        <p className="small">
          Runs <code>{setup.initCommand}</code> in <b className="mono">{setup.root}</b>, then restarts Code Viewer on it. git keeps working; delete <code>.jj</code> to undo.
        </p>
        <div className="row end">
          <button onClick={onCancel}>Cancel</button>
          <button className="primary" autoFocus onClick={onConfirm}>
            Initialise jj
          </button>
        </div>
      </div>
    </div>
  );
}

/** The commit picked in the graph: message, author, refs, parents and files. */
function CommitDetail({ row, detail }: { row?: GraphRow; detail?: GitCommitDetail }) {
  if (!row) return <div className="card muted small">Click a commit in the graph to see its message and files.</div>;
  const r = row.rev;
  return (
    <div className="card commit-detail" aria-label="Commit">
      <div className="row wrap">
        <span className="mono">{r.commit.slice(0, 12)}</span>
        {r.workspaces.map((w) => (
          <span key={w} className="chip ws">
            {w}
          </span>
        ))}
        {r.bookmarks.map((b) => (
          <span key={b} className="chip bm">
            {b}
          </span>
        ))}
        {r.tags.map((t) => (
          <span key={t} className="chip tag">
            {t}
          </span>
        ))}
      </div>
      <pre className="commit-msg">{r.description || "(no message)"}</pre>
      <div className="small muted">
        {r.author.name} &lt;{r.author.email}&gt; · {ago(r.author.time)} · {r.parents.length === 0 ? "root commit" : r.parents.length > 1 ? `merge of ${r.parents.map((p) => p.slice(0, 8)).join(", ")}` : `parent ${r.parents[0].slice(0, 8)}`}
      </div>
      {!detail ? (
        <div className="small muted">loading files…</div>
      ) : (
        <ul className="commit-files small mono">
          {detail.files.length === 0 && <li className="muted">no file changes</li>}
          {detail.files.map((f) => (
            <li key={f.path}>
              <span className="add">{f.added === undefined ? "bin" : `+${f.added}`}</span> <span className="del">{f.removed === undefined ? "" : `−${f.removed}`}</span> {f.path}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
