// Pure helpers used by app.js. No DOM, no Tauri — unit tested with `node --test`.

/** 3.4 MB, 402 KB, 342 MB — decimal units, like Finder. */
export function formatBytes(bytes) {
  const b = Math.max(0, Number(bytes) || 0);
  if (b < 1000) return `${b} B`;
  if (b < 1e6) return `${Math.round(b / 1e3)} KB`;
  if (b < 1e9) {
    const mb = b / 1e6;
    return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
  }
  return `${(b / 1e9).toFixed(1)} GB`;
}

/** Whole-percent size reduction; never negative. */
export function savingsPct(orig, out) {
  if (!orig || orig <= 0) return 0;
  return Math.max(0, Math.round(100 * (1 - out / orig)));
}

/** Whole-percent size increase (for outputs that grew). */
export function growthPct(orig, out) {
  if (!orig || orig <= 0 || out <= orig) return 0;
  return Math.max(1, Math.round(100 * (out / orig - 1)));
}

/** "About 40 seconds left" etc. Returns "" when there is nothing to estimate. */
export function formatEta(seconds) {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return "";
  if (seconds < 5) return "Almost done";
  if (seconds < 55) return `About ${Math.max(5, Math.round(seconds / 5) * 5)} seconds left`;
  if (seconds < 90) return "About a minute left";
  if (seconds < 3600) return `About ${Math.round(seconds / 60)} minutes left`;
  const h = Math.floor(seconds / 3600);
  const m = Math.round((seconds % 3600) / 60);
  return `About ${h} h ${m} min left`;
}

/** 12 s, 1 min 12 s, 1 h 3 min */
export function formatDuration(ms) {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min ${s % 60} s`;
  return `${Math.floor(s / 3600)} h ${Math.round((s % 3600) / 60)} min`;
}

export function formatDims(w, h) {
  return `${w} × ${h}`;
}

/** "4032 × 3024 → 1920 w" once resized, otherwise just the source dimensions. */
export function dimsLabel(srcW, srcH, outW) {
  const base = formatDims(srcW, srcH);
  return outW && outW !== srcW ? `${base} → ${outW} w` : base;
}

/** Max-width input: integer in 16..8192, or null when invalid. */
export function parseMaxWidth(text) {
  const t = String(text ?? "").trim();
  if (!/^\d{1,4}$/.test(t)) return null;
  const n = Number(t);
  return n >= 16 && n <= 8192 ? n : null;
}

/** Engine options as the Rust command expects them. */
export function buildOptions({ mode, maxWidth, webp }) {
  return { max_width: mode === "max" ? maxWidth : null, to_webp: Boolean(webp) };
}

/** "Max width 1920 px · WEBP" / "Original size" */
export function optionsLabel(opts) {
  const parts = [opts.max_width ? `Max width ${opts.max_width} px` : "Original size"];
  if (opts.to_webp) parts.push("WEBP");
  return parts.join(" · ");
}

/** Append new images, skipping paths already in the list. */
export function mergeImages(existing, incoming) {
  const seen = new Set(existing.map((i) => i.path));
  const added = [];
  for (const img of incoming) {
    if (seen.has(img.path)) continue;
    seen.add(img.path);
    added.push(img);
  }
  return { merged: existing.concat(added), added: added.length, duplicates: incoming.length - added.length };
}

export function plural(n, word, pluralWord = `${word}s`) {
  return `${n} ${n === 1 ? word : pluralWord}`;
}

/** Tile colour variant, cycling like the design. */
export function tileVariant(index) {
  return `v${index % 5}`;
}

/**
 * Overall batch progress weighted by file size (bigger photos take longer),
 * plus a throughput-based time estimate.
 * `items`: [{ size, status, progress }] with status queued|working|done|failed|cancelled.
 */
export function batchProgress(items, elapsedMs) {
  let total = 0;
  let settled = 0;
  for (const it of items) {
    if (it.status === "cancelled") continue;
    total += it.size;
    if (it.status === "done" || it.status === "failed") settled += it.size;
    else if (it.status === "working") settled += it.size * (it.progress || 0);
  }
  const fraction = total > 0 ? Math.min(1, settled / total) : 1;
  let etaSeconds = null;
  if (elapsedMs > 1500 && fraction > 0.02 && fraction < 1) {
    const rate = settled / elapsedMs; // bytes per ms
    etaSeconds = (total - settled) / rate / 1000;
  }
  return { fraction, etaSeconds };
}

/** Totals over finished files: bytes in, bytes out. */
export function doneTotals(items) {
  let orig = 0;
  let out = 0;
  let done = 0;
  let failed = 0;
  for (const it of items) {
    if (it.status === "done") {
      done += 1;
      orig += it.size;
      out += it.outSize;
    } else if (it.status === "failed") failed += 1;
  }
  return { orig, out, done, failed };
}
