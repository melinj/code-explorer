import { useEffect, useState } from "react";
import type { GraphOpResult, OpDetail, OpEntry, OpLog } from "../contracts/model";
import type { Live } from "../data/live";
import { ago } from "./Panels";
import { api, post } from "./graphApi";

const PAGE = 100;
const short = (id: string) => id.slice(0, 12);

/**
 * The operation log (jj op log) inside the Graph panel: newest first, snapshots hidden unless asked
 * for. A row opens what that operation changed; "Undo" reverts just that operation (jj op revert,
 * POST /graph:undo) and "Restore to here…" puts the whole repository back as it was after it
 * (jj op restore, previewed first). Both are operations themselves, so they can be undone too.
 */
export function OpsLog({ live, label, onApplied }: { live: Live; label: (change: string) => string; onApplied: (r: GraphOpResult) => void }) {
  const [log, setLog] = useState<OpLog>();
  const [limit, setLimit] = useState(PAGE);
  const [snapshots, setSnapshots] = useState(false);
  const [open, setOpen] = useState<string>();
  const [detail, setDetail] = useState<OpDetail>();
  const [restore, setRestore] = useState<OpEntry>();
  const [err, setErr] = useState<string>();

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const load = () =>
      api<OpLog>(`/ops?limit=${limit}`)
        .then((l) => alive && (setLog(l), setErr(undefined)))
        .catch((e) => alive && setErr((e as Error).message));
    load();
    const off = live.on((e) => {
      if (e.type === "op.changed") {
        clearTimeout(timer);
        timer = setTimeout(load, 150);
      }
    });
    return () => {
      alive = false;
      clearTimeout(timer);
      off();
    };
  }, [live, limit]);

  useEffect(() => {
    setDetail(undefined);
    if (!open) return;
    let alive = true;
    api<OpDetail>(`/ops/${open}`)
      .then((d) => alive && setDetail(d))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [open]);

  const undo = async (o: OpEntry) => {
    try {
      const r = await post<GraphOpResult>("/graph:undo", { op: o.op });
      if (r.status === "applied") onApplied(r);
      else setErr(r.error?.message ?? r.summary);
    } catch (e) {
      setErr((e as Error).message);
    }
  };

  const ops = (log?.ops ?? []).filter((o) => snapshots || !o.snapshot || o.op === log?.current);
  const hidden = (log?.ops.length ?? 0) - ops.length;
  return (
    <div className="gp-body ops" aria-label="Operation log">
      <div className="ops-bar small">
        <span className="muted">{log ? `${log.ops.length}${log.truncated ? "+" : ""} operations, newest first` : "loading…"}</span>
        <span className="grow" />
        <label className="row">
          <input type="checkbox" checked={snapshots} onChange={(e) => setSnapshots(e.target.checked)} /> Snapshots{!snapshots && hidden > 0 ? ` (${hidden} hidden)` : ""}
        </label>
      </div>
      {err && <div className="err small pad0">{err}</div>}
      {log && !log.ops.length && <div className="muted small pad0">No operation log here.</div>}
      <ol className="ops-list">
        {ops.map((o) => {
          const current = o.op === log?.current;
          const isOpen = open === o.op;
          return (
            <li key={o.op} className={`op ${isOpen ? "open" : ""} ${o.snapshot ? "snap" : ""}`}>
              <button className="op-row" aria-expanded={isOpen} onClick={() => setOpen(isOpen ? undefined : o.op)} title={`${o.op}\n${o.user}`}>
                <span className="mono op-id">{short(o.op)}</span>
                {current && <span className="chip ws">current</span>}
                <span className="op-desc">{o.description || "(no description)"}</span>
                <span className="small muted op-when">{ago(o.time)}</span>
              </button>
              {isOpen && (
                <div className="op-detail small">
                  <div className="muted">
                    {new Date(o.time).toLocaleString()} · {o.user}
                  </div>
                  {!detail ? (
                    <div className="muted">loading…</div>
                  ) : (
                    <>
                      {detail.bookmarks.map((b) => (
                        <div key={b.name} className="mono">
                          {b.name}: {b.from ? b.from.slice(0, 8) : "(new)"} → {b.to ? b.to.slice(0, 8) : "(deleted)"}
                        </div>
                      ))}
                      {detail.changed.length > 0 && <div>Changed: {detail.changed.slice(0, 6).map(label).join(", ")}{detail.changed.length > 6 ? ` and ${detail.changed.length - 6} more` : ""}</div>}
                      {detail.abandoned.length > 0 && <div>Abandoned: {detail.abandoned.slice(0, 6).map(label).join(", ")}{detail.abandoned.length > 6 ? ` and ${detail.abandoned.length - 6} more` : ""}</div>}
                      {!detail.bookmarks.length && !detail.changed.length && !detail.abandoned.length && <div className="muted">No change to commits or bookmarks.</div>}
                    </>
                  )}
                  <div className="row">
                    {o.parents.length === 1 && (
                      <button className="small" onClick={() => undo(o)} title="jj op revert: take back just this operation, keeping everything after it">
                        Undo this operation
                      </button>
                    )}
                    {!current && o.parents.length > 0 && (
                      <button className="small" onClick={() => setRestore(o)} title="jj op restore: the whole repository as it was right after this operation">
                        Restore to here…
                      </button>
                    )}
                  </div>
                </div>
              )}
            </li>
          );
        })}
      </ol>
      {log?.truncated && (
        <button className="gp-more" onClick={() => setLimit((l) => l + PAGE)}>
          Show {PAGE} more
        </button>
      )}
      {restore && <RestoreDialog op={restore} onClose={() => setRestore(undefined)} onApplied={(r) => (setRestore(undefined), onApplied(r))} />}
    </div>
  );
}

/** jj op restore, previewed exactly first (POST /ops:restore dryRun). */
function RestoreDialog({ op, onClose, onApplied }: { op: OpEntry; onClose: () => void; onApplied: (r: GraphOpResult) => void }) {
  const [preview, setPreview] = useState<GraphOpResult>();
  const [err, setErr] = useState<string>();
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    post<GraphOpResult>("/ops:restore", { op: op.op, dryRun: true })
      .then(setPreview)
      .catch((e) => setErr((e as Error).message));
  }, [op.op]);
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && (e.stopPropagation(), onClose());
    window.addEventListener("keydown", k, true);
    return () => window.removeEventListener("keydown", k, true);
  }, [onClose]);
  const apply = async () => {
    setBusy(true);
    try {
      const r = await post<GraphOpResult>("/ops:restore", { op: op.op });
      if (r.status === "applied") onApplied(r);
      else setErr(r.error?.message ?? r.summary);
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="modal-back" onClick={onClose}>
      <div className="modal gp-dialog" role="dialog" aria-label="Restore the repository" onClick={(e) => e.stopPropagation()}>
        <h2>Restore to {short(op.op)}?</h2>
        <div className="small">
          Puts every change, bookmark and workspace back as they were right after <b>{op.description || short(op.op)}</b> ({ago(op.time)}). Later operations stay in the log, so this can be undone too. Workspaces whose @ moves get their files checked out.
        </div>
        <div className="gp-preview" aria-live="polite">
          {err && <div className="err small">{err}</div>}
          {!preview && !err && <div className="muted small">previewing…</div>}
          {preview && preview.status === "rejected" && <div className="warn-box small">{preview.error?.message ?? preview.summary}</div>}
          {preview && preview.status === "previewed" && (
            <div className="small">
              <div>
                <b>Preview:</b> {preview.summary}
              </div>
              {preview.bookmarks.map((b) => (
                <div key={b.name} className="mono">
                  {b.name}: {b.from ? b.from.slice(0, 8) : "(new)"} → {b.to ? b.to.slice(0, 8) : "(deleted)"}
                </div>
              ))}
              {preview.workspaces.length > 0 && <div>Updates the files of {preview.workspaces.map((w) => `${w.name}@`).join(", ")} on disk.</div>}
            </div>
          )}
        </div>
        <div className="row end">
          <button onClick={onClose}>Cancel</button>
          <button className="primary" disabled={busy || preview?.status !== "previewed"} onClick={apply}>
            {busy ? "Restoring…" : "Restore"}
          </button>
        </div>
      </div>
    </div>
  );
}
