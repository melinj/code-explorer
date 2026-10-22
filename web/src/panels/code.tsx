import { useEffect, useState, type ReactNode } from "react";
import type { BlobText } from "../contracts/http";
import type { WindowHeader } from "../contracts/model";
import type { Engine } from "../canvas/engine";
import { hex } from "../canvas/theme";

/** Blob text by blob:from:to, shared by the side panels (a window's text is asked for once). */
const cache = new Map<string, Promise<BlobText | undefined>>();

function blobText(blob: string, from: number, to: number) {
  const k = `${blob}:${from}:${to}`;
  let p = cache.get(k);
  if (!p) {
    p = fetch(`/api/v1/blobs/${blob}/text?from=${from}&to=${to}`)
      .then((r) => (r.ok ? (r.json() as Promise<BlobText>) : undefined))
      .catch(() => undefined);
    cache.set(k, p);
    if (cache.size > 400) cache.delete(cache.keys().next().value!);
  }
  return p;
}

/** Lines `from`–`to` of a blob (the canvas's loaded text when it covers them, else fetched). */
export function useText(engine: Engine, w: WindowHeader | undefined, from: number, to: number): BlobText | undefined {
  const loaded = w && engine.client.texts.get(engine.client.textKey(w));
  const covers = loaded && loaded.from <= from && loaded.to >= to;
  const [text, setText] = useState<BlobText | undefined>(covers ? loaded : undefined);
  useEffect(() => {
    if (!w || covers) return;
    let live = true;
    blobText(w.blob, from, to).then((t) => live && setText(t));
    return () => void (live = false);
  }, [w?.blob, from, to, covers]);
  return covers ? loaded : text;
}

/** One source line with the canvas's token colours (BlobText tokens: [col, len, class] triples). */
export function CodeLine({ engine, text, n, trim = false, dedent = 0 }: { engine: Engine; text: BlobText; n: number; trim?: boolean; dedent?: number }) {
  const i = n - text.from;
  const s = text.lines[i];
  if (s === undefined) return null;
  const t = text.tokens?.[i] ?? [];
  const tok = engine.theme.token;
  const parts: ReactNode[] = [];
  // `trim`: drop the indentation (one-line snippets in lists).
  // `dedent`: drop that much common indentation (previews in a narrow panel).
  const lead = s.length - s.trimStart().length;
  let at = trim ? lead : Math.min(dedent, lead);
  for (let k = 0; k + 2 < t.length; k += 3) {
    const [c, len, cls] = [t[k], t[k + 1], t[k + 2]];
    if (c + len <= at) continue;
    if (c > at) parts.push(s.slice(at, c));
    parts.push(
      <span key={k} style={{ color: tok[cls] !== undefined ? hex(tok[cls]) : undefined }}>
        {s.slice(c, c + len)}
      </span>,
    );
    at = c + len;
  }
  parts.push(s.slice(at));
  return <>{parts}</>;
}

/** A line of the selected window that a link leaves from (or arrives at): number and code. */
export function LocalLine({ engine, self, line, more = 0 }: { engine: Engine; self: WindowHeader; line: number; more?: number }) {
  const text = useText(engine, self, self.lines.start, self.lines.end);
  return (
    <span className="lk-local mono" title="The line in the selected window this link belongs to">
      <span className="ln">L{line}</span>
      <span className="src">{text ? <CodeLine engine={engine} text={text} n={line} trim /> : null}</span>
      {more > 0 && <span className="more">+{more}</span>}
    </span>
  );
}
