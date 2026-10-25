// End-to-end check of the Graph side panel against `cv serve` on a scripted jj repo: `g` opens it,
// double-click / "Diff of commit" show a commit's diff, "State at commit" its snapshot, the Diff
// mode follows the selection and "Repo" returns to the snapshot, History's "Open in Graph" selects
// a change there, the side panel hides and comes back, a bookmark dragged onto another change is previewed (backwards needs the toggle),
// applied and undone, workspace `ws` lands on `main` by merge (its @ follows) and is undone, a
// bookmark's menu moves main to ws@ (and undoes it) or to a change clicked next, Commit runs
// jj commit in ws (moving feature along, undone), Squash folds ws@ into S1, the operation log
// undoes it and restores it (undone), main is pushed to a local bare remote and fetched, and
// Expand fills the window with the graph.
// Usage: node scripts/graph-flow.mjs <repo dir> <screenshot dir>
//   makes the repo when it does not exist yet (SETUP_ONLY=1: only that), then waits for `cv serve`
//   on it at URL (default http://127.0.0.1:7992; a `?token=` in URL is used for the API too).
import { chromium } from "playwright-core";
import { chrome } from "./chrome.mjs";
import { mkdirSync, writeFileSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { join } from "node:path";

const [repoArg, out = "shots-graph"] = process.argv.slice(2);
mkdirSync(out, { recursive: true });
const URL_ = process.env.URL ?? "http://127.0.0.1:7992";
const REPO = repoArg;
const WS = `${REPO}.ws`;
const jjIn = (dir, ...args) => execFileSync("jj", ["--no-pager", "--color=never", ...args], { cwd: dir, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, JJ_EDITOR: "true" } }).trim();
const jj = (...args) => jjIn(REPO, ...args);
const log = (rev, t) => jj("log", "--ignore-working-copy", "--no-graph", "-r", rev, "-T", t);

if (process.env.SETUP || !existsSync(join(REPO, ".jj"))) {
  // main = T0 (src/*), workspace ws: S1 on T0 (+ empty @); main moves on to T1.
  mkdirSync(join(REPO, "src"), { recursive: true });
  jj("git", "init");
  for (let i = 0; i < 6; i++) writeFileSync(join(REPO, "src", `mod${i}.py`), Array.from({ length: 30 }, (_, k) => `def f${i}_${k}(x):\n    return x + ${k}\n`).join("\n"));
  writeFileSync(join(REPO, "src", "main.py"), "from mod0 import f0_0\nprint(f0_0(1))\n");
  jj("commit", "-m", "T0: initial modules");
  jj("bookmark", "create", "main", "-r", "@-");
  jj("workspace", "add", "--name", "ws", "-r", "main", WS);
  writeFileSync(join(WS, "src", "mod1.py"), "def added(x):\n    return x * 2\n\n" + Array.from({ length: 30 }, (_, k) => `def f1_${k}(x):\n    return x + ${k}\n`).join("\n"));
  jjIn(WS, "commit", "-m", "S1: double things in mod1");
  jjIn(WS, "bookmark", "create", "feature", "-r", "@-");
  jj("new", "main");
  writeFileSync(join(REPO, "src", "main.py"), "from mod0 import f0_0\nprint(f0_0(2))\n");
  jj("commit", "-m", "T1: print two");
  jj("bookmark", "set", "main", "-r", "@-");
  // A local bare git remote for fetch and push.
  execFileSync("git", ["init", "-q", "--bare", `${REPO}.remote.git`]);
  jj("git", "remote", "add", "origin", `${REPO}.remote.git`);
  console.log(`repo ready; serve it:\n  target/release/cv serve --port 7992 --no-auth ${REPO}`);
}
if (process.env.SETUP_ONLY) process.exit(0);

const errors = [];
const step = (s) => console.log(`ok   ${s}`);
const fail = (s) => {
  errors.push(s);
  console.log(`FAIL ${s}`);
};
const check = (c, s) => (c ? step(s) : fail(s));

// Wait for the server.
const { origin, searchParams } = new URL(URL_);
const token = searchParams.get("token");
for (let i = 0; i < 300; i++) {
  if (await fetch(`${origin}/api/v1/repo`, { headers: token ? { authorization: `Bearer ${token}` } : {} }).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 500));
}

const browser = await chromium.launch({ ...chrome(), args: process.env.CHROME_ARGS ? process.env.CHROME_ARGS.split(" ") : ["--ignore-gpu-blocklist", "--use-angle=vulkan", "--enable-features=Vulkan"] });
const page = await browser.newPage({ viewport: { width: 1600, height: 950 }, colorScheme: process.env.LIGHT ? "light" : "dark" });
page.on("pageerror", (e) => errors.push(e.message));
page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
await page.goto(URL_);
await page.evaluate(() => localStorage.removeItem("cv.graph.open"));
await page.goto(URL_);
await page.waitForFunction(() => !!window.__cv);
await page.waitForTimeout(800);

const panel = page.locator(".graph-panel");
await page.keyboard.press("g");
await panel.waitFor();
const row = (text) => page.locator(".gp-row", { hasText: text }).first();
await row("S1").waitFor();
check((await page.locator(".gp-row").count()) >= 4, `graph shows ${await page.locator(".gp-row").count()} changes`);
check(await page.locator(".gp-ws", { hasText: "ws@" }).isVisible(), "workspace ws@ in the strip");
check((await page.locator(".gp-ws", { hasText: "default@" }).getByRole("button", { name: "Land" }).count()) === 0, "default@ (already in main) offers no Land");
check(await row("root()").isVisible(), "the root commit reads root()");
check(await page.locator(".gp-lanes path").count() >= 3, "lane edges drawn");
await page.screenshot({ path: join(out, "1-graph-open.png") });

// What the map shows: double-click = diff of commit; State at commit; Diff of commit; the Diff
// mode follows the selection (two unrelated changes: per commit); Repo goes back.
const spec = () => page.evaluate(() => ({ range: window.__cv.engine.isRange, revset: window.__cv.engine.client.view?.spec?.revset ?? "" }));
const s1 = log("feature", "change_id.short(12)");
await row("S1").dblclick();
await page.waitForFunction((c) => window.__cv.engine.isRange && window.__cv.engine.client.view.spec.revset === c, s1, { timeout: 30000 }).catch(() => {});
check((await spec()).revset === s1, `double-click shows the diff of that commit (${(await spec()).revset})`);
check(await page.getByRole("group", { name: "The map shows" }).getByRole("button", { name: "Diff" }).getAttribute("aria-pressed") === "true", "the map switch says Diff");
await page.waitForTimeout(1200);
await page.screenshot({ path: join(out, "2-diff-of-commit.png") });
const actions = page.locator(".gp-actions");
await actions.getByRole("button", { name: "State at commit" }).click();
await page.waitForFunction(() => !window.__cv.engine.isRange, null, { timeout: 30000 }).catch(() => {});
check(!(await spec()).range, "State at commit opens a snapshot");
await page.waitForFunction(() => document.querySelector(".rev-chip")?.textContent?.trim() !== "@", null, { timeout: 30000 }).catch(() => {});
const chip = await page.locator(".rev-chip").innerText();
check(chip.startsWith(s1.slice(0, 8)) || log("feature", "commit_id").startsWith(chip.slice(0, 12)), `the snapshot is at that change (${chip})`);
check(await page.getByRole("group", { name: "The map shows" }).getByRole("button", { name: "Repo" }).getAttribute("aria-pressed") === "true", "the map switch says Repo");
await actions.getByRole("button", { name: "Diff of commit" }).click();
await page.waitForFunction((c) => window.__cv.engine.isRange && window.__cv.engine.client.view.spec.revset === c, s1, { timeout: 30000 }).catch(() => {});
check((await spec()).revset === s1, "Diff of commit opens its diff");
await row("T1").click({ modifiers: ["ControlOrMeta"] });
check(await actions.getByRole("button", { name: "Diff of commits" }).isVisible(), "two unrelated changes offer Diff of commits");
await page.waitForFunction(() => / \| /.test(window.__cv.engine.client.view.spec.revset ?? ""), null, { timeout: 30000 }).catch(() => {});
check(/ \| /.test((await spec()).revset), "and the map follows: per commit");
await row("S1").click();
await row("ws@").click({ modifiers: ["Shift"] });
check(await actions.getByRole("button", { name: "Diff of range" }).isVisible(), "a chain offers Diff of range");
await page.getByRole("group", { name: "The map shows" }).getByRole("button", { name: "Repo" }).click();
await page.waitForFunction(() => !window.__cv.engine.isRange, null, { timeout: 30000 }).catch(() => {});
check(!(await spec()).range, "Repo goes back to the snapshot");

// The History tab opens the Graph panel on the change picked there; the side panel hides.
await page.keyboard.press("g");
await panel.waitFor({ state: "detached" });
await page.getByRole("button", { name: "History", exact: true }).click();
await page.locator(".hist li", { hasText: "T0: initial" }).click();
await page.locator(".hist-bar").getByRole("button", { name: "Open in Graph" }).click();
await panel.waitFor();
await page.locator(".gp-row.on", { hasText: "T0" }).waitFor({ timeout: 10000 }).catch(() => {});
check(await page.locator(".gp-row.on", { hasText: "T0" }).isVisible(), "History's Open in Graph selects that change in the graph");
await page.getByRole("button", { name: "Hide the side panel" }).click();
check(!(await page.locator("aside.side").isVisible()), "» hides the side panel");
await page.screenshot({ path: join(out, "2b-side-hidden.png") });
await page.getByRole("button", { name: "Side panel" }).click();
check(await page.locator("aside.side").isVisible(), "Side panel brings it back");
await page.getByRole("button", { name: "Inspector", exact: true }).click();

// Drag `feature` onto T0: backwards, so the preview refuses until allowed.
const t0 = log("main-", "commit_id");
const feature0 = log("feature", "commit_id");
await page.locator(".gp-row .chip.bm", { hasText: "feature" }).dragTo(row("T0"));
const dlg = page.getByRole("dialog", { name: "Set bookmark" });
await dlg.waitFor();
await dlg.locator(".warn-box").waitFor();
check(/backwards/.test(await dlg.locator(".warn-box").innerText()), "backwards move refused in the preview");
await dlg.getByLabel("Allow moving it backwards or sideways").check();
await dlg.getByText("Preview:").waitFor();
await page.screenshot({ path: join(out, "3-move-bookmark.png") });
await dlg.getByRole("button", { name: "Apply" }).click();
await page.locator(".gp-done").waitFor();
check(log("feature", "commit_id") === t0, "feature moved onto T0");
await page.locator(".gp-done").getByRole("button", { name: "Undo" }).click();
await page.waitForFunction(() => /Undone/.test(document.querySelector(".gp-done")?.textContent ?? ""));
check(log("feature", "commit_id") === feature0, "undo put feature back");

// Land ws on main by merge.
await page.locator(".gp-ws", { hasText: "ws@" }).getByRole("button", { name: "Land" }).click();
const land = page.getByRole("dialog", { name: "Land on the trunk" });
await land.waitFor();
await land.locator(".warn-box").waitFor();
check(/fast-forward/.test(await land.locator(".warn-box").innerText()), "fast-forward refused: main moved on");
await land.getByRole("radio", { name: "Merge" }).click();
await land.getByText("Preview:").waitFor();
await page.screenshot({ path: join(out, "4-land-preview.png") });
await land.getByRole("button", { name: "Apply" }).click();
await page.locator(".gp-done").waitFor();
await page.waitForTimeout(800);
const parents = log("parents(main)", 'description.first_line() ++ "\\n"').split("\n").sort();
check(parents.length === 2 && parents[0].startsWith("S1") && parents[1].startsWith("T1"), `main is a merge of S1 and T1 (${parents})`);
check(log("parents(ws@)", "commit_id") === log("main", "commit_id"), "ws@ moved onto the merge");
check(existsSync(join(WS, "src", "mod1.py")), "ws files on disk updated");
await page.locator(".gp-row .chip.bm.trunk").first().waitFor();
await page.screenshot({ path: join(out, "5-landed.png") });
await page.locator(".gp-done").getByRole("button", { name: "Undo" }).click();
await page.waitForFunction(() => /Undone/.test(document.querySelector(".gp-done")?.textContent ?? ""));
check(log("main", 'description.first_line()').startsWith("T1"), "undo put main back on T1");

// Bookmarks: the strip and a chip's menu. main → ws@ (sideways: allowed on request), applied and undone.
const strip = page.locator(".gp-bms");
check(await strip.getByRole("button", { name: /^main/ }).isVisible(), "main in the bookmark strip");
await strip.getByRole("button", { name: /^main/ }).click();
const menu = page.getByRole("menu", { name: "Bookmark main" });
await menu.waitFor();
check(!(await menu.getByRole("menuitem", { name: /Move to default@/ }).count()), "main is already at default@'s head: no move offered");
await page.screenshot({ path: join(out, "6-bookmark-menu.png") });
await menu.getByRole("menuitem", { name: /Move to ws@/ }).click();
const mv = page.getByRole("dialog", { name: "Set bookmark" });
await mv.waitFor();
check(/S1/.test(await mv.innerText()), "Move to ws@ targets ws's head (S1, not its empty @)");
await mv.getByLabel("Allow moving it backwards or sideways").check();
await mv.getByText("Preview:").waitFor();
await mv.getByRole("button", { name: "Apply" }).click();
await page.locator(".gp-done").waitFor();
check(log("main", "commit_id") === feature0, "main moved to ws@'s head");
await page.locator(".gp-done").getByRole("button", { name: "Undo" }).click();
await page.waitForFunction(() => /Undone/.test(document.querySelector(".gp-done")?.textContent ?? ""));
check(log("main", 'description.first_line()').startsWith("T1"), "undo put main back");
// A row's chip opens the same menu; "Move to a change…" then a click picks the target.
await row("S1").locator(".chip.bm", { hasText: "feature" }).click();
await page.getByRole("menu", { name: "Bookmark feature" }).getByRole("menuitem", { name: /Move to a change/ }).click();
await page.locator(".gp-pick", { hasText: "move feature" }).waitFor();
await row("T0").click();
await mv.waitFor();
check(/T0/.test(await mv.innerText()), "Move to a change… then a click opens the move to it");
await mv.getByRole("button", { name: "Cancel" }).click();

// Commit (jj commit) in ws: a file written on disk, ws@ committed with a message, feature moved to it.
writeFileSync(join(WS, "src", "notes.py"), "NOTE = 1\n");
await row("ws@").click();
await page.locator(".gp-actions").getByRole("button", { name: "Commit…" }).click();
const cm = page.getByRole("dialog", { name: "Commit" });
await cm.waitFor();
await cm.getByLabel("Message").fill("S2: notes");
await cm.getByLabel("Move a bookmark to it").check();
check((await cm.getByLabel("Bookmark", { exact: true }).inputValue()) === "feature", "the bookmark on ws@'s parent is suggested");
await cm.getByText("Preview:").waitFor();
// Ticking the bookmark re-asks for the preview: wait for that answer, not the one already shown.
const movesFeature = await cm.locator(".gp-preview", { hasText: "feature" }).waitFor({ timeout: 15000 }).then(() => true, () => false);
check(movesFeature, "the commit preview moves feature");
await page.screenshot({ path: join(out, "6b-commit.png") });
await cm.getByRole("button", { name: "Apply" }).click();
await page.locator(".gp-done", { hasText: "commit ws@" }).waitFor();
check(log("ws@-", "description.first_line()") === "S2: notes", "ws@ committed with the message");
check(log("ws@", "empty") === "true", "a new empty ws@ on top");
check(jjIn(WS, "file", "show", "--ignore-working-copy", "-r", "ws@-", "src/notes.py") === "NOTE = 1", "the file on disk is in the commit");
check(log("feature", "commit_id") === log("ws@-", "commit_id"), "feature moved to the commit");
await page.locator(".gp-done").getByRole("button", { name: "Undo" }).click();
await page.waitForFunction(() => /Undone/.test(document.querySelector(".gp-done")?.textContent ?? ""));
check(log("feature", "commit_id") === feature0, "undo put feature back");

// Squash ws@ (notes.py, restored by the undo above) into its parent S1, then undo.
if (!/\bon\b/.test((await row("ws@").getAttribute("class")) ?? "")) await row("ws@").click();
await page.locator(".gp-actions").getByRole("button", { name: "Squash…" }).click();
const sq = page.getByRole("dialog", { name: "Squash" });
await sq.waitFor();
check(/S1/.test(await sq.innerText()), "squash goes into the parent S1 by default");
check((await sq.getByLabel("Message").inputValue()).startsWith("S1: double things"), "the message is the combined description");
await sq.getByText("Preview:").waitFor();
await page.screenshot({ path: join(out, "6c-squash.png") });
await sq.getByRole("button", { name: "Apply" }).click();
await page.locator(".gp-done", { hasText: "squash" }).waitFor();
check(jjIn(WS, "file", "show", "--ignore-working-copy", "-r", "feature", "src/notes.py") === "NOTE = 1", "notes.py squashed into S1");
check(log("ws@", "empty") === "true", "ws@ is a new empty change");
const squashed = log("feature", "commit_id");

// The operation log: the squash on top; undo it from there, then restore back to it.
await page.getByRole("tab", { name: "Operations" }).click();
const ops = page.getByLabel("Operation log");
await ops.locator(".op-row", { hasText: "squash" }).first().waitFor();
check(/squash/.test(await ops.locator(".op-row").first().innerText()), "the newest operation is the squash");
await ops.locator(".op-row", { hasText: "squash" }).first().click();
await ops.locator(".op-detail", { hasText: "Changed" }).waitFor();
await page.screenshot({ path: join(out, "6d-ops.png") });
await ops.getByRole("button", { name: "Undo this operation" }).click();
await page.locator(".gp-done", { hasText: "undo operation" }).waitFor();
check(log("feature", "commit_id") === feature0, "undo from the log put S1 back");
const sqRow = ops.locator(".op-row", { hasText: "cv: squash" }).first();
if ((await sqRow.getAttribute("aria-expanded")) !== "true") await sqRow.click();
await ops.getByRole("button", { name: "Restore to here…" }).click();
const rs = page.getByRole("dialog", { name: "Restore the repository" });
await rs.getByText("Preview:").waitFor();
await rs.getByRole("button", { name: "Restore" }).click();
await page.locator(".gp-done", { hasText: "restore" }).waitFor();
check(log("feature", "commit_id") === squashed, "restore brought the squash back");
await page.locator(".gp-done").getByRole("button", { name: "Undo" }).click();
await page.waitForFunction(() => /Undone/.test(document.querySelector(".gp-done")?.textContent ?? ""));
check(log("feature", "commit_id") === feature0, "and its undo took it away again");
await page.getByRole("tab", { name: "Graph" }).click();

// Push main to the bare remote (jj's dry run first), then fetch.
await strip.getByRole("button", { name: /^main/ }).click();
await page.getByRole("menu", { name: "Bookmark main" }).getByRole("menuitem", { name: /Push to origin/ }).click();
const pd = page.getByRole("dialog", { name: "Push" });
await pd.locator("pre.gp-out").waitFor({ timeout: 30000 });
check(/main/.test(await pd.locator("pre.gp-out").innerText()), "the push dry run names main");
await page.screenshot({ path: join(out, "6e-push.png") });
await pd.getByRole("button", { name: "Push to origin" }).click();
await page.locator(".gp-done", { hasText: "pushed main" }).waitFor({ timeout: 30000 });
check(execFileSync("git", ["--git-dir", `${REPO}.remote.git`, "rev-parse", "main"], { encoding: "utf8" }).trim() === log("main", "commit_id"), "the remote has main");
await strip.getByRole("button", { name: "Fetch" }).click();
await page.locator(".gp-done", { hasText: "fetched" }).waitFor({ timeout: 30000 });
check(true, "fetch answered");

// Expanded: the graph fills the window, with dates and commit ids; Dock puts it back.
await panel.getByRole("button", { name: /Expand/ }).click();
const wide = (await panel.boundingBox()).width;
check(wide > 1200, `expanded fills the window (${Math.round(wide)} px)`);
check(await page.locator(".gp-when").first().isVisible(), "expanded rows show dates and commit ids");
await page.screenshot({ path: join(out, "7-expanded.png") });
await panel.getByRole("button", { name: /Dock/ }).click();
check((await panel.boundingBox()).width < 800, "Dock returns to the side panel");
await page.keyboard.press("Shift+G");
check((await panel.boundingBox()).width > 1200, "G expands too");
await page.keyboard.press("Shift+G");

// Light theme and the narrow layout render.
await page.getByRole("button", { name: "Light" }).click();
await page.waitForTimeout(300);
await page.screenshot({ path: join(out, "8-light.png") });
await page.setViewportSize({ width: 420, height: 860 });
await page.waitForTimeout(400);
await page.screenshot({ path: join(out, "9-narrow.png") });

await browser.close();
if (errors.length) {
  console.log(`\n${errors.length} problem(s):\n  ${errors.join("\n  ")}`);
  process.exit(1);
}
console.log("\ngraph-flow: all checks passed");
