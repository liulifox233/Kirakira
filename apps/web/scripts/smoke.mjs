import { readFile } from "node:fs/promises";

// The CI image does not require a full browser installation.  This smoke test
// still instantiates the release wasm module and exercises the exported DOM
// boundary with the smallest HTMLCanvasElement/document shim.
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
globalThis.window.performance = { now: () => 1000 };
const startupBytes = new TextEncoder().encode('[ch text="WEB"]');
const lazyBytes = new Uint8Array([1, 2, 3, 4]);
const lazyCsvBytes = new TextEncoder().encode("name,value\nstart,1\n");
const packageFiles = new Map([
  ["/games/startup.ks", startupBytes],
  ["/games/uipsd/lazy.tlg", lazyBytes],
  ["/games/uipsd/lazy.csv", lazyCsvBytes],
  ["/games/manifest.json", new TextEncoder().encode(JSON.stringify({
    format: 1,
    game: "fixture",
    engine: "krkrz",
    bootstrap: ["startup.ks"],
    entries: {
      "startup.ks": {
        path: "startup.ks",
        kind: "script",
        size: startupBytes.length,
        mime: "text/plain",
        preload: true,
      },
      "uipsd/lazy.tlg": {
        path: "uipsd/lazy.tlg",
        kind: "image",
        size: lazyBytes.length,
        mime: "application/octet-stream",
      },
      "uipsd/lazy.csv": {
        path: "uipsd/lazy.csv",
        kind: "binary",
        size: lazyCsvBytes.length,
        mime: "text/csv",
      },
    },
  }))],
]);
globalThis.window.fetch = async (url) => {
  const bytes = packageFiles.get(new URL(url, "http://localhost").pathname);
  return new Response(bytes ? Uint8Array.from(bytes) : new Uint8Array(), {
    status: bytes ? 200 : 404,
    headers: { "content-type": "application/octet-stream" },
  });
};

const moduleUrl = new URL("../public/pkg/krkr_web.js", import.meta.url);
const wasm = await import(moduleUrl);
const bytes = await readFile(new URL("../public/pkg/krkr_web_bg.wasm", import.meta.url));
await wasm.default(bytes);
wasm.attach_canvas("kirakira-canvas");
const runtime = new wasm.WebRuntime(640, 360);
runtime.pointer_move(12, 24);
runtime.pointer_down(12, 24);
const frame = runtime.tick(1000);
runtime.pointer_up(12, 24);
if (frame.drawCommands === undefined || frame.imageUploads === undefined) {
  throw new Error("WebRuntime did not return a frame model");
}
console.log(`web wasm smoke: attach_canvas + runtime tick ok (draws=${frame.drawCommands})`);
await runtime.load_package("/games");
const packagedFrame = runtime.tick(1016);
if (packagedFrame.drawList === undefined) throw new Error("packaged runtime did not render");
console.log(`web wasm smoke: package preload + startup.ks ok (draws=${packagedFrame.drawCommands})`);
const lazy = await runtime.load_storage("uipsd/lazy.tlg");
if (lazy.length !== lazyBytes.length) throw new Error("semantic lazy resource fetch failed");
console.log(`web wasm smoke: semantic lazy resource fetch ok (bytes=${lazy.length})`);

// CSVParser is used by GINKA's title UI loader for `.func` files. It must use
// the same resumable host read path as images/scripts; otherwise a Web cache
// miss is reported as a permanent storage error and the opening loop restarts.
runtime.execute_script("csvProbe = new CSVParser();");
runtime.execute_script('csvProbe.parseStorage("uipsd/lazy.csv");');
let csvFrame;
for (let frame = 0; frame < 30; frame += 1) {
  await new Promise((resolve) => setImmediate(resolve));
  csvFrame = runtime.tick(1032 + frame * 16.6667);
  if (csvFrame.pendingAssets === 0 && csvFrame.pendingExternalResources === 0) break;
}
if (
  !csvFrame ||
  csvFrame.pendingAssets !== 0 ||
  csvFrame.pendingExternalResources !== 0 ||
  csvFrame.scriptSuspended !== 0
) {
  throw new Error("CSVParser lazy resource did not settle");
}
const csvFile = runtime.debug_eval("csvProbe.__csvFile");
if (!csvFile.includes("uipsd/lazy.csv")) {
  throw new Error(`CSVParser did not resume after lazy fetch: ${csvFile}`);
}
console.log("web wasm smoke: CSVParser lazy resource resume ok");
