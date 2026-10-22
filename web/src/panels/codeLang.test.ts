import { describe, expect, it } from "vitest";
import { EditorState } from "@codemirror/state";
import { ensureSyntaxTree, language } from "@codemirror/language";
import { classHighlighter, highlightTree } from "@lezer/highlight";
import { editorLang, loadLang, type EditorLang } from "./codeLang";

// Every `lang` the packs emit (cv-lang-* `LanguagePack::lang`), with a path and a line of code.
const SAMPLES: Array<[string, string, string, EditorLang]> = [
  ["python", "src/a.py", "def f(x):\n    return 1", "python"],
  ["typescript", "src/a.ts", "const x: number = 1;", "typescript"],
  ["tsx", "src/a.tsx", "const x = <div>{1}</div>;", "tsx"],
  ["javascript", "src/a.js", "function f() { return 1; }", "javascript"],
  ["rust", "src/lib.rs", "fn main() { let x = 1; }", "rust"],
  ["c", "src/a.c", "int main(void) { return 0; }", "cpp"],
  ["cpp", "src/a.cpp", "int main() { return 0; }", "cpp"],
  ["yaml", "conf/a.yaml", "key: 1\nlist:\n  - a", "yaml"],
  ["toml", "Cargo.toml", "[package]\nname = \"x\"", "toml"],
  ["json", "package.json", "{\"a\": 1}", "json"],
  ["markdown", "README.md", "# Title\n\nSome *text*.", "markdown"],
  ["other", "Dockerfile", "FROM python:3.12\nRUN echo hi", "dockerfile"],
  ["other", "scripts/run.sh", "if [ -n \"$X\" ]; then echo hi; fi", "shell"],
  ["other", "setup.cfg", "[metadata]\nname = x", "properties"],
];

describe("editor languages (every pack's languages highlight)", () => {
  it("maps each pack language to an editor language", () => {
    for (const [lang, path, , want] of SAMPLES) expect(editorLang(lang, path), `${lang} ${path}`).toBe(want);
    expect(editorLang("other", "data/blob.bin")).toBeUndefined();
  });

  it("loads a grammar for each and highlights its sample", async () => {
    for (const [lang, path, text, want] of SAMPLES) {
      const ext = await loadLang(editorLang(lang, path)!);
      const state = EditorState.create({ doc: text, extensions: [ext] });
      expect(state.facet(language), want).toBeTruthy();
      // Tokens get highlight classes (the grammar parsed and tagged them).
      const tree = ensureSyntaxTree(state, state.doc.length, 5000)!;
      let tagged = 0;
      highlightTree(tree, classHighlighter, () => void tagged++);
      expect(tagged, want).toBeGreaterThan(0);
    }
  });
});
