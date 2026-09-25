// SW3 Images — UI controller. All user-visible text from files (names,
// errors) is written with textContent, never parsed as HTML.
import * as L from "./lib.js";

const api = window.__TAURI__ ? tauriApi() : (await import("./dev-mock.js")).default;

function tauriApi() {
  const T = window.__TAURI__;
  return {
    invoke: T.core.invoke,
    channel(onMessage) {
      const ch = new T.core.Channel();
      ch.onmessage = onMessage;
      return ch;
    },
    onDragDrop(handler) {
      return T.webview.getCurrentWebview().onDragDropEvent((e) => handler(e.payload));
    },
    on(event, handler) {
      return T.event.listen(event, (e) => handler(e.payload));
    },
  };
}

const $ = (id) => document.getElementById(id);
if (navigator.userAgent.includes("Mac")) document.documentElement.classList.add("mac");

// ------------------------------------------------------------------ state

const PREFS_KEY = "sw3.prefs";
const state = {
  images: [], // current selection: { id, path, name, format, width, height, size } (ids come from Rust)
  prefs: loadPrefs(),
  scanning: 0,
  job: null,
  updating: false,
};

function loadPrefs() {
  const defaults = { mode: "original", maxWidth: 1920, webp: false };
  try {
    const saved = JSON.parse(localStorage.getItem(PREFS_KEY) || "{}");
    return {
      mode: saved.mode === "max" ? "max" : "original",
      maxWidth: L.parseMaxWidth(saved.maxWidth) ?? defaults.maxWidth,
      webp: Boolean(saved.webp),
    };
  } catch {
    return defaults;
  }
}

function savePrefs() {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(state.prefs));
  } catch {
    /* storage unavailable — preferences just won't persist */
  }
}

// ------------------------------------------------------------------ toast

let toastTimer;
function toast(message, ms = 3800) {
  const el = $("toast");
  el.textContent = message;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), ms);
}

function errorText(e) {
  return typeof e === "string" ? e : e?.message || "Something went wrong";
}

// ------------------------------------------------------------ setup view

const tplTile = $("tpl-tile").content.firstElementChild;
const grid = $("grid");

function renderSetup() {
  const n = state.images.length;
  $("drop-empty").hidden = n > 0;
  $("drop-bar").hidden = n === 0;
  $("files-head").hidden = n === 0;
  grid.hidden = n === 0;
  $("scanning").hidden = state.scanning === 0;
  if (n > 0) {
    $("count").textContent = L.plural(n, "image");
    $("total-size").textContent = L.formatBytes(state.images.reduce((s, i) => s + i.size, 0));
  }
  renderCompressButton();
}

function renderCompressButton() {
  const n = state.images.length;
  const widthOk = state.prefs.mode !== "max" || L.parseMaxWidth($("max-width").value) !== null;
  $("compress").disabled = n === 0 || !widthOk || state.scanning > 0;
  $("compress-label").textContent =
    n === 0 ? "Add images to start" : !widthOk ? "Enter a valid width" : `Compress ${L.plural(n, "image")}`;
}

function appendTiles(images) {
  const frag = document.createDocumentFragment();
  for (const img of images) {
    const tile = tplTile.cloneNode(true);
    tile.dataset.id = img.id;
    tile.title = `${img.name}\n${L.formatDims(img.width, img.height)} · ${L.formatBytes(img.size)}`;
    tile.querySelector(".thumb").classList.add(L.tileVariant(img.id - 1));
    tile.querySelector(".tile-name").textContent = img.name;
    frag.append(tile);
  }
  grid.append(frag);
}

/** Tell Rust to drop images the UI no longer shows. */
function forget(ids) {
  if (ids.length) api.invoke("forget_images", { ids }).catch(() => {});
}

function addScanned(result) {
  const incoming = result.images;
  const { merged, added, duplicates } = L.mergeImages(state.images, incoming);
  const addedImages = merged.slice(state.images.length);
  const kept = new Set(addedImages.map((i) => i.id));
  forget(incoming.filter((i) => !kept.has(i.id)).map((i) => i.id));
  state.images = merged;
  appendTiles(addedImages);
  renderSetup();
  const notes = [];
  if (result.skipped > 0) {
    const what = result.skipped === 1 ? "file that isn't" : "files that aren't";
    notes.push(`Skipped ${result.skipped} ${what} a JPG, PNG or WEBP image`);
  }
  if (duplicates > 0) notes.push(`${L.plural(duplicates, "image")} already added`);
  if (added === 0 && notes.length === 0) notes.push("No images found");
  if (notes.length) toast(notes.join(" · "));
}

async function scanWith(promise) {
  state.scanning++;
  renderSetup();
  try {
    const result = await promise;
    if (result) addScanned(result); // null: the file dialog was cancelled
  } catch (e) {
    toast(errorText(e));
  } finally {
    state.scanning--;
    renderSetup();
  }
}

function browse() {
  if (!$("setup").hidden) scanWith(api.invoke("pick_images"));
}

document.querySelectorAll("[data-browse]").forEach((b) => b.addEventListener("click", browse));

grid.addEventListener("click", (e) => {
  const btn = e.target.closest(".remove");
  if (!btn) return;
  const tile = btn.closest(".tile");
  const id = Number(tile.dataset.id);
  state.images = state.images.filter((i) => i.id !== id);
  forget([id]);
  tile.remove();
  renderSetup();
});

$("clear").addEventListener("click", () => {
  forget(state.images.map((i) => i.id));
  state.images = [];
  grid.replaceChildren();
  renderSetup();
});

// Options -------------------------------------------------------------------

function renderOptions() {
  const { mode, webp } = state.prefs;
  document.querySelector(`input[name="size"][value="${mode}"]`).checked = true;
  $("choice-original").classList.toggle("on", mode === "original");
  $("choice-max").classList.toggle("on", mode === "max");
  $("webp").checked = webp;
  $("choice-webp").classList.toggle("on", webp);
  const valid = L.parseMaxWidth($("max-width").value) !== null;
  $("max-width").parentElement.classList.toggle("invalid", mode === "max" && !valid);
  renderCompressButton();
}

document.querySelectorAll('input[name="size"]').forEach((r) =>
  r.addEventListener("change", () => {
    state.prefs.mode = r.value;
    savePrefs();
    renderOptions();
    if (r.value === "max") $("max-width").focus();
  }),
);
$("webp").addEventListener("change", (e) => {
  state.prefs.webp = e.target.checked;
  savePrefs();
  renderOptions();
});
$("max-width").addEventListener("input", (e) => {
  e.target.value = e.target.value.replace(/\D/g, "").slice(0, 4);
  const w = L.parseMaxWidth(e.target.value);
  if (w !== null) {
    state.prefs.maxWidth = w;
    savePrefs();
  }
  renderOptions();
});
$("max-width").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !$("compress").disabled) startBatch();
});

// Output folder ---------------------------------------------------------------

function renderOutput(out) {
  $("out-label").textContent = out.label;
  $("out-label").parentElement.parentElement.title = out.path;
}

$("change-dir").addEventListener("click", async () => {
  try {
    const out = await api.invoke("pick_output_dir");
    if (out) renderOutput(out);
  } catch (e) {
    toast(errorText(e));
  }
});

// -------------------------------------------------------------- run view

const tplRow = $("tpl-row").content.firstElementChild;
const list = $("list");

function showView(name) {
  $("setup").hidden = name !== "setup";
  $("run").hidden = name !== "run";
}

async function startBatch() {
  if (state.images.length === 0 || state.job?.running) return;
  const maxWidth = L.parseMaxWidth($("max-width").value);
  if (state.prefs.mode === "max" && maxWidth === null) return;
  const opts = L.buildOptions({ ...state.prefs, maxWidth });

  const items = state.images.map((img) => ({ ...img, status: "queued", progress: 0, outSize: 0 }));
  const job = {
    items,
    byId: new Map(items.map((i) => [i.id, i])),
    opts,
    running: true,
    stopping: false,
    stopped: false,
    startedAt: performance.now(),
    elapsedMs: 0,
    channel: null,
  };
  job.channel = api.channel((ev) => onEvent(job, ev));
  state.job = job;

  buildRows(job);
  showView("run");
  renderRunButtons();
  scheduleHero();
  job.ticker = setInterval(scheduleHero, 500);

  try {
    await api.invoke("start_batch", {
      ids: items.map((i) => i.id),
      options: opts,
      onEvent: job.channel,
    });
  } catch (e) {
    // Couldn't start (e.g. output folder not writable): back to the setup
    // screen with the selection intact.
    clearInterval(job.ticker);
    state.job = null;
    list.replaceChildren();
    showView("setup");
    toast(errorText(e), 6000);
  }
}

$("compress").addEventListener("click", startBatch);

function buildRows(job) {
  const frag = document.createDocumentFragment();
  for (const it of job.items) {
    const row = tplRow.cloneNode(true);
    row.dataset.id = it.id;
    row.querySelector(".thumb").classList.add(L.tileVariant(it.id - 1));
    row.querySelector(".row-name").textContent = it.name;
    row.querySelector(".row-name").title = it.path;
    it.el = {
      row,
      dims: row.querySelector(".row-dims"),
      size: row.querySelector(".row-size"),
      status: row.querySelector(".row-status"),
      rendered: null,
    };
    renderRow(it);
    frag.append(row);
  }
  list.replaceChildren(frag);
  list.scrollTop = 0;
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

function icon(name) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "ico");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#${name}`);
  svg.append(use);
  return svg;
}

function renderRow(it) {
  const { row, dims, size, status } = it.el;
  // Fast path: progress ticks only touch the bar.
  if (it.el.rendered === "working" && it.status === "working") {
    const pct = Math.round(it.progress * 100);
    it.el.fill.style.width = `${pct}%`;
    it.el.pct.textContent = `${pct}%`;
    return;
  }
  row.className = `row ${it.status}`;
  it.el.rendered = it.status;
  row.title = "";

  if (it.status === "done") {
    dims.textContent = L.dimsLabel(it.width, it.height, it.outWidth);
    size.textContent = `${L.formatBytes(it.size)} → ${L.formatBytes(it.outSize)}`;
  } else {
    dims.textContent = L.formatDims(it.width, it.height);
    size.textContent = L.formatBytes(it.size);
  }

  const parts = [];
  switch (it.status) {
    case "queued":
      parts.push(el("span", "status-muted", "Waiting"));
      break;
    case "working": {
      const bar = el("div", "mini-bar");
      const fill = el("div", "mini-fill");
      bar.append(fill);
      const pct = Math.round(it.progress * 100);
      fill.style.width = `${pct}%`;
      const label = el("span", "mini-pct", `${pct}%`);
      it.el.fill = fill;
      it.el.pct = label;
      parts.push(bar, label);
      break;
    }
    case "done": {
      // Converting to WebP can occasionally grow a file that was already
      // tiny; say so instead of pretending it was optimal.
      const saved = L.savingsPct(it.size, it.outSize);
      const grew = it.outSize > it.size;
      const label = saved > 0 ? `-${saved}%` : grew ? `+${L.growthPct(it.size, it.outSize)}%` : "Already optimal";
      const pill = el("span", saved > 0 ? "pill" : "pill same");
      pill.append(icon("i-check"), document.createTextNode(label));
      row.title = `${it.outName}\n${it.note}`;
      parts.push(pill);
      break;
    }
    case "failed": {
      parts.push(el("span", "status-failed", "Failed"));
      const retry = el("button", "btn-retry", "Retry");
      retry.type = "button";
      parts.push(retry);
      row.title = it.error || "";
      break;
    }
    case "cancelled":
      parts.push(el("span", "status-muted", "Skipped"));
      break;
  }
  status.replaceChildren(...parts);
}

function onEvent(job, ev) {
  if (ev.kind === "finished") {
    finishJob(job, ev);
    return;
  }
  const it = job.byId.get(ev.id);
  if (!it) return;
  switch (ev.kind) {
    case "started":
      it.status = "working";
      it.progress = 0;
      follow(it);
      break;
    case "progress":
      it.progress = Math.max(it.progress, ev.progress);
      break;
    case "done":
      Object.assign(it, {
        status: "done",
        progress: 1,
        outSize: ev.outSize,
        outName: ev.outName,
        outWidth: ev.width,
        outHeight: ev.height,
        note: ev.note,
      });
      break;
    case "failed":
      it.status = "failed";
      it.error = ev.error;
      break;
    case "cancelled":
      it.status = "cancelled";
      break;
  }
  renderRow(it);
  scheduleHero();
}

// Keep the rows being worked on in view, unless the user is scrolling.
let lastUserScroll = 0;
["wheel", "touchmove", "mousedown", "keydown"].forEach((t) =>
  list.addEventListener(t, () => (lastUserScroll = Date.now()), { passive: true }),
);
function follow(it) {
  if (Date.now() - lastUserScroll < 4000) return;
  it.el.row.scrollIntoView({ block: "nearest", behavior: "smooth" });
}

list.addEventListener("click", async (e) => {
  const btn = e.target.closest(".btn-retry");
  if (!btn || !state.job) return;
  const job = state.job;
  const it = job.byId.get(Number(btn.closest(".row").dataset.id));
  if (!it) return;
  it.status = "working";
  it.progress = 0;
  it.error = undefined;
  renderRow(it);
  scheduleHero();
  try {
    await api.invoke("retry_file", { id: it.id, options: job.opts, onEvent: job.channel });
  } catch (e) {
    it.status = "failed";
    it.error = errorText(e);
    renderRow(it);
    scheduleHero();
  }
});

function finishJob(job, ev) {
  job.running = false;
  job.stopped = job.stopping;
  job.elapsedMs = ev?.elapsedMs ?? performance.now() - job.startedAt;
  clearInterval(job.ticker);
  renderRunButtons();
  scheduleHero();
}

// Hero ----------------------------------------------------------------------

let heroQueued = false;
function scheduleHero() {
  if (heroQueued) return;
  heroQueued = true;
  requestAnimationFrame(() => {
    heroQueued = false;
    if (state.job) renderHero(state.job);
  });
}

function renderHero(job) {
  const total = job.items.length;
  const t = L.doneTotals(job.items);
  const busy = job.items.some((i) => i.status === "working");
  const live = job.running || busy;
  const elapsed = job.running ? performance.now() - job.startedAt : job.elapsedMs;
  const { fraction, etaSeconds } = L.batchProgress(job.items, elapsed);

  const phase = job.running ? (job.stopping ? "Stopping" : "Compressing") : job.stopped ? "Stopped" : "Done";
  $("hero-label").textContent = `${phase} · ${L.optionsLabel(job.opts)}`;

  let title;
  if (live) title = `${t.done + t.failed} of ${L.plural(total, "image")}`;
  else if (job.stopped) title = `${t.done} of ${total} compressed`;
  else if (t.failed === 0) title = `${L.plural(t.done, "image")} compressed`;
  else title = `${t.done} of ${total} compressed`;
  $("hero-title").textContent = title;

  // A stopped batch shows how much of the whole selection got processed.
  const shown = live ? fraction : job.stopped ? (t.done + t.failed) / total : 1;
  const pct = live ? Math.min(99, Math.floor(shown * 100)) : Math.floor(shown * 100);
  $("hero-pct").textContent = `${pct}%`;
  $("hero-bar").style.width = `${shown * 100}%`;

  let eta;
  if (live) eta = job.stopping ? "Finishing current images…" : L.formatEta(etaSeconds) || "Estimating time…";
  else {
    eta = `${job.stopped ? "Stopped after" : "Finished in"} ${L.formatDuration(elapsed)}`;
    if (t.failed > 0) eta = `${t.failed} failed · ${eta}`;
  }
  $("hero-eta").textContent = eta;

  const bytes = $("hero-bytes");
  if (t.done > 0) {
    bytes.replaceChildren(
      el("b", null, L.formatBytes(t.orig)),
      document.createTextNode(" → "),
      el("b", null, L.formatBytes(t.out)),
      document.createTextNode(live ? " so far" : " total"),
    );
    $("hero-saved").replaceChildren(el("b", null, `${L.savingsPct(t.orig, t.out)}%`), document.createTextNode(" smaller"));
  } else {
    bytes.textContent = live ? "Working on the first images…" : "";
    $("hero-saved").textContent = "";
  }
}

// Footer buttons ------------------------------------------------------------

function renderRunButtons() {
  const job = state.job;
  const stop = $("stop");
  if (job?.running) {
    stop.textContent = job.stopping ? "Stopping…" : "Stop";
    stop.disabled = job.stopping;
  } else {
    stop.textContent = "New batch";
    stop.disabled = false;
  }
}

$("stop").addEventListener("click", async () => {
  const job = state.job;
  if (!job) return;
  if (job.running) {
    job.stopping = true;
    renderRunButtons();
    scheduleHero();
    try {
      await api.invoke("stop_batch");
    } catch (e) {
      toast(errorText(e));
    }
    return;
  }
  newBatch();
});

function newBatch() {
  forget(state.images.map((i) => i.id));
  state.job = null;
  state.images = [];
  grid.replaceChildren();
  list.replaceChildren();
  showView("setup");
  renderSetup();
}

$("show-folder").addEventListener("click", () => api.invoke("open_output_dir").catch((e) => toast(errorText(e))));

// ---------------------------------------------------------- drag & drop
// The webview only shows the drop overlay; the dropped paths are handled by
// Rust (scanned there, never passed back through the UI).

const veil = $("drag-veil");
api.onDragDrop((p) => {
  const busy = state.job?.running;
  if (p.type === "enter" || p.type === "over") {
    if (!busy) veil.hidden = false;
  } else {
    veil.hidden = true;
  }
});
api.on("drop-ignored", () => toast("Hold on — drop more images when this batch has finished"));
api.on("scan-started", () => {
  if (state.job && !state.job.running) newBatch(); // dropping on a finished batch starts a new one
  state.scanning++;
  renderSetup();
});
api.on("scanned", (result) => {
  state.scanning = Math.max(0, state.scanning - 1);
  addScanned(result);
});

// -------------------------------------------------------------- updates

async function checkForUpdate(manual = false) {
  try {
    const info = await api.invoke("check_for_update");
    if (!info) {
      if (manual) toast(`You're on the latest version${state.version ? ` (${state.version})` : ""}`);
      return;
    }
    $("update-text").textContent = `Version ${info.version} is available`;
    $("update").title = info.notes || "";
    $("update").hidden = false;
  } catch (e) {
    // Offline or no release published yet — automatic checks retry later.
    if (manual) toast(`Couldn't check for updates: ${errorText(e)}`);
  }
}

$("update-btn").addEventListener("click", async () => {
  if (state.updating) return;
  if (state.job?.running) {
    toast("Finish or stop the current batch before updating");
    return;
  }
  state.updating = true;
  const btn = $("update-btn");
  const text = $("update-text");
  btn.disabled = true;
  text.textContent = "Downloading update…";
  const ch = api.channel((ev) => {
    if (ev.kind === "progress" && ev.total) {
      text.textContent = `Downloading update… ${Math.round((100 * ev.downloaded) / ev.total)}%`;
    } else if (ev.kind === "installing") {
      text.textContent = "Installing — SW3 Images will restart";
    }
  });
  try {
    await api.invoke("install_update", { onEvent: ch });
  } catch (e) {
    toast(`Update failed: ${errorText(e)}`);
    text.textContent = "Update available";
    btn.disabled = false;
    state.updating = false;
  }
});

// ------------------------------------------------------------------ init

// Native menu items (File → Add Images… ⌘O, Check for Updates…).
api.on("menu", (id) => {
  if (id === "add_images") browse();
  else if (id === "check_updates") checkForUpdate(true);
});
// Feels like a native app: no browser context menu outside text fields.
document.addEventListener("contextmenu", (e) => {
  if (!e.target.closest("input")) e.preventDefault();
});

$("max-width").value = String(state.prefs.maxWidth);
renderOptions();
renderSetup();

try {
  const info = await api.invoke("app_info");
  state.version = info.version;
  renderOutput(info.output);
  $("app-title").title = `SW3 Images ${info.version} · compressing ${info.workers} images at a time`;
} catch (e) {
  toast(errorText(e));
}

setTimeout(() => checkForUpdate(), 2500);
setInterval(() => checkForUpdate(), 6 * 60 * 60 * 1000);
