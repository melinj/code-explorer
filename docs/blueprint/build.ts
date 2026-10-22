// Assembles the Code Viewer Blueprint page (published at https://claude.ai/artifact/M8kXC79WAw5WeW7s2Rk7Bp)
// from shell.html, the wireframe fragments and the contract sources. Run: bun docs/blueprint/build.ts → dist/.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "fs";
import { resolve } from "path";

const dir = import.meta.dir;
const repo = resolve(dir, "../..");
const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const fragment = (file: string) =>
  existsSync(`${dir}/${file}`) ? readFileSync(`${dir}/${file}`, "utf8") : `<p class="muted">Figures pending.</p>`;

let html = readFileSync(`${dir}/shell.html`, "utf8");
// Function replacements so "$" sequences in the inserted text are never treated as patterns.
html = html.replace("<!--WIREFRAMES-EMPHASIS-->", () => fragment("wireframes-emphasis.html"));
html = html.replace("<!--WIREFRAMES-->", () => fragment("wireframes.html"));
html = html.replace(/<!--FRAGMENT:([\w.-]+)-->/g, (_m, file: string) => fragment(file));
html = html.replace(/<!--SRC:([\w./-]+)-->/g, (_m, file: string) =>
  esc(readFileSync(`${repo}/docs/contracts/${file}`, "utf8").replace(/\n$/, "")),
);

const left = html.match(/<!--(WIREFRAMES[\w-]*|FRAGMENT:[\w.-]+|SRC:[\w./-]+)-->/g);
if (left) throw new Error(`unreplaced placeholders: ${left.join(", ")}`);

const dist = `${dir}/dist`;
mkdirSync(dist, { recursive: true });
const out = `${dist}/code-viewer-blueprint.html`;
writeFileSync(out, html);
// Local preview only: approximates the skeleton the artifact host wraps around the page at publish time.
writeFileSync(
  `${dist}/preview.html`,
  `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">` +
    `<style>:root{color-scheme:light;padding-top:env(safe-area-inset-top,0px);padding-bottom:env(safe-area-inset-bottom,0px)}` +
    `body{margin:0;font:14px system-ui,sans-serif}img{max-width:100%}[hidden]{display:none!important}</style></head><body>` +
    html +
    `</body></html>`,
);
const script = html.slice(html.lastIndexOf("<script>") + 8, html.lastIndexOf("</script>"));
writeFileSync(`${dist}/.page-script.js`, script);
console.log(JSON.stringify({
  out,
  kb: Math.round(Buffer.byteLength(html) / 1024),
  emphasis: existsSync(`${dir}/wireframes-emphasis.html`),
}));
