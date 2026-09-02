import { readFile } from "node:fs/promises";
import { join } from "node:path";

const packageRoot = process.argv[2];
if (!packageRoot) {
  throw new Error("usage: node scripts/ginka-smoke.mjs <packed-ginka-directory>");
}
const manifest = JSON.parse(await readFile(join(packageRoot, "manifest.json"), "utf8"));
const hasInitialSystemSave = (manifest.bootstrap ?? [])
  .some((path) => path.toLowerCase() === "savedata/datasu.ksd");

globalThis.HTMLCanvasElement = class HTMLCanvasElement {};
const canvas = new globalThis.HTMLCanvasElement();
globalThis.document = {
  getElementById(id) {
    if (id !== "kirakira-canvas") throw new Error(`unexpected canvas id: ${id}`);
    return canvas;
  },
};
globalThis.Window = class Window {};
globalThis.window = new globalThis.Window();
globalThis.window.document = globalThis.document;
let clockNow = 1000;
// WebClock is backed by performance.now(), so the Node harness must advance
// it along with the requestAnimationFrame timestamp. A fixed value would
// leave KRKR wait/sync tags suspended forever and would never exercise the
// real logo/opening transition.
globalThis.window.performance = { now: () => clockNow };
globalThis.window.fetch = async (url) => {
  const path = new URL(url, "http://localhost").pathname;
  try {
    const filePath = path.startsWith(packageRoot)
      ? path
      : join(packageRoot, path.replace(/^\/+/, ""));
    return new Response(await readFile(filePath), { status: 200 });
  } catch {
    return new Response(new Uint8Array(), { status: 404 });
  }
};

const moduleUrl = new URL("../public/pkg/krkr_web.js", import.meta.url);
const wasm = await import(moduleUrl);
const wasmBytes = await readFile(new URL("../public/pkg/krkr_web_bg.wasm", import.meta.url));
await wasm.default(wasmBytes);
wasm.attach_canvas("kirakira-canvas");
const runtime = new wasm.WebRuntime(1280, 720);
await runtime.load_package(packageRoot);
const entry = runtime.entry_scenario();
if (entry) throw new Error(`GINKA must use the startup.tjs flow, got entry ${entry}`);
const first = runtime.tick(1000);
let sawBrandLogo = false;
let sawDisplaySetting = false;
let sawProjectDispatcher = false;
const observeLogs = (model) => {
  for (const message of model.logs ?? []) {
    if (/safeEvalStorage|datasu|dataPath|savedata/i.test(message)) console.log(`[ginka] ${message}`);
    sawProjectDispatcher ||= message.includes("project startup: executing startup.tjs dispatcher");
    sawBrandLogo ||= /brandlogo(?:2|_effect)?\.png/i.test(message);
    // `langselect__bg0` is also reused by title UI assets.  The first-run
    // branch has a dedicated display-choice sheet, so only count that
    // marker (or an explicit langsel label) as entering setup.
    sawDisplaySetting ||= /config_1display(?:__pack)?|(?:label|target).*langsel/i.test(message);
  }
};
observeLogs(first);
runtime.pointer_down(640, 360);
clockNow = 1016;
const second = runtime.tick(1016);
observeLogs(second);
runtime.pointer_up(640, 360);
runtime.key_down("Enter");
clockNow = 1032;
const third = runtime.tick(1032);
observeLogs(third);
runtime.key_up("Enter");
if (!first.drawList || !second.drawList || !third.drawList) {
  throw new Error("GINKA package did not produce frame draw lists");
}
console.log(`ginka wasm smoke: startup + input + frame lists ok (draws=${third.drawCommands})`);
await new Promise((resolve) => setTimeout(resolve, 50));
let titleFrame;
let sawFirstScenario = false;
let sawTitleScenario = false;
const maxFrames = Number(process.env.GINKA_SMOKE_FRAMES ?? 6000);
for (let frame = 0; frame < maxFrames; frame += 1) {
  // WebResourceStore completes fetches from spawn_local; yield to Node's
  // microtask queue so the test observes the same asynchronous progression as
  // requestAnimationFrame in a browser.
  await new Promise((resolve) => setImmediate(resolve));
  if (frame === 600 || frame === 1200 || frame === 1799) {
    runtime.pointer_down(640, 360);
    runtime.pointer_up(640, 360);
  }
  const timestamp = 1064 + frame * 16.6667;
  clockNow = timestamp;
  titleFrame = runtime.tick(timestamp);
  observeLogs(titleFrame);
  if (frame % 500 === 0) {
    console.log(`[ginka] frame=${frame} location=${titleFrame.locationStorage ?? ""} draws=${titleFrame.drawCommands} kag=${titleFrame.kagState ?? ""} pending=${titleFrame.pendingAssets ?? 0} suspended=${titleFrame.scriptSuspended ?? 0} external=${titleFrame.pendingExternalResources ?? 0} loads=${titleFrame.pendingResourceLoads ?? 0} continuous=${titleFrame.continuousHandlers ?? 0} scriptEvents=${titleFrame.scriptEvents ?? 0} idle=${titleFrame.idleAsyncTriggers ?? 0} timers=${titleFrame.timers ?? 0} windowUpdates=${titleFrame.windowUpdates ?? 0} timerEnabled=${titleFrame.timerEnabled ?? 0} timerScheduled=${titleFrame.timerScheduled ?? 0} timerDue=${titleFrame.timerDue ?? 0} timerNow=${titleFrame.timerNow ?? 0}`);
  }
  for (const message of titleFrame.logs ?? []) {
    if (process.env.GINKA_SMOKE_ALL_LOGS === "1") console.log(`[ginka] ${message}`);
    sawFirstScenario ||= message.includes("first.ks");
    sawTitleScenario ||= message.includes("title.ks");
    if (/first\.ks|title\.ks|brandlogo/i.test(message)) console.log(`[ginka] ${message}`);
  }
}
if (!titleFrame?.drawList) throw new Error("GINKA startup produced no draw list");
if (!sawProjectDispatcher) {
  throw new Error("GINKA startup did not use the shared project startup dispatcher");
}
if (!sawBrandLogo) throw new Error("GINKA startup did not execute the Desktop opening/logo hook");
if (!sawFirstScenario || !sawTitleScenario) {
  throw new Error("GINKA startup did not reach both first.ks and title.ks through startup.tjs");
}
if (hasInitialSystemSave && sawDisplaySetting) {
  throw new Error("GINKA startup entered first-run language/display settings despite savedata/datasu.ksd");
}
console.log(`ginka wasm smoke: Desktop startup flow ${sawFirstScenario ? "dispatched first.ks" : "used saved system state"}${sawTitleScenario ? " and title.ks" : ""} (draws=${titleFrame.drawCommands})`);
console.log(`ginka wasm smoke: opening flow observed${hasInitialSystemSave ? "; persisted state skipped first-run settings" : ""}`);
if (process.env.GINKA_SMOKE_ENGINE_OP === "1") {
  // Exercise the same KAG movie path used by Desktop. This is opt-in because
  // it deliberately replaces the title parser after the startup assertions;
  // normal smoke keeps the game at its title screen.
  runtime.load_scenario("movie_1.ks");
  let movieFrame;
  let movieScenarioReady = false;
  for (let frame = 0; frame < 600; frame += 1) {
    await new Promise((resolve) => setImmediate(resolve));
    movieFrame = runtime.tick(100000 + frame * 16.6667);
    observeLogs(movieFrame);
    movieScenarioReady ||= movieFrame.locationStorage?.toLowerCase() === "movie_1.ks";
    if (movieScenarioReady) break;
  }
  if (!movieScenarioReady) throw new Error("GINKA movie scenario did not load");
  // `KAGWindow.process` is the script API used by the title/recollection
  // menus. Supplying the label is important: loading a scenario alone does
  // not select *op and would leave the parser at its first label.
  runtime.execute_script('kag.process("movie_1.ks", "*op");');
  for (let frame = 0; frame < 600; frame += 1) {
    await new Promise((resolve) => setImmediate(resolve));
    movieFrame = runtime.tick(110000 + frame * 16.6667);
    observeLogs(movieFrame);
    for (const message of movieFrame.logs ?? []) console.log(`[ginka] ${message}`);
    if (movieFrame.videos?.some((video) => video.storage?.toLowerCase() === "op.wmv" && video.status === "play")) break;
  }
  if (!movieFrame?.videos?.some((video) => video.storage?.toLowerCase() === "op.wmv" && video.status === "play")) {
    throw new Error("GINKA Desktop movie scenario did not reach OP browser playback");
  }
  console.log("ginka wasm smoke: Desktop sysmovie OP request reached browser overlay");
}
runtime.execute_script('m = new VideoOverlay(); m.mode = "layer"; m.setBounds(0,0,1280,720); m.visible = true; m.open("OP"); m.play();');
let opFrame;
for (let frame = 0; frame < 240; frame += 1) {
  await new Promise((resolve) => setImmediate(resolve));
  opFrame = runtime.tick(31000 + frame * 16.6667);
  if (opFrame.videos?.some((video) => video.storage?.toLowerCase() === "op.wmv" && video.status === "play")) break;
}
if (!opFrame?.videos?.some((video) => video.storage?.toLowerCase() === "op.wmv" && video.status === "play")) {
  throw new Error("GINKA OP did not enter browser video overlay playback");
}
const opBytes = await runtime.load_video("op.wmv");
if (opBytes.length < 1024 * 1024) throw new Error("GINKA OP payload is unexpectedly small");
console.log(`ginka wasm smoke: OP browser overlay + payload bridge ok (${opBytes.length} bytes)`);
