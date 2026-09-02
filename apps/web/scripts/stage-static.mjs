import { cp, mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const packageRoot = process.argv[2];
const outputRoot = process.argv[3];
if (!packageRoot || !outputRoot) {
  throw new Error("usage: pnpm stage:static <web-package> <output-directory> [scenario]");
}

const source = resolve(packageRoot);
const output = resolve(outputRoot);
const manifest = JSON.parse(await readFile(resolve(source, "manifest.json"), "utf8"));
const scenario = process.argv[4]?.trim() || manifest.entry?.trim();
if (manifest.format !== 1) {
  throw new Error(`unsupported Web manifest format ${manifest.format}; expected v1`);
}
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
await cp(resolve("dist"), output, { recursive: true, force: true });
if (scenario) {
  const indexPath = resolve(output, "index.html");
  let html = await readFile(indexPath, "utf8");
  const config = `<script>document.body.dataset.scenario=${JSON.stringify(scenario)};</script>`;
  if (!html.includes("document.body.dataset.scenario")) {
    html = html.replace("</body>", `${config}</body>`);
  } else {
    html = html.replace(/document\.body\.dataset\.scenario\s*=\s*(['\"]).*?\1\s*;/, `document.body.dataset.scenario=${JSON.stringify(scenario)};`);
  }
  await writeFile(indexPath, html);
}
console.log(`staged static game at ${output}${scenario ? ` (entry: ${scenario})` : " (no entry configured)"}`);
