// Browser-only stand-in for the Tauri backend, used when ui/index.html is
// opened outside the app (design previews via scripts/preview-ui.mjs). Not
// part of the app bundle; app.js only imports it when window.__TAURI__ is
// missing.

const names = [
  "IMG_4821.jpg", "IMG_4822.jpg", "studio-shoot-01.png", "IMG_4830.jpg", "product-hero.png",
  "DSC_0192.jpg", "DSC_0193.jpg", "team-summer.jpg", "IMG_4845.jpg", "banner-wide.png",
  "DSC_0201.jpg", "office-lobby.jpg", "IMG_4851.jpg", "logo-mockup.png", "DSC_0210.jpg",
];

function fakeImages(count, offset = 0) {
  return Array.from({ length: count }, (_, i) => {
    const name = names[(i + offset) % names.length].replace(/(\.\w+)$/, i >= names.length ? `-${i}$1` : "$1");
    const big = i % 3 !== 2;
    return {
      id: offset + i + 1,
      path: `/Users/demo/Pictures/shoot/${offset}-${i}-${name}`,
      name,
      format: name.endsWith(".png") ? "png" : "jpeg",
      width: big ? 4032 : 2400,
      height: big ? 3024 : 1600,
      size: Math.round((2.6 + ((i * 7) % 30) / 10) * 1e6),
    };
  });
}

let stopFlag = false;

async function runFake(items, options, ch, workers = 5) {
  const queue = [...items];
  const t0 = performance.now();
  let done = 0, failed = 0, cancelled = 0, orig = 0, out = 0;
  const one = async (it) => {
    ch.fn({ kind: "started", id: it.id });
    for (let p = 0.1; p < 1; p += 0.1 + Math.random() * 0.15) {
      await new Promise((r) => setTimeout(r, 120 + Math.random() * 200));
      ch.fn({ kind: "progress", id: it.id, progress: Math.min(p, 0.99) });
    }
    if (it.id % 11 === 4) {
      failed++;
      ch.fn({ kind: "failed", id: it.id, error: "Image is too large (96 megapixels; max 90)" });
      return;
    }
    const size = 3_000_000, outSize = Math.round(size * (0.1 + Math.random() * 0.05));
    done++; orig += size; out += outSize;
    const w = options.max_width && options.max_width < 4032 ? options.max_width : 4032;
    ch.fn({ kind: "done", id: it.id, outName: "x", outSize, origSize: size, width: w, height: Math.round(w * 0.75), format: options.to_webp ? "webp" : "jpeg", note: "JPEG q78 4:2:0, progressive, trellis-optimized" });
  };
  await Promise.all(Array.from({ length: workers }, async () => {
    while (queue.length) {
      const it = queue.shift();
      if (stopFlag) { cancelled++; ch.fn({ kind: "cancelled", id: it.id }); continue; }
      await one(it);
    }
  }));
  ch.fn({ kind: "finished", done, failed, cancelled, origBytes: orig, outBytes: out, elapsedMs: performance.now() - t0 });
}

let scans = 0;
export default {
  async invoke(cmd, args = {}) {
    switch (cmd) {
      case "app_info":
        return { version: "0.1.0-dev", workers: 5, output: { path: "/Users/demo/Pictures/SW3 Compressed", label: "Pictures / SW3 Compressed" } };
      case "pick_images":
      case "scan_paths":
        await new Promise((r) => setTimeout(r, 300));
        return { images: fakeImages(scans++ === 0 ? 128 : 6, scans * 1000), skipped: 0 };
      case "pick_output_dir":
        return null;
      case "start_batch":
        stopFlag = false;
        setTimeout(() => runFake(args.ids.map((id) => ({ id })), args.options, args.onEvent), 50);
        return 5;
      case "stop_batch":
        stopFlag = true;
        return null;
      case "retry_file":
        setTimeout(() => runFake([{ id: args.id }], args.options, { fn: (e) => e.kind !== "finished" && args.onEvent.fn(e) }, 1), 50);
        return null;
      case "forget_images":
        return null;
      case "check_for_update":
        return null;
      default:
        return null;
    }
  },
  channel(fn) {
    return { fn };
  },
  onDragDrop() {},
  on() {},
};
