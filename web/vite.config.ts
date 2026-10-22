import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The API (cv serve, or cv-mock during development) listens on 127.0.0.1:7878.
const api = process.env.CV_API ?? "http://127.0.0.1:7878";
// cv serve requires a bearer token (it prints it; it is kept in ~/.local/share/code-viewer/token): with CV_TOKEN set
// the proxy adds it, so the dev UI and the e2e scripts need no ?token=.
const headers = process.env.CV_TOKEN ? { Authorization: `Bearer ${process.env.CV_TOKEN}` } : undefined;
const proxy = { "/api": { target: api, ws: true, headers } };

export default defineConfig({
  plugins: [react()],
  server: { port: 5178, strictPort: true, proxy },
  preview: { port: 5179, strictPort: true, proxy },
  // A local app loaded once: the ~930 kB main chunk (PixiJS, CodeMirror, React) is expected, so the
  // size warning would only alarm someone running start.sh for the first time.
  build: { target: "es2022", sourcemap: true, chunkSizeWarningLimit: 1500 },
});
