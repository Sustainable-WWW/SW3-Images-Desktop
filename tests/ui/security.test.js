// Static security checks on the UI bundle. File names and error messages come
// from untrusted files, so they must never be interpreted as HTML, and the app
// must never load remote code.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

const ui = fileURLToPath(new URL("../../ui/", import.meta.url));
const read = (f) => readFileSync(join(ui, f), "utf8");
const scripts = readdirSync(ui).filter((f) => f.endsWith(".js"));
const htmlFiles = readdirSync(ui).filter((f) => f.endsWith(".html"));
const html = htmlFiles.map(read).join("\n");

test("no HTML injection sinks in UI scripts", () => {
  const sinks = [/\.innerHTML\s*=/, /\.outerHTML\s*=/, /insertAdjacentHTML/, /document\.write/, /\beval\s*\(/, /new\s+Function\s*\(/, /setTimeout\(\s*["'`]/];
  for (const f of scripts) {
    const src = read(f);
    for (const re of sinks) assert.ok(!re.test(src), `${f} uses ${re}`);
  }
});

test("no inline scripts or inline event handlers (CSP-compatible)", () => {
  assert.ok(!/<script(?![^>]*\bsrc=)[^>]*>/i.test(html), "inline <script> found");
  assert.ok(!/\son[a-z]+\s*=/i.test(html), "inline on* handler found");
  assert.ok(!/javascript:/i.test(html));
});

test("nothing is loaded from the network", () => {
  const styles = readdirSync(ui).filter((f) => f.endsWith(".css"));
  for (const [name, src] of [["*.html", html], ...styles.map((f) => [f, read(f)]), ...scripts.map((f) => [f, read(f)])]) {
    const urls = src.match(/(src|href|url)\s*[=(]\s*["']?https?:\/\//gi) || [];
    assert.deepEqual(urls, [], `${name} references a remote resource`);
    assert.ok(!/@import\s+url\(\s*["']?https?:/i.test(src), `${name} imports remote CSS`);
  }
});

test("dev mock is only loaded outside the app and is not shipped", () => {
  const app = read("app.js");
  assert.match(app, /window\.__TAURI__\s*\?\s*tauriApi\(\)\s*:\s*\(await import\("\.\/dev-mock\.js"\)\)/);
  assert.ok(!readdirSync(ui).includes("dev-mock.js"), "the design-preview mock must stay out of ui/ (the bundled folder)");
});

test("the UI never sends file paths to Rust", () => {
  const app = read("app.js");
  assert.ok(!/scan_paths/.test(app), "drops are handled in Rust");
  for (const m of app.matchAll(/invoke\("(start_batch|retry_file)",\s*\{([^}]*)\}/g)) {
    assert.ok(!/path/.test(m[2]), `${m[1]} must send ids only`);
  }
});

test("user text is written with textContent", () => {
  const app = read("app.js");
  assert.match(app, /querySelector\("\.tile-name"\)\.textContent = img\.name/);
  assert.match(app, /querySelector\("\.row-name"\)\.textContent = it\.name/);
});

test("the About window can only open allowlisted links by name", () => {
  const about = read("about.js");
  assert.match(about, /invoke\?\.\("open_link", \{ name: btn\.dataset\.link \}\)/);
  for (const m of read("about.html").matchAll(/data-link="([^"]+)"/g)) {
    assert.ok(["sustainablewww", "michael", "github", "releases"].includes(m[1]), m[1]);
  }
});
