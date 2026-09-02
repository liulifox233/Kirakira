import { chromium } from "playwright";

const packagePath = process.env.KIRAKIRA_WEB_PACKAGE;
const origin = process.env.KIRAKIRA_WEB_URL
  || (packagePath ? `http://127.0.0.1:5173/${packagePath.replace(/^\/+/, "").replace(/\/+$/, "")}/` : "http://127.0.0.1:5173/");
const url = new URL(origin);
if (packagePath) url.searchParams.set("debug", "1");

const browser = await chromium.launch({
  headless: process.env.HEADLESS !== "0",
  args: ["--enable-unsafe-swiftshader", "--use-angle=swiftshader"],
});
const page = await browser.newPage({ viewport: { width: 1280, height: 720 }, deviceScaleFactor: 1 });
const pageErrors = [];
page.on("pageerror", (error) => pageErrors.push(String(error.stack || error)));
page.on("console", (message) => {
  if (process.env.KIRAKIRA_BROWSER_VERBOSE === "1") {
    console.log(`[browser:${message.type()}] ${message.text()}`);
  }
});

try {
  await page.goto(url.toString(), { waitUntil: "domcontentloaded", timeout: 30_000 });
  if (packagePath) {
    await page.waitForFunction(
      () => {
        const status = document.body.dataset.status || "";
        // The shell starts the manifest/static-shell entry immediately after
        // bootstrap, so "Game package ready" can be a one-frame transient.
        // Accept the subsequent scenario states as proof that package
        // bootstrap completed, while still surfacing a load failure through
        // the real-draw probe below.
        return status === "Game package ready"
          || status === "Game startup owns scenario selection"
          || status.startsWith("Loading scenario:")
          || status.startsWith("Scenario ready:")
          || status.startsWith("Scenario failed:");
      },
      null,
      { timeout: 15 * 60_000 },
    );
  }
  await page.waitForFunction(() => document.body.dataset.renderer, null, { timeout: 30_000 });
  let packageProbe = null;
  if (packagePath) {
    packageProbe = await page.evaluate(async () => {
      const runtime = window.__kirakiraRuntime;
      if (!runtime) throw new Error("debug runtime was not exposed");
      await new Promise((resolve, reject) => {
        const deadline = setTimeout(() => reject(new Error("GINKA Chromium loop produced no ready draw commands")), 120_000);
        const poll = () => {
          // Two draw commands are the transparent 1280x720 root placeholders
          // created while startup.tjs is still bootstrapping.  Do not inject
          // a synthetic click into that state: wait for a populated scene and
          // an idle asset queue.  Some real games intentionally keep the KAG
          // location unset while a movie/overlay owns the first screen, so a
          // location string is not a reliable readiness signal here.
          const model = window.__kirakiraLastModel;
          if ((model?.drawList?.length ?? 0) > 10
            && (model.pendingAssets ?? 0) === 0
            && model.kagState !== "WaitingResource") {
            clearTimeout(deadline);
            resolve();
          } else {
            setTimeout(poll, 16);
          }
        };
        poll();
      });
      const model = window.__kirakiraLastModel;
      if (!model?.drawList?.length) throw new Error("GINKA Chromium loop produced no ready draw commands");
      runtime.pointer_down(640, 360);
      runtime.pointer_up(640, 360);
      const video = await runtime.load_video("op.wmv");
      if (video.length < 1_000_000) throw new Error(`GINKA Chromium video payload too small: ${video.length}`);
      const audio = await runtime.load_storage("bgm/bgm001.ogg");
      const audioContext = new OfflineAudioContext(2, 44_100, 44_100);
      const decoded = await audioContext.decodeAudioData(audio.buffer.slice(0));
      if (!decoded.duration) throw new Error("GINKA Chromium audio payload did not decode");
      return { draws: model.drawList.length, videoBytes: video.length, audioSeconds: decoded.duration };
    });
  }
  const canvas = await page.locator("canvas").evaluate((element) => ({
    width: element.width,
    height: element.height,
  }));
  await page.screenshot({ path: process.env.KIRAKIRA_BROWSER_SCREENSHOT || "/tmp/kirakira-browser-smoke.png" });
  if (pageErrors.length) throw new Error(`browser page errors:\n${pageErrors.join("\n")}`);
  console.log(JSON.stringify({
    url: url.toString(),
    renderer: await page.locator("body").getAttribute("data-renderer"),
    status: await page.locator("body").getAttribute("data-status"),
    canvas,
    packageProbe,
  }));
} finally {
  await browser.close();
}
