// Which Chrome the e2e scripts and the bench launch: $CHROME, else a usual install on Linux or
// macOS, else Playwright's "chrome" channel (it finds a standard Chrome install itself).
import { existsSync } from "node:fs";

const CANDIDATES = [
  "/usr/bin/google-chrome-stable",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/Applications/Chromium.app/Contents/MacOS/Chromium",
];

/** Launch options naming the browser: `{ executablePath }` or `{ channel: "chrome" }`. */
export function chrome() {
  const path = process.env.CHROME ?? CANDIDATES.find((p) => existsSync(p));
  return path ? { executablePath: path } : { channel: "chrome" };
}
