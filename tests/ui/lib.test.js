import { test } from "node:test";
import assert from "node:assert/strict";
import * as L from "../../ui/lib.js";

test("formatBytes matches the design", () => {
  assert.equal(L.formatBytes(0), "0 B");
  assert.equal(L.formatBytes(402_000), "402 KB");
  assert.equal(L.formatBytes(3_400_000), "3.4 MB");
  assert.equal(L.formatBytes(342_000_000), "342 MB");
  assert.equal(L.formatBytes(2_500_000_000), "2.5 GB");
  assert.equal(L.formatBytes(-5), "0 B");
  assert.equal(L.formatBytes("junk"), "0 B");
});

test("savings percentage", () => {
  assert.equal(L.savingsPct(3_400_000, 402_000), 88);
  assert.equal(L.savingsPct(100, 100), 0);
  assert.equal(L.savingsPct(100, 150), 0, "never negative");
  assert.equal(L.savingsPct(0, 10), 0);
});

test("ETA wording", () => {
  assert.equal(L.formatEta(null), "");
  assert.equal(L.formatEta(Infinity), "");
  assert.equal(L.formatEta(3), "Almost done");
  assert.equal(L.formatEta(41), "About 40 seconds left");
  assert.equal(L.formatEta(70), "About a minute left");
  assert.equal(L.formatEta(300), "About 5 minutes left");
  assert.equal(L.formatEta(3 * 3600 + 600), "About 3 h 10 min left");
});

test("durations", () => {
  assert.equal(L.formatDuration(12_000), "12 s");
  assert.equal(L.formatDuration(72_000), "1 min 12 s");
  assert.equal(L.formatDuration(3_780_000), "1 h 3 min");
});

test("dimension labels", () => {
  assert.equal(L.dimsLabel(4032, 3024, 1920), "4032 × 3024 → 1920 w");
  assert.equal(L.dimsLabel(6000, 4000), "6000 × 4000");
  assert.equal(L.dimsLabel(800, 600, 800), "800 × 600", "no arrow when not resized");
});

test("max width parsing rejects anything unsafe or out of range", () => {
  assert.equal(L.parseMaxWidth("1920"), 1920);
  assert.equal(L.parseMaxWidth(" 800 "), 800);
  for (const bad of ["", "15", "8193", "12.5", "-100", "1e3", "abc", "99999", null, undefined]) {
    assert.equal(L.parseMaxWidth(bad), null, String(bad));
  }
});

test("options sent to Rust", () => {
  assert.deepEqual(L.buildOptions({ mode: "max", maxWidth: 1920, webp: true }), { max_width: 1920, to_webp: true });
  assert.deepEqual(L.buildOptions({ mode: "original", maxWidth: 1920, webp: false }), { max_width: null, to_webp: false });
  assert.equal(L.optionsLabel({ max_width: 1920, to_webp: true }), "Max width 1920 px · WEBP");
  assert.equal(L.optionsLabel({ max_width: null, to_webp: false }), "Original size");
});

test("merging selections has no limit and skips duplicates", () => {
  const many = Array.from({ length: 5000 }, (_, i) => ({ path: `/p/${i}.jpg` }));
  const { merged, added } = L.mergeImages([], many);
  assert.equal(merged.length, 5000, "no file-count limit");
  assert.equal(added, 5000);
  const again = L.mergeImages(merged, [{ path: "/p/1.jpg" }, { path: "/p/new.jpg" }, { path: "/p/new.jpg" }]);
  assert.equal(again.added, 1);
  assert.equal(again.duplicates, 2);
  assert.equal(again.merged.length, 5001);
});

test("batch progress is weighted by size and estimates time", () => {
  const items = [
    { size: 300, status: "done" },
    { size: 100, status: "working", progress: 0.5 },
    { size: 600, status: "queued" },
    { size: 999, status: "cancelled" },
  ];
  const { fraction, etaSeconds } = L.batchProgress(items, 10_000);
  assert.equal(fraction, 0.35);
  // 350 bytes in 10 s -> 650 bytes left at 35 B/s ≈ 18.6 s
  assert.ok(Math.abs(etaSeconds - 650 / 35) < 0.01);
  assert.equal(L.batchProgress(items, 500).etaSeconds, null, "no estimate in the first moments");
  assert.equal(L.batchProgress([], 0).fraction, 1);
});

test("done totals", () => {
  const t = L.doneTotals([
    { status: "done", size: 1000, outSize: 100 },
    { status: "failed", size: 50 },
    { status: "done", size: 500, outSize: 50 },
  ]);
  assert.deepEqual(t, { orig: 1500, out: 150, done: 2, failed: 1 });
});

test("growth percentage for outputs that got bigger", () => {
  assert.equal(L.growthPct(1000, 1120), 12);
  assert.equal(L.growthPct(1000, 1001), 1, "never shows +0%");
  assert.equal(L.growthPct(1000, 900), 0);
});

test("plural + tile variants", () => {
  assert.equal(L.plural(1, "image"), "1 image");
  assert.equal(L.plural(128, "image"), "128 images");
  assert.deepEqual([0, 1, 2, 3, 4, 5].map(L.tileVariant), ["v0", "v1", "v2", "v3", "v4", "v0"]);
});
