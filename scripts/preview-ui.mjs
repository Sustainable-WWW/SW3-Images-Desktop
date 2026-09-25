// Serves ui/ on http://127.0.0.1:1420 for design previews in a normal browser
// (the backend is replaced by ui/dev-mock.js). Run: node scripts/preview-ui.mjs
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../ui/", import.meta.url));
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml" };

createServer(async (req, res) => {
  const rel = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname)).replace(/^([/\\])+/, "") || "index.html";
  if (rel.startsWith("..")) return res.writeHead(403).end();
  try {
    const body = await readFile(join(root, rel));
    res.writeHead(200, { "content-type": types[extname(rel)] || "application/octet-stream" }).end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
}).listen(1420, "127.0.0.1", () => console.log("UI preview on http://127.0.0.1:1420"));
