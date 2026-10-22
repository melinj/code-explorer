import type { Extension } from "@codemirror/state";
import { python } from "@codemirror/lang-python";
import { javascript } from "@codemirror/lang-javascript";

/**
 * CodeMirror syntax for the editor (Review.tsx), for every language a pack covers (DESIGN §4.2):
 * Python, TypeScript/TSX/JavaScript, Rust, C/C++, and the config pack's formats (YAML, TOML, JSON,
 * Markdown; Dockerfiles, shell scripts and INI-style files by name, since the pack calls those
 * "other"). Python and the JavaScript family are bundled (the editor has them at once, as before);
 * the other grammars load on first use, so the canvas bundle does not carry them.
 */
export type EditorLang =
  | "python"
  | "javascript"
  | "typescript"
  | "tsx"
  | "jsx"
  | "rust"
  | "cpp"
  | "yaml"
  | "toml"
  | "json"
  | "markdown"
  | "dockerfile"
  | "shell"
  | "properties";

/** The editor language for a window's `lang` (as the packs name them) and path. */
export function editorLang(lang: string, path: string): EditorLang | undefined {
  switch (lang) {
    case "python":
    case "typescript":
    case "tsx":
    case "rust":
    case "yaml":
    case "toml":
    case "json":
    case "markdown":
      return lang;
    case "javascript":
      return /\.[cm]?jsx$/.test(path) ? "jsx" : "javascript";
    case "c":
    case "cpp":
      return "cpp";
  }
  const name = path.split("/").pop()!.toLowerCase();
  if (name.startsWith("dockerfile") || name.endsWith(".dockerfile")) return "dockerfile";
  if (/\.(sh|bash|zsh)$/.test(name) || name === ".envrc") return "shell";
  if (/\.(ini|cfg|conf|properties)$/.test(name) || name.startsWith(".env")) return "properties";
  return undefined;
}

/** The bundled languages' extensions, at once; undefined for the ones [loadLang] fetches. */
export function bundledLang(l: EditorLang): Extension | undefined {
  switch (l) {
    case "python":
      return python();
    case "javascript":
    case "jsx":
    case "typescript":
    case "tsx":
      return javascript({ typescript: l === "typescript" || l === "tsx", jsx: l === "tsx" || l === "jsx" });
  }
  return undefined;
}

/** The CodeMirror extension for `l` (a language with its highlighting). */
export async function loadLang(l: EditorLang): Promise<Extension> {
  const now = bundledLang(l);
  if (now) return now;
  const { StreamLanguage } = await import("@codemirror/language");
  switch (l) {
    case "python":
    case "javascript":
    case "jsx":
    case "typescript":
    case "tsx":
      throw new Error("bundled");
    case "rust":
      return (await import("@codemirror/lang-rust")).rust();
    case "cpp":
      return (await import("@codemirror/lang-cpp")).cpp();
    case "yaml":
      return (await import("@codemirror/lang-yaml")).yaml();
    case "json":
      return (await import("@codemirror/lang-json")).json();
    case "markdown":
      return (await import("@codemirror/lang-markdown")).markdown();
    case "toml":
      return StreamLanguage.define((await import("@codemirror/legacy-modes/mode/toml")).toml);
    case "dockerfile":
      return StreamLanguage.define((await import("@codemirror/legacy-modes/mode/dockerfile")).dockerFile);
    case "shell":
      return StreamLanguage.define((await import("@codemirror/legacy-modes/mode/shell")).shell);
    case "properties":
      return StreamLanguage.define((await import("@codemirror/legacy-modes/mode/properties")).properties);
  }
}
