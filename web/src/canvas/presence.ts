import { Container } from "pixi.js";
import type { WindowHeader } from "../contracts/model";
import { LabelBatch, sansFont, textWidth } from "./labels";
import { RectBatch, dispose } from "./rects";
import type { Theme } from "./theme";

export interface Attention {
  agent: string;
  workspace?: string;
  tool: string;
  windows: string[];
  note?: string;
  at: number;
}

const FADE_MS = 12_000;

/** An Actor as shown to people: "claude@gym" for an agent in a workspace (Actor.workspace, additive). */
export function actorName(a: { kind: string; name?: string; workspace?: string }) {
  if (a.kind === "agent") return presenceName({ agent: a.name ?? "agent", workspace: a.workspace });
  return a.name ?? a.kind;
}

/** "claude@gym" when the agent works in a named workspace (§7 agent binding), else its name. */
export function presenceName(a: { agent: string; workspace?: string }) {
  return a.workspace ? `${a.agent}@${a.workspace}` : a.agent;
}

/**
 * Where agents are looking (agent.attention): an outline in the agent colour around each window it
 * touches and a "name · tool" tag above it, fading out over 12 s. The human always sees what the
 * agent is working on.
 */
export class Presence {
  readonly root = new Container();
  private latest = new Map<string, Attention>();

  add(a: Attention) {
    this.latest.set(a.agent, a);
  }

  /** Agents seen recently, newest first. */
  get active() {
    return [...this.latest.values()].sort((a, b) => b.at - a.at);
  }

  /** True while something is still fading (the engine keeps redrawing). */
  build(now: number, windowOf: (id: string) => WindowHeader | undefined, theme: Theme, wuPerPx: number): boolean {
    for (const ch of this.root.removeChildren()) dispose(ch);
    const rb = new RectBatch(64);
    const labels = new LabelBatch(sansFont());
    const font = sansFont();
    let fading = false;
    for (const a of this.latest.values()) {
      const age = now - a.at;
      if (age > FADE_MS) continue;
      fading = true;
      const alpha = age < FADE_MS / 2 ? 1 : 1 - (age - FADE_MS / 2) / (FADE_MS / 2);
      for (const id of a.windows) {
        const w = windowOf(id);
        if (!w) continue;
        const bw = 2.5 * wuPerPx;
        const gap = 6 * wuPerPx;
        rb.frame(w.rect.x - gap, w.rect.y - gap, w.rect.w + 2 * gap, w.rect.h + 2 * gap, bw, theme.thread, alpha);
        const size = 13 * wuPerPx;
        const text = `${presenceName(a)} · ${a.tool}`;
        const tw = textWidth(font, text, size) + size;
        rb.add(w.rect.x - gap, w.rect.y - gap - size * 1.6, tw, size * 1.5, theme.thread, alpha);
        labels.add(text, w.rect.x - gap + size * 0.5, w.rect.y - gap - size * 1.45, size, theme.bg);
      }
    }
    const a = new Container();
    rb.build(a);
    const b = new Container();
    labels.build(b);
    this.root.addChild(a, b);
    return fading;
  }
}
