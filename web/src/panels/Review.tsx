import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags } from "@lezer/highlight";
import { bundledLang, editorLang, loadLang } from "./codeLang";
import type { BlobText } from "../contracts/http";
import type { ConflictDetail, EditMode, EditResult, Thread, ThreadKind, WindowHeader } from "../contracts/model";
import { HEADER_WU, LINE_WU } from "../contracts/wire";
import type { Engine } from "../canvas/engine";
import { COLHEAD_PX, RAIL_PX } from "../canvas/hud";
import { ApiError, editWarnings, suggestionOf, type AppliedEdit, type ReviewClient, type UndoBlocked } from "../data/review";
import { actorName } from "../canvas/presence";
import type { Live } from "../data/live";
import type { ViewSpec } from "../contracts/model";
import { useEscape } from "./useEscape";
import { outdatedNote, resolveRegions } from "./reviewLogic";

/** Where an applied suggestion goes (DESIGN §6: amend the anchored change, or insert after it). */
type SuggestionMode = Extract<EditMode, "amend" | "insert-after">;

/**
 * Pins a DOM element to a world point: it follows the camera on every view change (one transform
 * write), clamped inside the canvas. With `scale`, the element is laid out in world units and
 * scaled with the zoom (the editor); otherwise it keeps its screen size (popovers).
 */
function usePinned(engine: Engine, ref: React.RefObject<HTMLElement | null>, at: () => [number, number], scale = false, hideOffscreen = true) {
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const place = () => {
      const [x, y] = at();
      let [sx, sy] = engine.cam.toScreen(x, y);
      if (scale) {
        el.style.transform = `translate(${sx}px, ${sy}px) scale(${engine.cam.zoom})`;
        return;
      }
      const host = el.parentElement!;
      // Hidden while the anchor is off screen; clamped inside the canvas while it is near an edge.
      const off = sx < RAIL_PX - 40 || sy < COLHEAD_PX - 40 || sx > host.clientWidth + 40 || sy > host.clientHeight + 40;
      el.style.visibility = off && hideOffscreen ? "hidden" : "";
      sx = Math.max(RAIL_PX + 8, Math.min(sx, host.clientWidth - el.offsetWidth - 8));
      sy = Math.max(COLHEAD_PX + 8, Math.min(sy, host.clientHeight - el.offsetHeight - 8));
      el.style.transform = `translate(${sx}px, ${sy}px)`;
    };
    place();
    const off = engine.onView(place);
    const ro = new ResizeObserver(place);
    ro.observe(el);
    return () => {
      off();
      ro.disconnect();
    };
  });
}

function lineY(w: WindowHeader, line: number) {
  return w.rect.y + HEADER_WU + (line - w.lines.start) * LINE_WU;
}

function Popover({ engine, w, line, children, onClose, wide }: { engine: Engine; w: WindowHeader; line: number; children: ReactNode; onClose: () => void; wide?: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  usePinned(engine, ref, () => [w.rect.x + w.rect.w + 16 / engine.cam.zoom, lineY(w, line)]);
  useEscape(onClose);
  return (
    <div ref={ref} className={`pop ${wide ? "wide" : ""}`} onPointerDown={(e) => e.stopPropagation()}>
      <button className="x" onClick={onClose} aria-label="Close">
        ×
      </button>
      {children}
    </div>
  );
}

const who = (a: Thread["createdBy"]) => (a.kind === "agent" ? `${actorName(a)} (agent)` : a.kind === "human" ? a.name : "external");

/** Edit warnings (agent attached, divergent, recovered) as a list under a preview or result. */
function Warnings({ r }: { r?: EditResult }) {
  const w = editWarnings(r);
  if (!w.length) return null;
  return (
    <ul className="edit-warnings" role="alert">
      {w.map((x, i) => (
        <li key={i} className={`warn-box small w-${x.code}`}>
          <b>{x.code === "agent-attached" ? `${x.agent ? actorName({ ...x.agent, workspace: (x.agent as { workspace?: string }).workspace ?? x.workspace }) : "An agent"} is working in ${x.workspace ? `${x.workspace}@` : "an affected workspace"}` : x.code === "divergent" ? "Divergent" : x.code === "workspace-recovered" ? "Workspace recovered" : x.code}</b>
          {": "}
          {x.message}
          {x.changes?.length ? <span className="mono muted"> ({x.changes.map((c) => c.slice(0, 8)).join(", ")})</span> : null}
        </li>
      ))}
    </ul>
  );
}

/** A comment body with ```suggestion blocks shown as a replacement. */
function Body({ text }: { text: string }) {
  const parts = text.split(/```suggestion\n([\s\S]*?)\n?```/);
  return (
    <div className="body">
      {parts.map((p, i) =>
        i % 2 ? (
          <pre key={i} className="sugg">
            {p}
          </pre>
        ) : (
          p.trim() && <p key={i}>{p.trim()}</p>
        ),
      )}
    </div>
  );
}

function describeEdit(r: EditResult, mode: EditMode) {
  if (r.error) return `${r.error.code}: ${r.error.message}`;
  const target = r.rewritten[0] ?? "the change";
  const what =
    mode === "amend"
      ? `amends ${target}`
      : mode === "keep-descendants"
        ? `amends ${target}, descendants keep their content`
        : mode === "absorb"
          ? `absorbs into ${r.rewritten.length ? r.rewritten.slice(0, 3).join(", ") : "the commits that own the lines"}`
          : r.created
            ? `creates ${r.created}`
            : `creates a new change`;
  const desc = mode !== "absorb" && r.rewritten.length > 1 ? ` · rebases ${r.rewritten.length - 1} descendants` : "";
  const conf = r.conflicts.length ? ` · ${r.conflicts.length} conflict(s) ${r.predicted ? "predicted" : ""}` : r.predicted ? " · no conflicts predicted" : "";
  return `${r.status === "applied" ? "Applied: " : "Preview: "}${what}${desc}${conf}`;
}

/** A preview the server refused (status rejected or failed): nothing to apply. */
const refused = (r?: EditResult) => !!r && (r.status === "rejected" || r.status === "failed" || !!r.error);

/** stale-base, said plainly: the file moved on since this window loaded. */
const STALE_BASE = "This file changed since the window loaded (someone else edited it), so the edit no longer applies to what you saw. Reload the window to get the current text, then make the change again; your text stays here to copy.";

export function ThreadPopover({ engine, review, threadId, w, onClose }: { engine: Engine; review: ReviewClient; threadId: string; w: WindowHeader; onClose: () => void }) {
  const [, force] = useState(0);
  useEffect(() => review.onChange(() => force((n) => n + 1)), [review]);
  const t = review.threads.get(threadId);
  const [reply, setReply] = useState("");
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<EditResult>();
  // Where an applied suggestion goes (§6 table): amend the anchored change, or a new change after it.
  const [smode, setSmode] = useState<SuggestionMode>("amend");
  const [err, setErr] = useState<string>();
  // Highlight the commented lines on the canvas while the thread is open.
  const anchor = t?.anchor;
  useEffect(() => {
    if (!anchor) return;
    const clamp = (n: number) => Math.max(w.lines.start, Math.min(w.lines.end, n));
    engine.setCommentRange({ w, start: clamp(anchor.start), end: clamp(anchor.end), editable: false });
    return () => engine.setCommentRange(undefined);
  }, [engine, w, anchor?.start, anchor?.end]);
  if (!t) return null;
  const sugg = [...t.comments].reverse().map((c) => suggestionOf(c.body)).find((s) => s !== undefined);
  const open = t.status !== "resolved" && t.status !== "wontfix";
  const stale = outdatedNote(t);
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setErr(undefined);
    try {
      await f();
    } catch (e) {
      setErr(String((e as Error).message ?? e));
    } finally {
      setBusy(false);
    }
  };
  const where = `${t.anchor.path.split("/").pop()}:${t.anchor.start}`;
  return (
    <Popover engine={engine} w={w} line={t.anchor.start} onClose={onClose}>
      <header>
        <b>{t.kind}</b> <span className={`tstatus ${t.status}`}>{t.status}</span>
        {t.assignee && <span className="muted"> · for {t.assignee}</span>}
        <div className="muted mono small">
          {t.anchor.path.split("/").pop()}:{t.anchor.start}
          {t.anchor.end !== t.anchor.start ? `–${t.anchor.end}` : ""}
        </div>
      </header>
      {stale && (
        <div className="warn-box small outdated-note" data-testid="thread-outdated">
          {stale}. The lines shown may not be the ones commented on.
        </div>
      )}
      <ol className="comments">
        {t.comments.map((c) => (
          <li key={c.id}>
            <div className="by">{who(c.author)}</div>
            <Body text={c.body} />
          </li>
        ))}
      </ol>
      {sugg !== undefined && open && stale && <div className="warn-box small">The suggestion was written for lines that have since changed, so it is not offered for applying. Mark the thread “Still applies” to re-anchor it first, or reply.</div>}
      {sugg !== undefined && open && !stale && review.conflicts.has(t.anchor.path) && (
        <div className="warn-box small">This file has a conflict at this change; resolve it before applying suggestions.</div>
      )}
      {sugg !== undefined && open && !stale && !review.conflicts.has(t.anchor.path) && (
        <div className="sugg-bar">
          {!preview ? (
            <>
              <button
                disabled={busy}
                onClick={() =>
                  run(async () => setPreview(await review.edit(w, t.anchor.start, t.anchor.end, sugg, smode, true, smode === "insert-after" ? `Suggestion on ${where}` : undefined, t.id)))
                }
              >
                Preview suggestion
              </button>
              <select aria-label="Where the suggestion goes" data-testid="suggestion-mode" value={smode} onChange={(e) => setSmode(e.target.value as SuggestionMode)}>
                <option value="amend">amend {t.anchor.change.slice(0, 8)}</option>
                <option value="insert-after">new change after it</option>
              </select>
            </>
          ) : refused(preview) ? (
            <>
              <span className="small err">{preview.error?.code === "stale-base" ? STALE_BASE : describeEdit(preview, smode)}</span>
              <button className="linkish" onClick={() => setPreview(undefined)}>
                Back
              </button>
            </>
          ) : (
            <>
              <span className="small">{describeEdit(preview, smode)}</span>
              <Warnings r={preview} />
              <button
                className="primary"
                data-testid="suggestion-apply"
                disabled={busy}
                onClick={() =>
                  run(async () => {
                    // No single request applies an edit and answers its thread (POST /edits/:id/apply
                    // has no body), so: apply, and only once it applied, resolve the thread.
                    const r = await review.apply(preview.id, `suggestion on ${where}`, [w.path]);
                    if (r.status !== "applied") {
                      setPreview(r);
                      throw new Error(r.error ? `${r.error.code}: ${r.error.message}` : `the edit was ${r.status}, not applied`);
                    }
                    engine.refreshFile(w.path);
                    await review.reply(t.id, smode === "insert-after" ? `Applied the suggestion as a new change${r.created ? ` (${r.created.slice(0, 8)})` : ""}.` : "Applied the suggestion.", "resolved");
                    setPreview(undefined);
                  })
                }
              >
                {smode === "insert-after" ? "Apply (new change)" : "Apply (amend)"}
              </button>
              <button onClick={() => setPreview(undefined)}>Cancel</button>
            </>
          )}
        </div>
      )}
      <textarea placeholder={open ? "Reply… (```suggestion blocks propose a replacement)" : "Reply to reopen…"} value={reply} onChange={(e) => setReply(e.target.value)} rows={3} />
      {err && <div className="err">{err}</div>}
      <div className="row">
        <button className="primary" disabled={busy || !reply.trim()} onClick={() => run(async () => (await review.reply(t.id, reply, open ? undefined : "open"), setReply("")))}>
          Reply
        </button>
        {open ? (
          <button disabled={busy} onClick={() => run(() => review.reply(t.id, reply, "resolved").then(() => setReply("")))}>
            Resolve
          </button>
        ) : (
          <button disabled={busy} onClick={() => run(() => review.setStatus(t.id, "open"))}>
            Reopen
          </button>
        )}
        {open && t.assignee !== "agent" && (
          <button disabled={busy} onClick={() => run(() => review.setStatus(t.id, "open", "agent"))} title="Assign to the agent bound to this workspace">
            Send to agent
          </button>
        )}
      </div>
      {open && (
        <div className="row small status-actions">
          {stale && (
            <button disabled={busy} data-testid="thread-still-applies" title="The comment still applies: back to open (the server re-anchors it on the current commit)" onClick={() => run(() => review.setStatus(t.id, "open"))}>
              Still applies
            </button>
          )}
          {t.status !== "in-progress" && (
            <button disabled={busy} data-testid="thread-in-progress" title="Someone is working on it" onClick={() => run(() => review.reply(t.id, reply, "in-progress").then(() => setReply("")))}>
              In progress
            </button>
          )}
          <button disabled={busy} data-testid="thread-wontfix" title="Close without a change (the reply, if any, says why)" onClick={() => run(() => review.reply(t.id, reply, "wontfix").then(() => setReply("")))}>
            Won't fix
          </button>
        </div>
      )}
    </Popover>
  );
}

const KINDS: ThreadKind[] = ["comment", "suggestion", "question", "task"];

export function Composer({ engine, review, w, line: first, end: last, onClose, onCreated }: { engine: Engine; review: ReviewClient; w: WindowHeader; line: number; end?: number; onClose: () => void; onCreated: (t: Thread) => void }) {
  const [kind, setKind] = useState<ThreadKind>("comment");
  const [body, setBody] = useState("");
  // The range is shared with the canvas: its handles drag it, the inputs here type it.
  const [[line, end], setRange] = useState<[number, number]>([first, last ?? first]);
  const clamp = (n: number) => Math.max(w.lines.start, Math.min(w.lines.end, n || w.lines.start));
  const setLines = (a: number, b: number) => setRange([Math.min(clamp(a), clamp(b)), Math.max(clamp(a), clamp(b))]);
  useEffect(() => {
    engine.onCommentRange = (r) => setRange([r.start, r.end]);
    return () => {
      engine.onCommentRange = undefined;
      engine.setCommentRange(undefined);
    };
  }, [engine]);
  useEffect(() => {
    const cur = engine.commentRange;
    if (cur?.w.id !== w.id || cur.start !== line || cur.end !== end || !cur.editable) engine.setCommentRange({ w, start: line, end, editable: true });
  }, [engine, w, line, end]);
  const [err, setErr] = useState<string>();
  const current = () => {
    const t = engine.client.texts.get(engine.client.textKey(w));
    if (!t) return "";
    return t.lines.slice(line - t.from, end - t.from + 1).join("\n");
  };
  const pick = (k: ThreadKind) => {
    setKind(k);
    if (k === "suggestion" && !body.includes("```suggestion")) setBody(`${body}${body ? "\n" : ""}\`\`\`suggestion\n${current()}\n\`\`\``);
  };
  const create = async (assignee: "human" | "agent") => {
    try {
      const t = await review.create({ anchor: { path: w.path, rev: w.change, start: line, end }, body, kind: assignee === "agent" && kind === "comment" ? "task" : kind, assignee });
      onCreated(t);
    } catch (e) {
      setErr(String((e as Error).message));
    }
  };
  return (
    <Popover engine={engine} w={w} line={line} onClose={onClose}>
      <header>
        <b>New thread</b>
        <div className="muted mono small">
          {w.path.split("/").pop()}:
          <input className="num" type="number" aria-label="first line" min={w.lines.start} max={end} value={line} onChange={(e) => setLines(Number(e.target.value), end)} />–
          <input className="num" type="number" aria-label="last line" min={line} max={w.lines.end} value={end} onChange={(e) => setLines(line, Number(e.target.value))} />
          <span className="hint">{end > line ? `${end - line + 1} lines` : "drag the handles to widen"}</span>
        </div>
      </header>
      <div className="seg small">
        {KINDS.map((k) => (
          <button key={k} className={k === kind ? "on" : ""} onClick={() => pick(k)}>
            {k}
          </button>
        ))}
      </div>
      <textarea autoFocus rows={5} value={body} onChange={(e) => setBody(e.target.value)} placeholder="Leave a comment… ⌘↵ to post" onKeyDown={(e) => e.key === "Enter" && (e.metaKey || e.ctrlKey) && body.trim() && create("human")} />
      {err && <div className="err">{err}</div>}
      <div className="row">
        <button className="primary" disabled={!body.trim()} onClick={() => create("human")}>
          Comment
        </button>
        <button disabled={!body.trim()} onClick={() => create("agent")} title="Creates a task assigned to the agent">
          Ask agent
        </button>
      </div>
    </Popover>
  );
}

/** Editor highlighting in the canvas's token colours, so activating a window changes nothing visually. */
function codeStyle(engine: Engine) {
  const t = engine.theme.token;
  const c = (n: number) => "#" + (t[n] ?? engine.theme.ink).toString(16).padStart(6, "0");
  return HighlightStyle.define([
    { tag: [tags.keyword, tags.controlKeyword, tags.definitionKeyword, tags.moduleKeyword, tags.operatorKeyword], color: c(1) },
    { tag: [tags.string, tags.special(tags.string)], color: c(2) },
    { tag: [tags.comment, tags.lineComment, tags.blockComment], color: c(3), fontStyle: "italic" },
    { tag: [tags.number, tags.bool, tags.null], color: c(4) },
    { tag: [tags.function(tags.variableName), tags.function(tags.propertyName)], color: c(5) },
    { tag: [tags.typeName, tags.className], color: c(6) },
    { tag: [tags.meta, tags.annotation], color: c(10) },
    { tag: [tags.propertyName], color: c(12) },
  ]);
}

/** Where an edit goes (D10, §6 table): amend by default; immutable targets branch; range views absorb. */
const MODES: Array<[EditMode, string]> = [
  ["amend", "Amend"],
  ["insert-after", "New change after"],
  ["branch", "Branch"],
  ["keep-descendants", "Amend, keep descendants' content"],
];
/** Range view (tip): absorb into the commits that own the lines; or amend one commit; or a new change on top. */
const RANGE_MODES: Array<[EditMode | "amend-chosen", string]> = [
  ["absorb", "Absorb into owning commits"],
  ["amend-chosen", "Amend a chosen commit"],
  ["insert-after", "New change on top"],
];

/**
 * F3 activation: CodeMirror over the window's code area, scaled with the canvas, and a save bar that
 * defaults to amending the change that owns the lines (absorbing, in a range view). Preview first
 * (exact on the real server), then apply; the window's text reloads afterwards and the undo bar
 * offers to revert it.
 */
export function Editor({ engine, review, w, onClose }: { engine: Engine; review: ReviewClient; w: WindowHeader; onClose: () => void }) {
  const box = useRef<HTMLDivElement>(null);
  const bar = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView>(undefined);
  const range = engine.isRange;
  const spec = engine.client.view.spec as ViewSpec;
  const tip = engine.client.view.resolved.head;
  const commits = engine.client.view.resolved.commits;
  const [text, setText] = useState<BlobText>();
  const [mode, setMode] = useState<EditMode | "amend-chosen">(range ? "absorb" : "amend");
  const [chosen, setChosen] = useState<string>(commits[0] ?? tip);
  const [message, setMessage] = useState("");
  const [preview, setPreview] = useState<EditResult>();
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [dirty, setDirty] = useState(false);
  // Immutable targets are never rewritten (D10): the only mode is a new branch on top.
  const [immutable, setImmutable] = useState(false);
  useEffect(() => {
    if (range) return;
    fetch(`/api/v1/revisions?revset=${encodeURIComponent(w.change)}&limit=1`)
      .then((r) => (r.ok ? r.json() : undefined))
      .then((r) => {
        const row = r?.rows?.find((x: { rev: { change: string } }) => x.rev.change.startsWith(w.change));
        if (row?.rev.flags.immutable) {
          setImmutable(true);
          setMode("branch");
        }
      })
      .catch(() => {});
  }, [w, range]);
  // The editor is at most a screenful tall, so a long window scrolls inside it (wheel events stay
  // in the editor) instead of running off the canvas. It opens over the part of the window in
  // view, scrolled so its lines sit on the canvas's.
  const [frame] = useState(() => {
    const code = w.rect.h - HEADER_WU;
    const top = w.rect.y + HEADER_WU;
    const zoom = engine.cam.zoom;
    const shown = (engine.cam.height - COLHEAD_PX - 96) / zoom;
    const h = Math.max(Math.min(code, 6 * LINE_WU), Math.min(code, Math.floor(shown / LINE_WU) * LINE_WU));
    const viewTop = engine.cam.toWorld(0, COLHEAD_PX + 16)[1];
    const y = Math.max(top, Math.min(top + code - h, top + Math.round((viewTop - top) / LINE_WU) * LINE_WU));
    return { y, h, scroll: y - top };
  });
  usePinned(engine, box, () => [w.rect.x, frame.y], true);
  // The save bar stays on screen (clamped) even when the editor's bottom edge is not.
  usePinned(engine, bar, () => [w.rect.x, frame.y + frame.h + 12 / engine.cam.zoom], false, false);

  useEffect(() => {
    const cached = engine.client.texts.get(engine.client.textKey(w));
    if (cached) setText(cached);
    else
      fetch(`/api/v1/blobs/${w.blob}/text?from=${w.lines.start}&to=${w.lines.end}`)
        .then((r) => r.json())
        .then(setText)
        .catch((e) => setErr(String(e)));
  }, [engine, w]);

  useEffect(() => {
    if (!text || !box.current) return;
    // Every pack's language highlights (codeLang.ts); grammars not bundled load into the compartment.
    const l = editorLang(w.lang, w.path);
    const langNow = l ? bundledLang(l) : undefined;
    const lang = new Compartment();
    const v = new EditorView({
      parent: box.current,
      state: EditorState.create({
        doc: text.lines.join("\n"),
        // The cursor starts on the first line in view (focus would scroll back to line 1).
        selection: { anchor: Math.min(text.lines.slice(0, Math.round(frame.scroll / LINE_WU)).reduce((n, l) => n + l.length + 1, 0), text.lines.join("\n").length) },
        extensions: [
          lineNumbers({ formatNumber: (n) => String(n + text.from - 1) }),
          history(),
          highlightActiveLine(),
          syntaxHighlighting(codeStyle(engine)),
          keymap.of([...defaultKeymap, ...historyKeymap]),
          lang.of(langNow ?? []),
          EditorView.updateListener.of((u) => {
            if (u.docChanged) {
              setDirty(true);
              setPreview(undefined);
            }
          }),
        ],
      }),
    });
    view.current = v;
    v.focus();
    v.scrollDOM.scrollTop = frame.scroll;
    let live = true;
    if (l && !langNow)
      loadLang(l)
        .then((ext) => live && v.dispatch({ effects: lang.reconfigure(ext) }))
        .catch(() => {});
    return () => {
      live = false;
      v.destroy();
    };
  }, [text, w]);

  useEscape(onClose);

  const content = () => view.current?.state.doc.toString() ?? "";
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setErr(undefined);
    try {
      await f();
    } catch (e) {
      setErr(e instanceof ApiError && e.code === "stale-base" ? STALE_BASE : String((e as Error).message ?? e));
    } finally {
      setBusy(false);
    }
  };
  const start = text?.from ?? w.lines.start;
  const end = text?.to ?? w.lines.end;
  const editMode: EditMode = mode === "amend-chosen" ? "amend" : mode;
  const opts = () =>
    !range ? undefined : mode === "absorb" ? { target: tip, absorbInto: spec.mode === "range" ? spec.revset : undefined } : mode === "amend-chosen" ? { target: chosen } : { target: tip };
  const doPreview = (m: EditMode = editMode) =>
    run(async () => {
      setNotice(undefined);
      let r = await review.edit(w, start, end, content(), m, true, message || undefined, undefined, opts());
      // The HTTP route refuses immutable targets; do what the MCP side does and branch instead (D10).
      if (r.error?.code === "immutable" && m !== "branch") {
        setImmutable(true);
        setMode("branch");
        setNotice(`${w.change.slice(0, 8)} is immutable, so nothing is rewritten there: switched to a new branch on top.`);
        r = await review.edit(w, start, end, content(), "branch", true, message || undefined, undefined, opts());
      }
      setPreview(r);
    });
  const modes = range ? RANGE_MODES : MODES.filter(([m]) => !immutable || m === "branch");
  return (
    <>
      <div ref={box} className="cm-host" style={{ width: w.rect.w, height: frame.h }} onPointerDown={(e) => e.stopPropagation()} onWheel={(e) => e.stopPropagation()} />
      <div ref={bar} className="savebar" onPointerDown={(e) => e.stopPropagation()}>
        <b>Editing</b>
        <span className="muted mono small">
          {w.path.split("/").pop()}:{start}–{end} @ {range ? `range tip ${tip.slice(0, 8)}` : w.change}
        </span>
        <select aria-label="Where the edit goes" value={mode} onChange={(e) => (setMode(e.target.value as EditMode), setPreview(undefined))}>
          {modes.map(([m, l]) => (
            <option key={m} value={m}>
              {l}
            </option>
          ))}
        </select>
        {range && mode === "amend-chosen" && (
          <select aria-label="Commit to amend" value={chosen} onChange={(e) => (setChosen(e.target.value), setPreview(undefined))}>
            {commits.map((c) => (
              <option key={c} value={c}>
                {c.slice(0, 12)}
              </option>
            ))}
          </select>
        )}
        {immutable && <span className="small warn-inline">immutable: changes go on a new branch</span>}
        {(mode === "insert-after" || mode === "branch") && <input placeholder="Description" value={message} onChange={(e) => setMessage(e.target.value)} />}
        {!preview || refused(preview) ? (
          <button disabled={busy || !dirty} onClick={() => doPreview()}>
            Preview
          </button>
        ) : (
          <button
            className="primary"
            disabled={busy}
            onClick={() =>
              run(async () => {
                await review.apply(preview.id, `${w.path.split("/").pop()}:${start}–${end}`, [w.path]);
                engine.refreshFile(w.path);
                onClose();
              })
            }
          >
            Apply
          </button>
        )}
        <button onClick={onClose}>Cancel</button>
        {notice && <span className="small warn-inline">{notice}</span>}
        {preview && !refused(preview) && <span className="small">{describeEdit(preview, editMode)}</span>}
        {preview && <Warnings r={preview} />}
        {preview && refused(preview) && (
          <span className="err small" role="alert">
            {preview.error?.code === "stale-base" ? STALE_BASE : describeEdit(preview, editMode)}
            {preview.error?.code === "stale-base" && (
              <button className="linkish" onClick={() => (engine.refreshFile(w.path), onClose())}>
                Reload the window
              </button>
            )}
          </span>
        )}
        {err && <span className="err">{err}</span>}
      </div>
    </>
  );
}

/**
 * After an edit is applied from this tab: "Applied … · Undo" at the foot of the canvas. Undo is
 * POST /edits/:id/undo; when the server refuses (the change moved on, or undoing would leave it
 * divergent) its reason is shown instead.
 */
export function UndoBar({ engine, review }: { engine: Engine; review: ReviewClient }) {
  const [a, setA] = useState<AppliedEdit | undefined>(review.lastApplied);
  const [state, setState] = useState<{ busy?: boolean; err?: string; done?: string; blocked?: UndoBlocked; locked?: boolean; result?: EditResult }>({});
  useEffect(
    () =>
      review.onApplied((x) => {
        if (x) {
          setA(x);
          setState({});
        }
      }),
    [review],
  );
  useEffect(() => {
    if (!a || state.busy) return;
    const t = setTimeout(() => setA(undefined), state.err ? 45_000 : state.done ? 6000 : 25_000);
    return () => clearTimeout(t);
  }, [a, state]);
  if (!a) return null;
  const undo = async () => {
    setState({ busy: true });
    try {
      const r = await review.undo(a.result.id);
      for (const p of a.paths) engine.refreshFile(p);
      setState({ done: "Undone.", result: r });
    } catch (e) {
      const x = e instanceof ApiError ? e : undefined;
      // 409 locked: another mutation holds the queue; 409 undo-blocked: later operations depend on it.
      if (x?.code === "locked") setState({ locked: true, err: "the server is busy with another change; try again in a moment" });
      else if (x?.code === "undo-blocked") setState({ err: `undo-blocked: ${x.message}`, blocked: x.details as unknown as UndoBlocked });
      else setState({ err: `${x?.code ? x.code + ": " : ""}${(e as Error).message}` });
    }
  };
  const ops = (state.blocked?.laterOps as UndoBlocked["laterOps"] | undefined) ?? [];
  return (
    <div className={`undobar ${state.blocked || editWarnings(a.result).length ? "tall" : ""}`} role="status">
      <span>
        Applied <b>{a.label}</b>
        {a.result.rewritten.length ? <span className="muted mono small"> · {a.result.rewritten.slice(0, 2).join(", ")}</span> : null}
      </span>
      {state.done ? <span className="add">{state.done}</span> : <button disabled={state.busy} onClick={undo}>{state.locked ? "Retry undo" : "Undo"}</button>}
      {state.err && (
        <span className="err" role="alert">
          Undo refused: {state.err}
        </span>
      )}
      <button className="x" aria-label="Dismiss" onClick={() => setA(undefined)}>
        ×
      </button>
      {state.blocked && (
        <div className="undo-blocked small">
          {ops.length > 0 && (
            <>
              <div className="muted">Operations since this edit:</div>
              <ol>
                {ops.slice(0, 6).map((o) => (
                  <li key={o.op}>
                    <span className="mono">{o.op.slice(0, 10)}</span> {o.description}
                    {o.user ? <span className="muted"> · {o.user}</span> : null}
                    {o.changes?.length ? <span className="mono muted"> · {o.changes.map((c) => c.slice(0, 8)).join(", ")}</span> : null}
                  </li>
                ))}
              </ol>
            </>
          )}
          {state.blocked.suggestion && <div className="accent">{state.blocked.suggestion}</div>}
        </div>
      )}
      {!state.blocked && <Warnings r={state.result ?? a.result} />}
    </div>
  );
}

/**
 * A conflict: every conflicted region, one at a time (‹ › steps between them), each side with its
 * change label (and the base). Each region is settled by taking a side or editing a merge; the file
 * is resolved (one whole-content POST, http.ts) once every region has a choice. "Take side k
 * everywhere" settles all regions at once. Or ask the agent bound to the workspace to resolve it.
 */
export function ConflictPopover({ engine, review, live, d, w, onClose, onThread }: { engine: Engine; review: ReviewClient; live?: Live; d: ConflictDetail; w: WindowHeader; onClose: () => void; onThread: (t: Thread) => void }) {
  const n = d.regions.length;
  const [cur, setCur] = useState(0);
  const [choices, setChoices] = useState<Array<string | undefined>>(() => d.regions.map(() => undefined));
  const r = d.regions[Math.min(cur, Math.max(0, n - 1))];
  const [merge, setMerge] = useState(r?.sides[0]?.text ?? "");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string>();
  // "Ask agent": the task thread sent with reviews:submit, then what the agent did with it.
  const [asked, setAsked] = useState<string>();
  const [, force] = useState(0);
  const [resolution, setResolution] = useState<{ by?: string; text?: string }>();
  const first = d.regions[0];
  const last = d.regions[n - 1];
  useEffect(
    () =>
      review.onChange(() => {
        force((n) => n + 1);
        // conflicts.changed reloads the list (App): gone from it means resolved, whoever did it.
        if (asked && !review.conflicts.has(d.path)) setResolution((x) => x ?? {});
      }),
    [review, asked, d.path],
  );
  useEffect(() => {
    if (!asked || !live) return;
    return live.on((e) => {
      if (e.type === "conflicts.changed" && e.resolved.some((c) => c.path === d.path)) setResolution((x) => x ?? {});
      if (e.type === "edit.updated" && e.edit.status === "applied") {
        const f = e.edit.files?.find((f) => f.path === d.path);
        if (!f) return;
        const by = e.edit.actor.kind === "agent" ? e.edit.actor.name : undefined;
        setResolution((x) => ({ ...x, by }));
        if (f.newBlob && first && last) {
          const n = last.end - first.start + Math.max(...last.sides.map((s) => s.text.split("\n").length)) + 2;
          fetch(`/api/v1/blobs/${f.newBlob}/text?from=${first.start}&to=${first.start + n}`)
            .then((x) => (x.ok ? x.json() : undefined))
            .then((b: BlobText | undefined) => b && setResolution((x) => ({ ...x, by, text: b.lines.join("\n") })))
            .catch(() => {});
        }
      }
    });
  }, [asked, live, d.path, first, last]);
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setErr(undefined);
    try {
      await f();
    } catch (e) {
      setErr(String((e as Error).message ?? e));
    } finally {
      setBusy(false);
    }
  };
  const resolveAll = (cs: Array<string | undefined>) =>
    run(async () => {
      await review.resolveConflict(d, resolveRegions(d.materialized, d.regions, cs));
      engine.clearConflicts(d.path);
      engine.refreshFile(d.path);
      onClose();
    });
  const go = (k: number) => {
    setCur(k);
    setMerge(choices[k] ?? d.regions[k]?.sides[0]?.text ?? "");
  };
  /** Settle the current region; resolve the file when that was the last open one, else go to the next. */
  const choose = (text: string) => {
    const cs = choices.map((c, k) => (k === cur ? text : c));
    setChoices(cs);
    const next = cs.findIndex((c, k) => c === undefined && k !== cur);
    if (next < 0) resolveAll(cs);
    else {
      setCur(next);
      setMerge(d.regions[next]?.sides[0]?.text ?? "");
    }
  };
  if (!r) return null;
  const done = choices.filter((c) => c !== undefined).length;
  const sideCount = Math.max(...d.regions.map((x) => x.sides.length));
  const thread = asked ? review.threads.get(asked) : undefined;
  const replies = thread?.comments.filter((c) => c.author.kind === "agent") ?? [];
  return (
    <Popover engine={engine} w={w} line={r.start} onClose={onClose} wide>
      <header>
        <b className="bad">Conflict</b>{" "}
        <span className="muted">
          · {d.sides} sides{n > 1 ? ` · ${n} regions` : ""} · lines {r.start}–{r.end}
        </span>
        <div className="muted mono small">{d.path}</div>
      </header>
      {n > 1 && !asked && (
        <div className="row small region-nav" data-testid="conflict-regions">
          <button disabled={cur === 0} aria-label="Previous region" onClick={() => go(cur - 1)}>
            ‹
          </button>
          <span data-testid="conflict-region-index">
            Region {cur + 1} of {n}
            {choices[cur] !== undefined ? " · chosen" : ""} · {done}/{n} settled
          </span>
          <button disabled={cur === n - 1} aria-label="Next region" onClick={() => go(cur + 1)}>
            ›
          </button>
          <div className="grow" />
          {Array.from({ length: sideCount }, (_, k) => (
            <button
              key={k}
              disabled={busy}
              data-testid={`conflict-take-all-${k}`}
              title={`Resolve every region with side ${k + 1} (${d.regions[0]?.sides[k]?.label ?? ""})`}
              onClick={() => resolveAll(d.regions.map((x) => x.sides[k]?.text ?? x.sides[0]?.text ?? ""))}
            >
              Side {k + 1} everywhere
            </button>
          ))}
        </div>
      )}
      {r.base !== undefined && (
        <details>
          <summary className="small muted">base</summary>
          <pre className="side base">{r.base}</pre>
        </details>
      )}
      {r.sides.map((s, i) => (
        <div key={i} className="side-box">
          <div className="row">
            <span className="mono small">{s.label}</span>
            <div className="grow" />
            <button disabled={busy} onClick={() => choose(s.text)}>
              Take this side
            </button>
            <button disabled={busy} onClick={() => setMerge(s.text)}>
              Edit from this
            </button>
          </div>
          <pre className="side">{s.text}</pre>
        </div>
      ))}
      {!asked && <textarea rows={Math.min(10, merge.split("\n").length + 1)} value={merge} onChange={(e) => setMerge(e.target.value)} />}
      {asked && (
        <div className="agent-res" aria-live="polite">
          <div className="small">
            {resolution ? <b className="add">Resolved{resolution.by ? ` by ${resolution.by}` : ""}</b> : <span className="muted">Sent to the agent (reviews:submit) · waiting for its resolution…</span>}
          </div>
          {replies.map((c) => (
            <div key={c.id} className="small">
              <span className="by">{who(c.author)}:</span> {c.body.split("\n")[0]}
            </div>
          ))}
          {resolution?.text !== undefined && (
            <>
              <div className="small muted">Agent's resolution (lines {first?.start ?? r.start}–)</div>
              <pre className="side resolved">{resolution.text}</pre>
            </>
          )}
          {thread && (
            <button className="linkish" onClick={() => onThread(thread)}>
              Open the task thread
            </button>
          )}
        </div>
      )}
      {err && <div className="err">{err}</div>}
      <div className="row">
        {!asked && (
          <button className="primary" disabled={busy} onClick={() => choose(merge)}>
            {n > 1 && choices.some((c, k) => c === undefined && k !== cur) ? `Use this merge for region ${cur + 1}` : "Resolve with this merge"}
          </button>
        )}
        <button
          disabled={busy || !!asked}
          title="Files a task thread and sends it to the agent bound to the workspace (reviews:submit)"
          onClick={() =>
            run(async () => {
              const t = await review.askAgent(
                {
                  anchor: { path: d.path, rev: d.change, start: first?.start ?? r.start, end: last?.end ?? r.end },
                  body: n > 1 ? `Please resolve the ${n} conflicted regions in this file, keeping the intent of both sides.` : "Please resolve this conflict, keeping the intent of both sides.",
                  kind: "task",
                },
                `Resolve the conflict in ${d.path}`,
              );
              setAsked(t.id);
            })
          }
        >
          {asked ? "Asked the agent" : "Ask agent to resolve"}
        </button>
      </div>
    </Popover>
  );
}
