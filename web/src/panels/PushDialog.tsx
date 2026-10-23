import { useEffect, useState } from "react";
import type { RemoteResult } from "../contracts/model";
import { post } from "./graphApi";

/**
 * jj git push -b <bookmark>: jj's dry run first (what it would push, or why it refuses: a change
 * without a description, conflicts, a remote that moved since the last fetch), then the push. A
 * push cannot be taken back from here: the remote has it.
 */
export function PushDialog({ bookmark, remotes, onClose, onDone }: { bookmark: string; remotes: string[]; onClose: () => void; onDone: (r: RemoteResult) => void }) {
  const [remote, setRemote] = useState(remotes.includes("origin") ? "origin" : remotes[0]);
  const [preview, setPreview] = useState<RemoteResult>();
  const [err, setErr] = useState<string>();
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    setPreview(undefined);
    setErr(undefined);
    let alive = true;
    post<RemoteResult>("/git:push", { bookmarks: [bookmark], remote, dryRun: true })
      .then((r) => alive && setPreview(r))
      .catch((e) => alive && setErr((e as Error).message));
    return () => {
      alive = false;
    };
  }, [bookmark, remote]);
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && (e.stopPropagation(), onClose());
    window.addEventListener("keydown", k, true);
    return () => window.removeEventListener("keydown", k, true);
  }, [onClose]);
  const push = async () => {
    setBusy(true);
    try {
      const r = await post<RemoteResult>("/git:push", { bookmarks: [bookmark], remote });
      if (r.status === "applied") onDone(r);
      else setPreview(r);
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  // jj's dry run says "Nothing changed." when the remote already has it.
  const nothing = preview?.status === "previewed" && /nothing changed/i.test(preview.output);
  return (
    <div className="modal-back" onClick={onClose}>
      <div className="modal gp-dialog" role="dialog" aria-label="Push" onClick={(e) => e.stopPropagation()}>
        <h2>Push {bookmark}</h2>
        {remotes.length > 1 && (
          <label className="field">
            <span>To</span>
            <select value={remote} onChange={(e) => setRemote(e.target.value)}>
              {remotes.map((r) => (
                <option key={r}>{r}</option>
              ))}
            </select>
          </label>
        )}
        <div className="small muted">
          jj git push -b {bookmark} --remote {remote}, with your git credentials. It is not undone by Undo: the remote keeps what it got.
        </div>
        <div className="gp-preview" aria-live="polite">
          {err && <div className="err small">{err}</div>}
          {!preview && !err && <div className="muted small">asking jj what it would push…</div>}
          {preview?.status === "failed" && <div className="warn-box small gp-out">{preview.error?.message ?? preview.summary}</div>}
          {preview?.status === "previewed" && <pre className="small gp-out">{preview.output || "(nothing to push)"}</pre>}
        </div>
        <div className="row end">
          <button onClick={onClose}>Cancel</button>
          <button className="primary" disabled={busy || preview?.status !== "previewed" || nothing} onClick={push}>
            {busy ? "Pushing…" : `Push to ${remote}`}
          </button>
        </div>
      </div>
    </div>
  );
}
