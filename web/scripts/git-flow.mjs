// End-to-end check of git mode: `cv serve` on a plain git repository (no jj) shows the setup screen
// and the history read-only (branches, tags, HEAD, a commit's files), jj routes answer 409
// "jj-required", and "Initialise jj here" runs `jj git init --colocate`, after which the server
// restarts itself and the page reloads into the full app with the graph's branches as bookmarks.
// With TERMINAL=1 jj is initialised from a "terminal" (this script) instead of the button.
// Usage: node scripts/git-flow.mjs <repo dir> <screenshot dir>
//   makes the git repository when it does not exist (SETUP_ONLY=1: only that), then waits for
//   `cv serve <repo dir>` at URL (default http://127.0.0.1:7993; a `?token=` in URL is used too).
import { chromium } from "playwright-core";
import { chrome } from "./chrome.mjs";
import { mkdirSync, writeFileSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { join } from "node:path";

const [REPO, out = "shots-git"] = process.argv.slice(2);
const URL_ = process.env.URL ?? "http://127.0.0.1:7993";
const git = (...args) => execFileSync("git", ["-c", "user.name=E2E", "-c", "user.email=e2e@example.invalid", "-c", "init.defaultBranch=main", ...args], { cwd: REPO, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();

if (!existsSync(join(REPO, ".git"))) {
  mkdirSync(join(REPO, "src"), { recursive: true });
  git("init", "-q");
  for (let i = 0; i < 4; i++) writeFileSync(join(REPO, "src", `m${i}.py`), Array.from({ length: 20 }, (_, k) => `def f${i}_${k}(x):\n    return x + ${k}\n`).join("\n"));
  git("add", ".");
  git("commit", "-qm", "initial modules");
  git("checkout", "-qb", "feature/login");
  writeFileSync(join(REPO, "src", "login.py"), "from m0 import f0_0\n\ndef login(u):\n    return f0_0(len(u))\n");
  git("add", ".");
  git("commit", "-qm", "login: first cut");
  git("checkout", "-q", "main");
  writeFileSync(join(REPO, "src", "m1.py"), "def g(x):\n    return x\n");
  git("commit", "-qam", "m1: simplify");
  git("merge", "-q", "--no-ff", "-m", "Merge feature/login", "feature/login");
  git("tag", "-a", "v0.1", "-m", "v0.1");
  console.log(`git repo ready: serve it with cv serve ${REPO} --port 7993`);
}
if (process.env.SETUP_ONLY) process.exit(0);
mkdirSync(out, { recursive: true });

const errors = [];
const check = (c, s) => (c ? console.log(`ok   ${s}`) : (errors.push(s), console.log(`FAIL ${s}`)));
const { origin, searchParams } = new URL(URL_);
const token = searchParams.get("token");
for (let i = 0; i < 300; i++) {
  if (await fetch(`${origin}/api/v1/setup`, { headers: token ? { authorization: `Bearer ${token}` } : {} }).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 500));
}

const browser = await chromium.launch({ ...chrome(), args: process.env.CHROME_ARGS ? process.env.CHROME_ARGS.split(" ") : ["--ignore-gpu-blocklist", "--use-angle=vulkan", "--enable-features=Vulkan"] });
const page = await browser.newPage({ viewport: { width: 1500, height: 900 }, colorScheme: process.env.LIGHT ? "light" : "dark" });
// 409 jj-required is asked for on purpose below; a restarting server drops requests.
page.on("pageerror", (e) => errors.push(e.message));
page.on("console", (m) => m.type() === "error" && !/409|Failed to fetch|ERR_CONNECTION|net::/.test(m.text()) && errors.push(m.text()));
await page.goto(URL_);

const card = page.getByRole("region", { name: "Set up jj" });
await card.waitFor({ timeout: 30000 });
check(await card.getByText("Set up jj to open the map").isVisible(), "the setup screen says jj is needed");
check(await page.getByText("git only · jj not set up").isVisible(), "the top bar says git only");
check(await page.locator(".chip.gp-ro").isVisible(), "the graph is marked read-only");
const row = (t) => page.locator(".gp-row", { hasText: t }).first();
await row("Merge feature/login").waitFor();
check((await page.locator(".gp-row").count()) === 4, `graph shows the 4 commits (${await page.locator(".gp-row").count()})`);
check(await row("Merge feature/login").locator(".chip.bm", { hasText: "main" }).isVisible(), "main on the merge");
check(await row("Merge feature/login").locator(".chip.tag", { hasText: "v0.1" }).isVisible(), "tag v0.1 on the merge");
check(await row("Merge feature/login").locator(".chip.ws", { hasText: "HEAD" }).isVisible(), "HEAD marked");
check((await row("login: first cut").getAttribute("draggable")) === "false", "rows are not draggable (read-only)");
await row("login: first cut").click();
await page.locator(".commit-files li", { hasText: "src/login.py" }).waitFor();
check(await page.locator(".commit-files li", { hasText: "+4" }).isVisible(), "a commit's files with line counts");
const status = await page.evaluate(() => fetch("/api/v1/views", { method: "POST", headers: { "content-type": "application/json" }, body: '{"mode":"snapshot"}' }).then((r) => r.status));
check(status === 409, `jj routes answer 409 (${status})`);
await page.screenshot({ path: join(out, "1-git-mode.png") });
await page.setViewportSize({ width: 420, height: 860 });
await page.waitForTimeout(300);
await page.screenshot({ path: join(out, "1b-git-mode-narrow.png") });
await page.setViewportSize({ width: 1500, height: 900 });

if (process.env.TERMINAL) {
  execFileSync("jj", ["git", "init", "--colocate"], { cwd: REPO, stdio: "ignore" });
} else {
  await card.getByRole("button", { name: "Initialise jj here" }).click();
  const dlg = page.getByRole("alertdialog", { name: "Initialise jj" });
  await dlg.waitFor();
  await page.screenshot({ path: join(out, "2-confirm.png") });
  await dlg.getByRole("button", { name: "Initialise jj" }).click();
  await page.locator(".setup-wait").waitFor({ timeout: 30000 }).catch(() => {});
  await page.screenshot({ path: join(out, "3-restarting.png") });
}
// The page reloads into the full app once the server is back with jj.
await page.waitForFunction(() => !!window.__cv, null, { timeout: 180000 }).catch(() => {});
check(await page.evaluate(() => !!window.__cv), "the page reloaded into the full app");
check(existsSync(join(REPO, ".jj")), "jj initialised alongside git");
check(existsSync(join(REPO, ".git")), "git still there");
await page.waitForTimeout(800);
await page.keyboard.press("g");
// jj's default log revset hides immutable history (the tag makes main immutable): ask for the branches.
await page.getByLabel("Graph preset").selectOption({ label: "Bookmarks" });
await page.locator(".gp-row .chip.bm", { hasText: "feature/login" }).first().waitFor({ timeout: 30000 }).catch(() => {});
check(await page.locator(".gp-row .chip.bm", { hasText: "feature/login" }).first().isVisible(), "git branches are jj bookmarks in the graph");
await page.screenshot({ path: join(out, "4-jj-mode.png") });

await browser.close();
if (errors.length) {
  console.log(`\n${errors.length} problem(s):\n  ${errors.join("\n  ")}`);
  process.exit(1);
}
console.log("\ngit-flow: all checks passed");
