#!/usr/bin/env node

/**
 * Small, deterministic browser driver for the Web v1 shell.
 *
 * It is intentionally a separate tool from browser-smoke.mjs: smoke tests
 * assert invariants, while this command leaves behind screenshots and a
 * machine-readable timeline that can be inspected while debugging a game.
 * The URL is always the static shell URL; `debug=1` is only used to expose the
 * in-page runtime model and never selects a package.
 */
import { mkdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { chromium } from "playwright";

const usage = `
Usage: pnpm play:browser [options]

Options:
  --url URL             static game URL (env KIRAKIRA_WEB_URL, default http://127.0.0.1:5173/)
  --out DIR             output directory (default /tmp/kirakira-browser-play-<timestamp>)
  --wait MS             wait before the implicit final screenshot (default 10000)
  --watch MS            save a screenshot/model snapshot every MS while actions run
  --click X Y           move and click the canvas at CSS viewport coordinates
  --key KEY             press a browser key, e.g. Enter, Escape, ArrowRight
  --inspect X Y         print engine hit-test candidates without sending input
  --screenshot LABEL    save a screenshot and model snapshot with LABEL
  --actions JSON        action array, e.g. '[{"wait":3000},{"click":{"x":640,"y":360}]'
  --headful             show Chromium instead of running headless
  --browser PATH        browser executable (env KIRAKIRA_BROWSER_EXECUTABLE)
  --verbose             print every browser console message
  --help                show this help

The action array accepts: {"wait": milliseconds}, {"click":{"x":X,"y":Y}},
{"key":"Enter"}, {"inspect":{"x":X,"y":Y}}, and {"screenshot":"label"}.
Actions are performed in order.
`;

const argv = process.argv.slice(2);
const takeValue = (index, option) => {
  const value = argv[index + 1];
  if (!value || value.startsWith("--")) throw new Error(`${option} requires a value`);
  return value;
};
let urlValue = process.env.KIRAKIRA_WEB_URL || "http://127.0.0.1:5173/";
let outValue = process.env.KIRAKIRA_BROWSER_OUT
  || `/tmp/kirakira-browser-play-${new Date().toISOString().replace(/[:.]/g, "-")}`;
let implicitWait = Number(process.env.KIRAKIRA_BROWSER_WAIT || 10_000);
let watchInterval = Number(process.env.KIRAKIRA_BROWSER_WATCH || 0);
let headless = process.env.HEADLESS !== "0";
let verbose = process.env.KIRAKIRA_BROWSER_VERBOSE === "1";
let executablePath = process.env.KIRAKIRA_BROWSER_EXECUTABLE || undefined;
let actions = [];

for (let index = 0; index < argv.length; index += 1) {
  const option = argv[index];
  if (option === "--help" || option === "-h") {
    console.log(usage.trim());
    process.exit(0);
  } else if (option === "--url") {
    urlValue = takeValue(index, option); index += 1;
  } else if (option === "--out") {
    outValue = takeValue(index, option); index += 1;
  } else if (option === "--wait") {
    implicitWait = Number(takeValue(index, option)); index += 1;
  } else if (option === "--watch") {
    watchInterval = Number(takeValue(index, option)); index += 1;
  } else if (option === "--click") {
    const x = Number(takeValue(index, option));
    const y = Number(argv[index + 2]);
    if (!Number.isFinite(x) || !Number.isFinite(y)) throw new Error("--click expects numeric X Y");
    actions.push({ click: { x, y } }); index += 2;
  } else if (option === "--key") {
    actions.push({ key: takeValue(index, option) }); index += 1;
  } else if (option === "--inspect") {
    const x = Number(takeValue(index, option));
    const y = Number(argv[index + 2]);
    if (!Number.isFinite(x) || !Number.isFinite(y)) throw new Error("--inspect expects numeric X Y");
    actions.push({ inspect: { x, y } }); index += 2;
  } else if (option === "--screenshot") {
    actions.push({ screenshot: takeValue(index, option) }); index += 1;
  } else if (option === "--actions") {
    const value = takeValue(index, option); index += 1;
    const parsed = JSON.parse(value);
    if (!Array.isArray(parsed)) throw new Error("--actions must be a JSON array");
    actions.push(...parsed);
  } else if (option === "--headful") {
    headless = false;
  } else if (option === "--browser") {
    executablePath = takeValue(index, option); index += 1;
  } else if (option === "--verbose") {
    verbose = true;
  } else {
    throw new Error(`unknown option ${option}\n${usage}`);
  }
}
if (!Number.isFinite(implicitWait) || implicitWait < 0) throw new Error("--wait must be a non-negative number");
if (!Number.isFinite(watchInterval) || watchInterval < 0) throw new Error("--watch must be a non-negative number");

const output = resolve(outValue);
await mkdir(output, { recursive: true });
const url = new URL(urlValue);
url.searchParams.set("debug", "1");

const browser = await chromium.launch({
  headless,
  executablePath,
  args: ["--enable-unsafe-swiftshader", "--use-angle=swiftshader"],
});
const page = await browser.newPage({ viewport: { width: 1280, height: 720 }, deviceScaleFactor: 1 });
const consoleLines = [];
const pageErrors = [];
const requestFailures = [];
const timeline = [];
let sequence = 0;

page.on("console", (message) => {
  const line = `[browser:${message.type()}] ${message.text()}`;
  consoleLines.push(line);
  if (verbose || message.type() === "error" || message.type() === "warning") console.log(line);
});
page.on("pageerror", (error) => {
  const line = String(error.stack || error);
  pageErrors.push(line);
  console.error(`[browser:pageerror] ${line}`);
});
page.on("requestfailed", (request) => {
  const line = `${request.method()} ${request.url()} (${request.failure()?.errorText || "failed"})`;
  requestFailures.push(line);
  console.error(`[browser:requestfailed] ${line}`);
});

const sleep = (milliseconds) => new Promise((resolveSleep) => setTimeout(resolveSleep, milliseconds));
const safeLabel = (label) => String(label || "snapshot").replace(/[^a-zA-Z0-9._-]+/g, "-").replace(/^-+|-+$/g, "") || "snapshot";

const readModel = async () => page.evaluate(() => {
  const model = window.__kirakiraLastModel || {};
  const body = document.body?.dataset || {};
  const canvas = document.querySelector("canvas");
  const layers = model.layers || model.layerTree || [];
  return {
    status: body.status || "",
    renderer: body.renderer || "",
    scenario: body.scenario || "",
    location: model.locationStorage || model.location || "",
    kagState: model.kagState || "",
    drawCommands: model.drawList?.length ?? model.drawCommands ?? 0,
    uploads: model.uploads?.length ?? model.imageUploads ?? 0,
    pendingAssets: model.pendingAssets ?? model.pendingAssetPaths?.length ?? 0,
    pendingPaths: model.pendingAssetPaths || [],
    layers: Array.isArray(layers) ? layers.length : 0,
    audioCommands: model.audio?.length || 0,
    videos: model.videos?.length || 0,
    canvas: canvas ? { width: canvas.width, height: canvas.height } : null,
    href: window.location.href,
  };
});

const snapshot = async (label = "snapshot") => {
  const id = String(++sequence).padStart(3, "0");
  const slug = safeLabel(label);
  const model = await readModel();
  const record = { id: Number(id), label: slug, at: new Date().toISOString(), ...model };
  timeline.push(record);
  const imagePath = join(output, `${id}-${slug}.png`);
  const jsonPath = join(output, `${id}-${slug}.json`);
  await page.screenshot({ path: imagePath, animations: "disabled" });
  await writeFile(jsonPath, `${JSON.stringify(record, null, 2)}\n`);
  console.log(`[play] ${slug}: ${imagePath}`);
  console.log(`[play] state ${JSON.stringify(model)}`);
  return record;
};

// A watch interval may fire while the main action sequence is taking its
// final screenshot. Keep asynchronous captures alive until the browser is
// closed so Playwright does not reject an in-flight screenshot during normal
// shutdown.
const pendingSnapshots = new Set();
const snapshotTracked = (label) => {
  const promise = snapshot(label);
  pendingSnapshots.add(promise);
  const remove = () => pendingSnapshots.delete(promise);
  promise.then(remove, remove);
  return promise;
};

const runAction = async (action, index) => {
  if (!action || typeof action !== "object") throw new Error(`action ${index} is not an object`);
  if (Object.hasOwn(action, "wait")) {
    const milliseconds = Number(action.wait);
    if (!Number.isFinite(milliseconds) || milliseconds < 0) throw new Error(`action ${index} has invalid wait`);
    console.log(`[play] wait ${milliseconds}ms`);
    await sleep(milliseconds);
  } else if (action.inspect) {
    const x = Number(action.inspect.x);
    const y = Number(action.inspect.y);
    if (!Number.isFinite(x) || !Number.isFinite(y)) throw new Error(`action ${index} has invalid inspect point`);
    const hitTargets = await page.evaluate(({ x: pointX, y: pointY }) => {
      const runtime = window.__kirakiraRuntime;
      const canvas = document.querySelector("canvas");
      const rect = canvas?.getBoundingClientRect();
      if (!runtime || typeof runtime.inspect_pointer !== "function" || !rect || !canvas) return [];
      const logicalX = (pointX - rect.left) * ((canvas.clientWidth || rect.width) / Math.max(1, rect.width));
      const logicalY = (pointY - rect.top) * ((canvas.clientHeight || rect.height) / Math.max(1, rect.height));
      return Array.from(runtime.inspect_pointer(logicalX, logicalY));
    }, { x, y });
    console.log(`[play] inspect (${x}, ${y}) ${JSON.stringify(hitTargets)}`);
  } else if (action.click) {
    const x = Number(action.click.x);
    const y = Number(action.click.y);
    if (!Number.isFinite(x) || !Number.isFinite(y)) throw new Error(`action ${index} has invalid click`);
    const hitTargets = await page.evaluate(({ x: pointX, y: pointY }) => {
      const runtime = window.__kirakiraRuntime;
      if (!runtime || typeof runtime.inspect_pointer !== "function") return [];
      const canvas = document.querySelector("canvas");
      const rect = canvas?.getBoundingClientRect();
      if (!rect || !canvas) return [];
      const logicalX = (pointX - rect.left) * ((canvas.clientWidth || rect.width) / Math.max(1, rect.width));
      const logicalY = (pointY - rect.top) * ((canvas.clientHeight || rect.height) / Math.max(1, rect.height));
      return Array.from(runtime.inspect_pointer(logicalX, logicalY));
    }, { x, y });
    if (hitTargets.length) console.log(`[play] hit targets ${JSON.stringify(hitTargets)}`);
    else console.log("[play] hit targets []");
    console.log(`[play] click (${x}, ${y})`);
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.up();
  } else if (action.key) {
    console.log(`[play] key ${action.key}`);
    await page.keyboard.press(String(action.key));
  } else if (action.screenshot) {
    await snapshotTracked(action.screenshot);
  } else {
    throw new Error(`action ${index} is unsupported: ${JSON.stringify(action)}`);
  }
};

let watchTimer;
try {
  console.log(`[play] opening ${url}`);
  await page.goto(url.toString(), { waitUntil: "domcontentloaded", timeout: 30_000 });
  // The first frame can be deliberately empty while WASM and the manifest
  // are fetched. Give the shell a short chance to publish its debug model.
  await page.waitForFunction(() => Boolean(window.__kirakiraLastModel || document.body?.dataset?.status), null, { timeout: 30_000 }).catch(() => {});
  await snapshotTracked("boot");
  if (watchInterval > 0) {
    watchTimer = setInterval(() => {
      void snapshotTracked("watch").catch((error) => {
        console.error(`[play] watch snapshot failed: ${error}`);
      });
    }, watchInterval);
  }
  for (let index = 0; index < actions.length; index += 1) await runAction(actions[index], index);
  if (implicitWait > 0) await sleep(implicitWait);
  await snapshotTracked("final");
} finally {
  if (watchTimer) clearInterval(watchTimer);
  await Promise.allSettled([...pendingSnapshots]);
  await writeFile(join(output, "timeline.jsonl"), timeline.map((entry) => JSON.stringify(entry)).join("\n") + (timeline.length ? "\n" : ""));
  await writeFile(join(output, "console.log"), consoleLines.join("\n") + (consoleLines.length ? "\n" : ""));
  await writeFile(join(output, "errors.json"), `${JSON.stringify({ pageErrors, requestFailures }, null, 2)}\n`);
  await browser.close();
}

if (pageErrors.length || requestFailures.length) process.exitCode = 1;
console.log(`[play] artifacts written to ${output}`);
