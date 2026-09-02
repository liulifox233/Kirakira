import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = process.argv[2];
const outputRoot = process.argv[3];
if (!packageRoot || !outputRoot) {
  throw new Error("usage: pnpm stage:static <web-package> <output-directory> [scenario]");
}

const source = resolve(packageRoot);
const output = resolve(outputRoot);
if (output === source || output.startsWith(`${source}${sep}`)) {
  throw new Error("static output must be outside the Web package source directory");
}
const manifest = JSON.parse(await readFile(resolve(source, "manifest.json"), "utf8"));
const scenario = process.argv[4]?.trim() || manifest.entry?.trim();
if (manifest.format !== 1) {
  throw new Error(`unsupported Web manifest format ${manifest.format}; expected v1`);
}
// Staging is a reproducible publication step: remove stale semantic assets
// before copying the current package and shell into the destination.
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
await cp(resolve(source, "manifest.json"), resolve(output, "manifest.json"), { force: true });
for (const entry of Object.values(manifest.entries || {})) {
  if (!entry?.path || entry.path.startsWith("/") || entry.path.split(/[\\/]/).includes("..")) {
    throw new Error(`invalid semantic asset path: ${entry?.path}`);
  }
  const destination = resolve(output, entry.path);
  await mkdir(resolve(destination, ".."), { recursive: true });
  await cp(resolve(source, entry.path), destination, { force: true });
}
const shellDist = fileURLToPath(new URL("../dist/", import.meta.url));
await cp(shellDist, output, { recursive: true, force: true });
const indexPath = resolve(output, "index.html");
let html = await readFile(indexPath, "utf8");
const packageConfig = "document.body.dataset.package=\"./\";";
if (html.includes("document.body.dataset.package")) {
  html = html.replace(/document\.body\.dataset\.package\s*=\s*(['\"]).*?\1\s*;/, packageConfig);
}
if (scenario) {
  const config = `<script>document.body.dataset.scenario=${JSON.stringify(scenario)};</script>`;
  if (!html.includes("document.body.dataset.scenario")) {
    html = html.replace("</body>", `${config}</body>`);
  } else {
    html = html.replace(/document\.body\.dataset\.scenario\s*=\s*(['\"]).*?\1\s*;/, `document.body.dataset.scenario=${JSON.stringify(scenario)};`);
  }
}
await writeFile(indexPath, html);
console.log(`staged static game at ${output}${scenario ? ` (entry: ${scenario})` : " (no entry configured)"}`);
